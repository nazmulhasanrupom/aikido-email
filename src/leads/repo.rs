//! Lead queries: read, change, delete (erase), lists and tags.
use super::email;
use super::events;
use super::fields::{FieldDefs, MAX_VALUE_CHARS, clean_custom, clean_tags};
use super::input::{MAX_NOTES_CHARS, clean_text, clean_timezone};
use super::model::*;
use super::query::{LeadFilter, push_where};
use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::{Map, Value, json};
use sqlx::{PgConnection, QueryBuilder};
use utoipa::ToSchema;
use uuid::Uuid;

pub async fn get(conn: &mut PgConnection, id: Uuid) -> Result<Option<Lead>, sqlx::Error> {
    sqlx::query_as(&format!("SELECT {LEAD_COLS} FROM leads WHERE id = $1")).bind(id).fetch_optional(conn).await
}

pub async fn by_email(conn: &mut PgConnection, email: &str) -> Result<Option<Lead>, sqlx::Error> {
    sqlx::query_as(&format!("SELECT {LEAD_COLS} FROM leads WHERE email = $1")).bind(email.trim().to_lowercase()).fetch_optional(conn).await
}

/// One page, newest first. `after` is the position of the cursor lead. Returns up to `limit + 1` rows.
pub async fn list(conn: &mut PgConnection, f: &LeadFilter, after: Option<(DateTime<Utc>, Uuid)>, limit: i64) -> Result<Vec<Lead>, sqlx::Error> {
    let mut qb = QueryBuilder::new(format!("SELECT {LEAD_COLS} FROM leads"));
    push_where(&mut qb, f);
    if let Some((created_at, id)) = after {
        qb.push(" AND (created_at, id) < (").push_bind(created_at).push(", ").push_bind(id).push(")");
    }
    qb.push(" ORDER BY created_at DESC, id DESC LIMIT ").push_bind(limit + 1);
    qb.build_query_as().fetch_all(conn).await
}

#[derive(Serialize, ToSchema, sqlx::FromRow, Clone, Debug)]
pub struct ListRef {
    pub id: Uuid,
    pub name: String,
}

#[derive(Serialize, ToSchema, sqlx::FromRow, Clone, Debug)]
pub struct CampaignRef {
    pub id: Uuid,
    pub name: String,
    /// The status of the campaign.
    pub status: String,
    /// What the campaign did with this lead: `pending`, `enrolled` or `skipped`.
    pub state: String,
    /// Why it was skipped.
    pub skip_reason: Option<String>,
}

#[derive(Serialize, ToSchema, Clone, Debug)]
pub struct LeadDetail {
    #[serde(flatten)]
    pub lead: Lead,
    /// The lists this lead is in.
    pub lists: Vec<ListRef>,
    /// The campaigns this lead is in.
    pub campaigns: Vec<CampaignRef>,
    /// The address or its domain is blocked in the suppression list. It will never be enrolled or sent to.
    pub suppressed: bool,
    /// The open enrollment (a sequence that is running for this address), if there is one.
    pub active_enrollment_id: Option<Uuid>,
}

pub async fn is_suppressed(conn: &mut PgConnection, lead: &Lead) -> Result<bool, sqlx::Error> {
    let (s,): (bool,) = sqlx::query_as(
        "SELECT EXISTS (SELECT 1 FROM suppressions WHERE kind = 'email' AND value = $1::citext)
             OR EXISTS (SELECT 1 FROM suppressions WHERE kind = 'domain' AND value = $2::citext)",
    )
    .bind(&lead.email)
    .bind(&lead.email_domain)
    .fetch_one(conn)
    .await?;
    Ok(s)
}

pub async fn open_enrollment(conn: &mut PgConnection, email: &str) -> Result<Option<Uuid>, sqlx::Error> {
    let row: Option<(Uuid,)> = sqlx::query_as(
        "SELECT id FROM enrollments WHERE to_email = $1::citext AND status IN ('active', 'waiting_send', 'paused') ORDER BY id DESC LIMIT 1",
    )
    .bind(email)
    .fetch_optional(conn)
    .await?;
    Ok(row.map(|r| r.0))
}

