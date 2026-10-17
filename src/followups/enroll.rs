//! Enroll recipients into a sequence. Bad items get an error each. Good items are still enrolled.
//! Every step is rendered for every item first, so a missing variable fails here and not at step 3.
use super::lifecycle;
use super::model::*;
use super::repo;
use super::template::{StepTemplates, build_vars, sender_name_of};
use crate::leads::fields::FieldDefs;
use crate::leads::hooks;
use crate::leads::input::LeadInput;
use crate::leads::model::{DedupeMode, LEAD_COLS, Lead, LeadSource};
use crate::leads::upsert::{UpsertCtx, upsert_batch};
use crate::leads::vars::{enroll_snapshot, render_variables};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::PgPool;
use std::collections::HashMap;
use utoipa::ToSchema;
use uuid::Uuid;

#[derive(Clone, Debug, Deserialize, ToSchema)]
pub struct NewEnrollment {
    /// The address. Give this or `lead_id`. With only an address, the lead is found by email, or made (source `enrollment`).
    pub to_email: Option<String>,
    /// The one account that sends every step.
    pub account_id: Uuid,
    /// Values for `{{name}}` in the steps: text, number or true/false. Built-in names: `email`, `sender_name`, `sender_email`.
    #[schema(value_type = Option<Object>)]
    pub variables: Option<Value>,
    /// A repeat key returns the old enrollment, not a new one. 1 to 200 characters.
    pub idempotency_key: Option<String>,
    /// Step 1 goes out not before this time. Default: now.
    pub start_at: Option<DateTime<Utc>>,
    /// The lead to enroll. The address comes from the lead, and the lead's fields fill the steps
    /// (the values in `variables` are used where the lead has none). Give this or `to_email`.
    pub lead_id: Option<Uuid>,
}

#[derive(Debug, Serialize, ToSchema, Clone, PartialEq)]
pub struct ItemError {
    pub code: String,
    pub message: String,
}

fn item_error(code: &str, message: impl Into<String>) -> ItemError {
    ItemError { code: code.into(), message: message.into() }
}

#[derive(Serialize, ToSchema, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum EnrollStatus {
    Enrolled,
    Duplicate,
    Error,
}

#[derive(Serialize, ToSchema)]
pub struct EnrollItem {
    pub index: usize,
    pub status: EnrollStatus,
    pub enrollment: Option<Enrollment>,
    pub error: Option<ItemError>,
}

fn error_item(index: usize, error: ItemError) -> EnrollItem {
    EnrollItem { index, status: EnrollStatus::Error, enrollment: None, error: Some(error) }
}

/// Parse the texts of every step once.
pub fn parse_steps(steps: &[SequenceStep]) -> Result<Vec<(i32, StepTemplates)>, String> {
    steps
        .iter()
        .map(|s| {
            StepTemplates::parse(&s.subject, s.body_text.as_deref(), s.body_html.as_deref())
                .map(|t| (s.step_number, t))
                .map_err(|e| format!("step {}: {}", s.step_number, e.message))
        })
        .collect()
}

#[derive(Debug)]
pub enum EnrollError {
    /// Nothing was enrolled: these items have a step with a variable that has no value and no fallback.
    MissingVariables(Vec<String>),
    Db(sqlx::Error),
}

impl From<sqlx::Error> for EnrollError {
    fn from(e: sqlx::Error) -> Self {
        EnrollError::Db(e)
    }
}

/// The error text for one item whose values do not fill every step. `None` when they do (or cannot be checked here).
fn missing_for(templates: &[(i32, StepTemplates)], variables: &Value, email: &str, account: &(String, String)) -> Option<String> {
    let vars = build_vars(variables, email, &sender_name_of(&account.1, &account.0), &account.0).ok()?;
    let missing: Vec<String> = templates
        .iter()
        .filter_map(|(n, t)| {
            // An AI slot (`ai:key`) is filled by the Email Generator at due time, or by its fallback: it is not a missing value.
            let m: Vec<String> = t.render(&vars).err()?.into_iter().filter(|name| !name.starts_with("ai:")).collect();
            (!m.is_empty()).then(|| format!("step {n} needs: {}", m.join(", ")))
        })
        .collect();
    (!missing.is_empty()).then(|| missing.join("; "))
}


fn norm(email: &str) -> String {
    email.trim().to_lowercase()
}