pub async fn detail(conn: &mut PgConnection, mut lead: Lead) -> Result<LeadDetail, sqlx::Error> {
    if let Some(b) = crate::verify_reoon::auto::briefs(&mut *conn, &[lead.id]).await?.remove(&lead.id) {
        (lead.verification_status, lead.verification_score, lead.verification_checked_at) = (Some(b.status), b.score, Some(b.checked_at));
    }
    if let Some((status, count)) = crate::tracking::stats::briefs(&mut *conn, &[lead.id]).await?.remove(&lead.id) {
        (lead.open_status, lead.open_count) = (Some(status), count);
    }
    let lists: Vec<ListRef> = sqlx::query_as(
        "SELECT l.id, l.name FROM lead_lists l JOIN lead_list_members m ON m.list_id = l.id WHERE m.lead_id = $1 ORDER BY l.name",
    )
    .bind(lead.id)
    .fetch_all(&mut *conn)
    .await?;
    let campaigns: Vec<CampaignRef> = sqlx::query_as(
        "SELECT c.id, c.name, c.status::text AS status, cl.state::text AS state, cl.skip_reason::text AS skip_reason
           FROM campaign_leads cl JOIN campaigns c ON c.id = cl.campaign_id WHERE cl.lead_id = $1 ORDER BY c.created_at DESC",
    )
    .bind(lead.id)
    .fetch_all(&mut *conn)
    .await?;
    let suppressed = is_suppressed(conn, &lead).await?;
    let active_enrollment_id = open_enrollment(conn, &lead.email).await?;
    Ok(LeadDetail { lead, lists, campaigns, suppressed, active_enrollment_id })
}

// ---------- change one lead ----------

#[derive(Debug)]
pub enum PatchError {
    NotFound,
    /// 422: the message names the field.
    Invalid(String),
    /// 409 `email_taken`
    EmailTaken,
    /// 409 `email_locked`: the lead has an open enrollment.
    EmailLocked,
    Db(sqlx::Error),
}

impl From<sqlx::Error> for PatchError {
    fn from(e: sqlx::Error) -> Self {
        Self::Db(e)
    }
}

fn is_unique_violation(e: &sqlx::Error) -> bool {
    e.as_database_error().is_some_and(|d| d.is_unique_violation())
}

/// Change a lead. `patch` is a JSON object: only the keys in it change. `null` or empty text clears a field.
/// `custom_fields` merges (a `null` value removes the key). `tags` replaces the list.
pub async fn patch(conn: &mut PgConnection, id: Uuid, patch: &Map<String, Value>, defs: &FieldDefs, now: DateTime<Utc>) -> Result<Lead, PatchError> {
    let mut lead: Lead = sqlx::query_as(&format!("SELECT {LEAD_COLS} FROM leads WHERE id = $1 FOR UPDATE"))
        .bind(id)
        .fetch_optional(&mut *conn)
        .await?
        .ok_or(PatchError::NotFound)?;
    let old_interest = lead.interest;
    let mut changed: Vec<&str> = Vec::new();

    for (key, value) in patch {
        let text = |max: usize| -> Result<Option<String>, PatchError> {
            match value {
                Value::Null => Ok(None),
                Value::String(s) => clean_text(key, Some(s), max).map_err(PatchError::Invalid),
                _ => Err(PatchError::Invalid(format!("{key} must be text or null"))),
            }
        };
        match key.as_str() {
            "first_name" => lead.first_name = text(MAX_VALUE_CHARS)?,
            "last_name" => lead.last_name = text(MAX_VALUE_CHARS)?,
            "company_name" => lead.company_name = text(MAX_VALUE_CHARS)?,
            "job_title" => lead.job_title = text(MAX_VALUE_CHARS)?,
            "phone" => lead.phone = text(MAX_VALUE_CHARS)?,
            "website" => lead.website = text(MAX_VALUE_CHARS)?,
            "linkedin_url" => lead.linkedin_url = text(MAX_VALUE_CHARS)?,
            "location" => lead.location = text(MAX_VALUE_CHARS)?,
            "industry" => lead.industry = text(MAX_VALUE_CHARS)?,
            "personalization_note" => lead.personalization_note = text(MAX_VALUE_CHARS)?,
            "trigger_event" => lead.trigger_event = text(MAX_VALUE_CHARS)?,
            "trigger_verified" => {
                lead.trigger_verified = match value {
                    Value::Bool(b) => *b,
                    Value::Null => false,
                    _ => return Err(PatchError::Invalid("trigger_verified must be true or false".into())),
                }
            }
            "notes" => lead.notes = text(MAX_NOTES_CHARS)?,
            "timezone" => {
                let raw = match value {
                    Value::Null => None,
                    Value::String(s) => Some(s.clone()),
                    _ => return Err(PatchError::Invalid("timezone must be text or null".into())),
                };
                lead.timezone = clean_timezone(raw.as_deref()).map_err(PatchError::Invalid)?;
            }
            "email" => {
                let Value::String(raw) = value else { return Err(PatchError::Invalid("email must be text".into())) };
                let new = email::normalize(raw).map_err(|e| PatchError::Invalid(e.to_string()))?;
                if new != lead.email {
                    if open_enrollment(conn, &lead.email).await?.is_some() {
                        return Err(PatchError::EmailLocked);
                    }
                    lead.email_domain = email::domain_of(&new);
                    lead.email = new;
                    lead.verification = Verification::SyntaxOk;
                }
            }
            "custom_fields" => {
                let Value::Object(m) = value else { return Err(PatchError::Invalid("custom_fields must be an object".into())) };
                let cleaned = clean_custom(defs, m, true).map_err(PatchError::Invalid)?;
                let mut current = lead.custom_fields.as_object().cloned().unwrap_or_default();
                for (k, v) in cleaned {
                    if v.is_null() {
                        current.remove(&k);
                    } else {
                        current.insert(k, v);
                    }
                }
                lead.custom_fields = Value::Object(current);
            }
            "tags" => {
                let tags: Vec<String> = serde_json::from_value(value.clone()).map_err(|_| PatchError::Invalid("tags must be a list of text".into()))?;
                lead.tags = clean_tags(&tags).map_err(PatchError::Invalid)?;
            }
            "interest" => {
                lead.interest = serde_json::from_value(value.clone()).map_err(|_| {
                    PatchError::Invalid("interest must be one of interested, meeting_booked, won, not_interested, wrong_person, lost, or null".into())
                })?;
            }
            other => return Err(PatchError::Invalid(format!("'{other}' can not be changed"))),
        }
        if key != "interest" {
            changed.push(key.as_str());
        }
    }

    let res = sqlx::query(
        "UPDATE leads SET email = $2, email_domain = $3, first_name = $4, last_name = $5, company_name = $6, job_title = $7, phone = $8,
                website = $9, linkedin_url = $10, location = $11, timezone = $12, custom_fields = $13, tags = $14, interest = $15,
                verification = $16, notes = $17, updated_at = $18, industry = $19, personalization_note = $20, trigger_event = $21,
                trigger_verified = $22 WHERE id = $1",
    )
    .bind(id)
    .bind(&lead.email)
    .bind(&lead.email_domain)
    .bind(&lead.first_name)
    .bind(&lead.last_name)
    .bind(&lead.company_name)
    .bind(&lead.job_title)
    .bind(&lead.phone)
    .bind(&lead.website)
    .bind(&lead.linkedin_url)
    .bind(&lead.location)
    .bind(&lead.timezone)
    .bind(&lead.custom_fields)
    .bind(&lead.tags)
    .bind(lead.interest)
    .bind(lead.verification)
    .bind(&lead.notes)
    .bind(now)
    .bind(&lead.industry)
    .bind(&lead.personalization_note)
    .bind(&lead.trigger_event)
    .bind(lead.trigger_verified)
    .execute(&mut *conn)
    .await;
    match res {
        Err(e) if is_unique_violation(&e) => return Err(PatchError::EmailTaken),
        Err(e) => return Err(e.into()),
        Ok(_) => {}
    }
    lead.updated_at = now;
    if !changed.is_empty() {
        events::log(conn, id, "updated", None, json!({ "fields": changed }), now).await?;
    }
    if lead.interest != old_interest {
        events::log(
            conn,
            id,
            "interest_changed",
            None,
            json!({ "from": old_interest.map(Interest::as_str), "to": lead.interest.map(Interest::as_str) }),
            now,
        )
        .await?;
    }
    Ok(lead)
}

// ---------- delete (erase) ----------

#[derive(Debug, Default, Serialize, ToSchema, PartialEq)]
pub struct EraseReport {
    pub deleted: i64,
    /// Open enrollments that were stopped.
    pub stopped_enrollments: i64,
    /// Queued emails that were cancelled.
    pub cancelled_jobs: i64,
}