/// The address of an item: from the lead, or from `to_email`. A lead and an address must agree.
fn target_email(item: &NewEnrollment, by_id: &HashMap<Uuid, Lead>) -> Result<String, ItemError> {
    match (item.lead_id, item.to_email.as_deref()) {
        (Some(id), to) => {
            let lead = by_id.get(&id).ok_or_else(|| item_error("lead_not_found", "No lead with this id."))?;
            if to.is_some_and(|t| norm(t) != lead.email) {
                return Err(item_error("lead_email_mismatch", "to_email is not the email of this lead. Give one of them."));
            }
            Ok(lead.email.clone())
        }
        (None, Some(to)) => Ok(norm(to)),
        (None, None) => Err(item_error("invalid_to_email", "Give to_email or lead_id.")),
    }
}

/// Enroll the items. A step with a variable that has no value and no fallback blocks the whole call (nothing is
/// enrolled), so a broken email never fails at step 3. Every other problem is an error on its own item.
pub async fn enroll(pool: &PgPool, sequence_id: Uuid, items: Vec<NewEnrollment>, now: DateTime<Utc>) -> Result<Vec<EnrollItem>, EnrollError> {
    let mut conn = pool.acquire().await?;
    let steps = repo::get_steps(&mut conn, sequence_id).await?;
    let settings = repo::load_settings(&mut conn).await?;
    drop(conn);
    // The texts were checked when the steps were saved. If this fails, the data is damaged.
    let templates = parse_steps(&steps).unwrap_or_default();

    let account_ids: Vec<Uuid> = items.iter().map(|i| i.account_id).collect();
    let accounts: Vec<(Uuid, String, String)> =
        sqlx::query_as("SELECT id, email::text, display_name FROM email_accounts WHERE id = ANY($1) AND deleted_at IS NULL")
            .bind(&account_ids)
            .fetch_all(pool)
            .await?;
    let accounts: HashMap<Uuid, (String, String)> = accounts.into_iter().map(|(id, e, n)| (id, (e, n))).collect();

    // The leads these items name: by id or by address. Their fields fill the steps.
    let lead_ids: Vec<Uuid> = items.iter().filter_map(|i| i.lead_id).collect();
    let emails: Vec<String> = items.iter().filter_map(|i| i.to_email.as_deref()).map(norm).collect();
    let known: Vec<Lead> = sqlx::query_as(&format!("SELECT {LEAD_COLS} FROM leads WHERE id = ANY($1) OR email = ANY($2)"))
        .bind(&lead_ids)
        .bind(&emails)
        .fetch_all(pool)
        .await?;
    let by_id: HashMap<Uuid, Lead> = known.iter().map(|l| (l.id, l.clone())).collect();
    let by_email: HashMap<String, Lead> = known.into_iter().map(|l| (l.email.clone(), l)).collect();
    let defs = {
        let mut conn = pool.acquire().await?;
        FieldDefs::load(&mut conn).await?
    };

    let blocked: Vec<String> = items
        .iter()
        .enumerate()
        .filter_map(|(i, item)| {
            let account = accounts.get(&item.account_id)?;
            let email = target_email(item, &by_id).ok()?;
            let lead = item.lead_id.and_then(|id| by_id.get(&id)).or_else(|| by_email.get(&email));
            let variables = render_variables(&enroll_snapshot(&item.variables.clone().unwrap_or_else(|| json!({})), lead), lead, &defs);
            missing_for(&templates, &variables, &email, account).map(|m| format!("item {i}: {m}"))
        })
        .collect();
    if !blocked.is_empty() {
        return Err(EnrollError::MissingVariables(blocked));
    }

    let mut out: Vec<EnrollItem> = Vec::with_capacity(items.len());
    let mut tx = pool.begin().await?;
    for (index, item) in items.into_iter().enumerate() {
        // 1. The address.
        let email = match target_email(&item, &by_id) {
            Ok(e) => e,
            Err(err) => {
                out.push(error_item(index, err));
                continue;
            }
        };
        if email.parse::<lettre::Address>().is_err() {
            out.push(error_item(index, item_error("invalid_to_email", format!("'{}' is not a valid email address.", item.to_email.as_deref().unwrap_or(&email)))));
            continue;
        }
        if item.idempotency_key.as_deref().is_some_and(|k| k.is_empty() || k.len() > 200) {
            out.push(error_item(index, item_error("invalid_idempotency_key", "idempotency_key must be 1 to 200 characters.")));
            continue;
        }
        // 2. A repeat key returns the old enrollment (before any other check: a retry must not fail).
        if let Some(key) = &item.idempotency_key {
            let old: Option<Enrollment> =
                sqlx::query_as(&format!("SELECT {ENROLLMENT_COLS} FROM enrollments WHERE idempotency_key = $1")).bind(key).fetch_optional(&mut *tx).await?;
            if let Some(old) = old {
                out.push(EnrollItem { index, status: EnrollStatus::Duplicate, enrollment: Some(old), error: None });
                continue;
            }
        }
        // 3. The account.
        let Some((account_email, display_name)) = accounts.get(&item.account_id) else {
            out.push(error_item(index, item_error("account_not_found", "No account with this id.")));
            continue;
        };
        // 4. The values must be well formed (the missing ones were checked before the loop).
        let explicit = item.variables.clone().unwrap_or_else(|| json!({}));
        if explicit.as_object().is_some_and(|m| m.len() > 50) {
            out.push(error_item(index, item_error("invalid_variables", "variables can have at most 50 entries.")));
            continue;
        }
        if let Err(msg) = build_vars(&explicit, &email, &sender_name_of(display_name, account_email), account_email) {
            out.push(error_item(index, item_error("invalid_variables", msg)));
            continue;
        }
        // 5. A suppressed address never gets mail.
        if let Some(reason) = repo::suppression_of(&mut tx, &email).await? {
            out.push(error_item(index, item_error("suppressed", format!("This address is suppressed ({reason})."))));
            continue;
        }
        // 6. One open enrollment per address. The lock makes two calls at the same moment take turns.
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 7306003))").bind(&email).execute(&mut *tx).await?;
        if settings.one_active_per_email {
            let open: Option<(Uuid,)> = sqlx::query_as(
                "SELECT id FROM enrollments WHERE to_email = $1::citext AND status IN ('active', 'waiting_send', 'paused') LIMIT 1",
            )
            .bind(&email)
            .fetch_optional(&mut *tx)
            .await?;
            if let Some((other,)) = open {
                out.push(error_item(index, item_error("already_enrolled", format!("This address is in an open enrollment already ({other})."))));
                continue;
            }
        }
        // 7. The lead. Every contacted address is a lead: find it, or make it.
        let lead_id = match item.lead_id.or_else(|| by_email.get(&email).map(|l| l.id)) {
            Some(id) => id,
            None => {
                let input = LeadInput { domain: crate::leads::email::domain_of(&email), email: email.clone(), ..Default::default() };
                let ctx = UpsertCtx { mode: DedupeMode::Skip, source: LeadSource::Enrollment, source_ref: None, ref_id: None, now };
                upsert_batch(&mut tx, std::slice::from_ref(&input), &ctx).await?.first().map(|r| r.id).unwrap_or_default()
            }
        };
        let lead_row = by_id.get(&lead_id).or_else(|| by_email.get(&email));
        let variables = enroll_snapshot(&explicit, lead_row);
        // 8. Write it.
        let id = Uuid::now_v7();
        let inserted: Option<Enrollment> = sqlx::query_as(&format!(
            "INSERT INTO enrollments (id, sequence_id, account_id, to_email, variables, status, next_step, next_due_at,
                idempotency_key, lead_id, created_at, updated_at)
             VALUES ($1, $2, $3, $4::citext, $5, 'active', 1, $6, $7, $8, $9, $9)
             ON CONFLICT (idempotency_key) DO NOTHING
             RETURNING {ENROLLMENT_COLS}"
        ))
        .bind(id)
        .bind(sequence_id)
        .bind(item.account_id)
        .bind(&email)
        .bind(&variables)
        .bind(item.start_at.unwrap_or(now))
        .bind(&item.idempotency_key)
        .bind(lead_id)
        .bind(now)
        .fetch_optional(&mut *tx)
        .await?;
        match inserted {
            Some(e) => {
                lifecycle::log(&mut tx, e.id, "enrolled", Some(1), json!({ "start_at": e.next_due_at }), now).await?;
                hooks::on_enrolled(&mut tx, lead_id, e.id, now).await;
                out.push(EnrollItem { index, status: EnrollStatus::Enrolled, enrollment: Some(e), error: None });
            }
            None => {
                let old: Enrollment = sqlx::query_as(&format!("SELECT {ENROLLMENT_COLS} FROM enrollments WHERE idempotency_key = $1"))
                    .bind(&item.idempotency_key)
                    .fetch_one(&mut *tx)
                    .await?;
                out.push(EnrollItem { index, status: EnrollStatus::Duplicate, enrollment: Some(old), error: None });
            }
        }
    }
    tx.commit().await?;
    Ok(out)
}