/// Decision B. Stop the open enrollments (and cancel their queued emails), add each address to the
/// suppression list with reason `erased`, then delete the lead and its timeline. The address is never
/// contacted again, even if it is imported again. An address that is already suppressed keeps its reason.
pub async fn erase(conn: &mut PgConnection, ids: &[Uuid], now: DateTime<Utc>) -> Result<EraseReport, sqlx::Error> {
    if ids.is_empty() {
        return Ok(EraseReport::default());
    }
    let emails: Vec<(String,)> = sqlx::query_as("SELECT email FROM leads WHERE id = ANY($1) ORDER BY id FOR UPDATE").bind(ids).fetch_all(&mut *conn).await?;
    let emails: Vec<String> = emails.into_iter().map(|e| e.0).collect();
    let mut report = EraseReport::default();

    let open: Vec<(Uuid,)> = sqlx::query_as(
        "SELECT id FROM enrollments WHERE to_email = ANY($1::citext[]) AND status IN ('active', 'waiting_send', 'paused') ORDER BY id",
    )
    .bind(&emails)
    .fetch_all(&mut *conn)
    .await?;
    for (enrollment_id,) in open {
        // the row lock; the enrollment may have ended since the read above
        let Some(e) = crate::followups::lifecycle::lock(conn, enrollment_id).await? else { continue };
        if !matches!(e.status, crate::followups::model::EnrollmentStatus::Active | crate::followups::model::EnrollmentStatus::WaitingSend | crate::followups::model::EnrollmentStatus::Paused) {
            continue;
        }
        let cancelled = crate::followups::lifecycle::stop(conn, &e, "lead_deleted", json!({}), now).await?;
        report.stopped_enrollments += 1;
        report.cancelled_jobs += i64::from(cancelled.is_some());
    }

    sqlx::query(
        "INSERT INTO suppressions (value, kind, reason, created_at)
         SELECT e, 'email', 'erased', $2 FROM unnest($1::text[]) AS t(e)
         ON CONFLICT (value, kind) DO NOTHING",
    )
    .bind(&emails)
    .bind(now)
    .execute(&mut *conn)
    .await?;

    // open tracking: the tokens and the opens of these leads go too
    crate::tracking::stats::erase_leads(conn, ids).await?;
    // other tables hold the id without a foreign key: do not leave it dangling
    sqlx::query("UPDATE enrollments SET lead_id = NULL WHERE lead_id = ANY($1)").bind(ids).execute(&mut *conn).await?;
    sqlx::query("UPDATE send_jobs SET lead_id = NULL WHERE lead_id = ANY($1)").bind(ids).execute(&mut *conn).await?;
    sqlx::query("UPDATE inbound_messages SET lead_id = NULL WHERE lead_id = ANY($1)").bind(ids).execute(&mut *conn).await?;
    report.deleted = sqlx::query("DELETE FROM leads WHERE id = ANY($1)").bind(ids).execute(&mut *conn).await?.rows_affected() as i64;
    Ok(report)
}

// ---------- lists, tags, interest (bulk) ----------

/// Returns the ids that were newly added.
pub async fn add_to_list(conn: &mut PgConnection, list_id: Uuid, ids: &[Uuid], now: DateTime<Utc>) -> Result<Vec<Uuid>, sqlx::Error> {
    let added: Vec<(Uuid,)> = sqlx::query_as(
        "INSERT INTO lead_list_members (list_id, lead_id, added_at) SELECT $1, l, $3 FROM unnest($2::uuid[]) AS t(l)
         ON CONFLICT DO NOTHING RETURNING lead_id",
    )
    .bind(list_id)
    .bind(ids)
    .bind(now)
    .fetch_all(&mut *conn)
    .await?;
    let added: Vec<Uuid> = added.into_iter().map(|r| r.0).collect();
    events::log_many(conn, &added, "list_added", Some(list_id), json!({}), now).await?;
    Ok(added)
}

/// Returns the ids that were removed.
pub async fn remove_from_list(conn: &mut PgConnection, list_id: Uuid, ids: &[Uuid], now: DateTime<Utc>) -> Result<Vec<Uuid>, sqlx::Error> {
    let removed: Vec<(Uuid,)> = sqlx::query_as("DELETE FROM lead_list_members WHERE list_id = $1 AND lead_id = ANY($2) RETURNING lead_id")
        .bind(list_id)
        .bind(ids)
        .fetch_all(&mut *conn)
        .await?;
    let removed: Vec<Uuid> = removed.into_iter().map(|r| r.0).collect();
    events::log_many(conn, &removed, "list_removed", Some(list_id), json!({}), now).await?;
    Ok(removed)
}

/// Add tags. A lead that would pass 20 tags is left as it is. Returns the ids that changed.
pub async fn add_tags(conn: &mut PgConnection, ids: &[Uuid], tags: &[String], now: DateTime<Utc>) -> Result<Vec<Uuid>, sqlx::Error> {
    let union = "COALESCE((SELECT array_agg(t ORDER BY n) FROM (SELECT t, min(n) AS n FROM unnest(tags || $2::text[]) WITH ORDINALITY u(t, n) GROUP BY t) x), '{}'::text[])";
    let rows: Vec<(Uuid,)> = sqlx::query_as(&format!(
        "UPDATE leads SET tags = {union}, updated_at = $3
         WHERE id = ANY($1) AND NOT tags @> $2::text[] AND cardinality({union}) <= 20 RETURNING id"
    ))
    .bind(ids)
    .bind(tags)
    .bind(now)
    .fetch_all(&mut *conn)
    .await?;
    let changed: Vec<Uuid> = rows.into_iter().map(|r| r.0).collect();
    events::log_many(conn, &changed, "updated", None, json!({ "fields": ["tags"], "added": tags }), now).await?;
    Ok(changed)
}

pub async fn remove_tags(conn: &mut PgConnection, ids: &[Uuid], tags: &[String], now: DateTime<Utc>) -> Result<Vec<Uuid>, sqlx::Error> {
    let rows: Vec<(Uuid,)> = sqlx::query_as(
        "UPDATE leads SET tags = ARRAY(SELECT t FROM unnest(tags) WITH ORDINALITY u(t, n) WHERE NOT t = ANY($2::text[]) ORDER BY n), updated_at = $3
         WHERE id = ANY($1) AND tags && $2::text[] RETURNING id",
    )
    .bind(ids)
    .bind(tags)
    .bind(now)
    .fetch_all(&mut *conn)
    .await?;
    let changed: Vec<Uuid> = rows.into_iter().map(|r| r.0).collect();
    events::log_many(conn, &changed, "updated", None, json!({ "fields": ["tags"], "removed": tags }), now).await?;
    Ok(changed)
}

/// `None` clears the interest. Logs `interest_changed` with the old and the new value for each lead.
pub async fn set_interest(conn: &mut PgConnection, ids: &[Uuid], interest: Option<Interest>, now: DateTime<Utc>) -> Result<Vec<Uuid>, sqlx::Error> {
    let rows: Vec<(Uuid, Option<Interest>)> = sqlx::query_as(
        "UPDATE leads n SET interest = $2, updated_at = $3 FROM (SELECT id, interest FROM leads WHERE id = ANY($1) FOR UPDATE) o
         WHERE n.id = o.id AND n.interest IS DISTINCT FROM $2 RETURNING n.id, o.interest",
    )
    .bind(ids)
    .bind(interest)
    .bind(now)
    .fetch_all(&mut *conn)
    .await?;
    for (id, old) in &rows {
        events::log(conn, *id, "interest_changed", None, json!({ "from": old.map(Interest::as_str), "to": interest.map(Interest::as_str) }), now).await?;
    }
    Ok(rows.into_iter().map(|r| r.0).collect())
}

/// Put leads in a campaign as `pending` (the feeder enrolls them later). Only while the campaign is `draft`,
/// `active` or `paused`. A lead that is in the campaign already is left alone. Returns how many were added.
pub async fn add_to_campaign(conn: &mut PgConnection, campaign_id: Uuid, ids: &[Uuid], now: DateTime<Utc>) -> Result<i64, sqlx::Error> {
    let (n,): (i64,) = sqlx::query_as(
        "WITH ins AS (
            INSERT INTO campaign_leads (campaign_id, lead_id, added_at)
            SELECT $1, l, $3 FROM unnest($2::uuid[]) AS t(l)
             WHERE EXISTS (SELECT 1 FROM campaigns WHERE id = $1 AND status IN ('draft', 'active', 'paused'))
            ON CONFLICT DO NOTHING RETURNING lead_id
         ), ev AS (
            INSERT INTO lead_events (lead_id, kind, ref_id, detail, at) SELECT lead_id, 'campaign_added', $1, '{}'::jsonb, $3 FROM ins RETURNING 1
         ) SELECT count(*) FROM ev",
    )
    .bind(campaign_id)
    .bind(ids)
    .bind(now)
    .fetch_one(conn)
    .await?;
    Ok(n)
}

/// `None`: no such campaign. `Some(false)`: it does not take leads (completed or archived).
pub async fn campaign_takes_leads(conn: &mut PgConnection, campaign_id: Uuid) -> Result<Option<bool>, sqlx::Error> {
    let row: Option<(bool,)> = sqlx::query_as("SELECT status IN ('draft', 'active', 'paused') FROM campaigns WHERE id = $1").bind(campaign_id).fetch_optional(conn).await?;
    Ok(row.map(|r| r.0))
}
