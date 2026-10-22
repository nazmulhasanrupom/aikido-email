//! `upsert_batch`: the one function that writes leads in bulk. The JSON bulk route and the CSV import
//! call it today. Auto Database calls it later, with source `auto_database`.
use super::input::LeadInput;
use super::model::{DedupeMode, LeadSource};
use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use sqlx::PgConnection;
use std::collections::HashMap;
use uuid::Uuid;

pub struct UpsertCtx<'a> {
    pub mode: DedupeMode,
    pub source: LeadSource,
    /// Saved on new leads. Example: the import id.
    pub source_ref: Option<&'a str>,
    /// The job or import this write is about. Goes on the timeline events.
    pub ref_id: Option<Uuid>,
    pub now: DateTime<Utc>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Outcome {
    /// A new lead.
    Created,
    /// The lead existed and something changed.
    Updated,
    /// The lead existed and nothing changed (mode `skip`, nothing new to add, or a repeat in the same batch).
    Skipped,
}

#[derive(Clone, Copy, Debug)]
pub struct Upserted {
    pub id: Uuid,
    pub outcome: Outcome,
    /// The same address came earlier in this batch. The first row won.
    pub repeat_in_batch: bool,
}

const TEXT_COLS: [&str; 13] = [
    "first_name", "last_name", "company_name", "job_title", "phone", "website", "linkedin_url", "location", "timezone", "notes", "industry",
    "personalization_note", "trigger_event",
];
/// First parameter of the time. The ids, emails and domains take $1 to $3, the text columns the next ones.
const TIME_PARAM: usize = 4 + TEXT_COLS.len();

fn text_of(l: &LeadInput, col: &str) -> Option<String> {
    match col {
        "first_name" => l.first_name.clone(),
        "last_name" => l.last_name.clone(),
        "company_name" => l.company_name.clone(),
        "job_title" => l.job_title.clone(),
        "phone" => l.phone.clone(),
        "website" => l.website.clone(),
        "linkedin_url" => l.linkedin_url.clone(),
        "location" => l.location.clone(),
        "timezone" => l.timezone.clone(),
        "notes" => l.notes.clone(),
        "industry" => l.industry.clone(),
        "personalization_note" => l.personalization_note.clone(),
        "trigger_event" => l.trigger_event.clone(),
        _ => None,
    }
}

/// The SQL of the conflict branch for `fill_empty` and `overwrite`. The "new value" expressions are written
/// once and used twice: in `SET`, and in `WHERE` (so a row that would not change is not written).
fn conflict_sql(mode: DedupeMode) -> String {
    let mut new_values: Vec<(String, String)> = Vec::new();
    for c in TEXT_COLS {
        let expr = match mode {
            DedupeMode::Overwrite => format!("COALESCE(excluded.{c}, leads.{c})"),
            _ => format!("COALESCE(leads.{c}, excluded.{c})"),
        };
        new_values.push((c.to_string(), expr));
    }
    let custom = match mode {
        DedupeMode::Overwrite => "leads.custom_fields || excluded.custom_fields",
        _ => "excluded.custom_fields || leads.custom_fields",
    };
    new_values.push(("custom_fields".into(), custom.into()));
    // an import can set the flag, never clear it
    new_values.push(("trigger_verified".into(), "(leads.trigger_verified OR excluded.trigger_verified)".into()));
    // union, first seen order kept. Over 20 tags: the old tags stay.
    let union = "COALESCE((SELECT array_agg(t ORDER BY n) FROM (SELECT t, min(n) AS n FROM unnest(leads.tags || excluded.tags) WITH ORDINALITY u(t, n) GROUP BY t) x), '{}'::text[])";
    new_values.push(("tags".into(), format!("CASE WHEN cardinality({union}) > 20 THEN leads.tags ELSE {union} END")));

    let set: Vec<String> = new_values.iter().map(|(c, e)| format!("{c} = {e}")).collect();
    let old: Vec<String> = new_values.iter().map(|(c, _)| format!("leads.{c}")).collect();
    let new: Vec<&str> = new_values.iter().map(|(_, e)| e.as_str()).collect();
    format!(
        "ON CONFLICT (email) DO UPDATE SET {}, updated_at = ${} WHERE ({}) IS DISTINCT FROM ({}) RETURNING email, (xmax = 0) AS inserted",
        set.join(", "),
        TIME_PARAM,
        old.join(", "),
        new.join(", ")
    )
}

/// Write the leads. `rows` are already checked (`RawLead::check`). The result has one entry per input row,
/// in the same order. A repeat of an address in the same batch counts as skipped: the first row wins.
///
/// Mode `skip`: an existing lead is not touched. `fill_empty`: only empty fields of an existing lead are
/// filled. `overwrite`: every field that has a value in the row replaces the old one. An empty value never
/// erases data. Tags are added, never removed.
///
/// New leads start with `verification = syntax_ok` (the syntax check already passed).
/// Call it inside a transaction, so the rows and their events are saved together.
pub async fn upsert_batch(conn: &mut PgConnection, rows: &[LeadInput], ctx: &UpsertCtx<'_>) -> Result<Vec<Upserted>, sqlx::Error> {
    // first row wins
    let mut first: HashMap<&str, usize> = HashMap::new();
    let mut unique: Vec<&LeadInput> = Vec::new();
    for r in rows {
        first.entry(r.email.as_str()).or_insert_with(|| {
            unique.push(r);
            unique.len() - 1
        });
    }
    if unique.is_empty() {
        return Ok(Vec::new());
    }
    // the same row order in every batch: two batches that share addresses lock the rows in the same order
    unique.sort_by(|a, b| a.email.cmp(&b.email));

    let ids: Vec<Uuid> = unique.iter().map(|_| Uuid::now_v7()).collect();
    let emails: Vec<String> = unique.iter().map(|l| l.email.clone()).collect();
    let domains: Vec<String> = unique.iter().map(|l| l.domain.clone()).collect();
    let text: Vec<Vec<Option<String>>> = TEXT_COLS.iter().map(|c| unique.iter().map(|l| text_of(l, c)).collect()).collect();
    let custom: Vec<Value> = unique.iter().map(|l| Value::Object(l.custom_fields.clone())).collect();
    let tags: Vec<Value> = unique.iter().map(|l| json!(l.tags)).collect();
    let verified: Vec<Option<bool>> = unique.iter().map(|l| l.trigger_verified).collect();

    let mode_sql = match ctx.mode {
        DedupeMode::Skip => "ON CONFLICT (email) DO NOTHING RETURNING email, true AS inserted".to_string(),
        m => conflict_sql(m),
    };
    // $1 ids, $2 emails, $3 domains, then the text columns, then time, custom, tags, source, source_ref, trigger_verified
    let (t, c, g, src, srcref, tv) = (TIME_PARAM, TIME_PARAM + 1, TIME_PARAM + 2, TIME_PARAM + 3, TIME_PARAM + 4, TIME_PARAM + 5);
    let text_params: Vec<String> = (0..TEXT_COLS.len()).map(|i| format!("${}::text[]", 4 + i)).collect();
    let cols = TEXT_COLS.join(", ");
    let sql = format!(
        "WITH incoming AS (
            SELECT * FROM unnest($1::uuid[], $2::text[], $3::text[], {}, ${c}::jsonb[], ${g}::jsonb[], ${tv}::boolean[])
              AS t(id, email, email_domain, {cols}, custom_fields, tags, trigger_verified)
         )
         INSERT INTO leads (id, email, email_domain, {cols}, custom_fields, tags, trigger_verified, verification, source, source_ref, created_at, updated_at)
         SELECT id, email, email_domain, {cols}, custom_fields, ARRAY(SELECT jsonb_array_elements_text(tags)), COALESCE(trigger_verified, false),
                'syntax_ok', ${src}, ${srcref}, ${t}, ${t}
           FROM incoming
         {mode_sql}",
        text_params.join(", ")
    );
    let mut q = sqlx::query_as::<_, (String, bool)>(&sql).bind(&ids).bind(&emails).bind(&domains);
    for col in &text {
        q = q.bind(col);
    }
    let returned: Vec<(String, bool)> = q.bind(ctx.now).bind(&custom).bind(&tags).bind(ctx.source).bind(ctx.source_ref).bind(&verified).fetch_all(&mut *conn).await?;
    let changed: HashMap<String, bool> = returned.into_iter().collect();

    // ids of every address, new and old
    let found: Vec<(Uuid, String)> = sqlx::query_as("SELECT id, email FROM leads WHERE email = ANY($1)").bind(&emails).fetch_all(&mut *conn).await?;
    let id_of: HashMap<String, Uuid> = found.into_iter().map(|(id, e)| (e, id)).collect();

    // the timeline
    let mut ev_leads: Vec<Uuid> = Vec::new();
    let mut ev_kinds: Vec<String> = Vec::new();
    let mut outcomes: HashMap<&str, Outcome> = HashMap::new();
    for l in &unique {
        // RETURNING lists the rows that were written, with `inserted`
        let outcome = match changed.get(&l.email) {
            Some(true) => Outcome::Created,
            Some(false) => Outcome::Updated,
            None => Outcome::Skipped,
        };
        outcomes.insert(l.email.as_str(), outcome);
        let Some(id) = id_of.get(&l.email) else { continue };
        match outcome {
            Outcome::Created => {
                ev_leads.push(*id);
                ev_kinds.push("created".into());
            }
            Outcome::Updated => {
                ev_leads.push(*id);
                ev_kinds.push(if ctx.source == LeadSource::Csv { "imported" } else { "updated" }.into());
            }
            Outcome::Skipped => {}
        }
    }
    if !ev_leads.is_empty() {
        sqlx::query(
            "INSERT INTO lead_events (lead_id, kind, ref_id, detail, at)
             SELECT l, k, $3, $4, $5 FROM unnest($1::uuid[], $2::text[]) AS t(l, k)",
        )
        .bind(&ev_leads)
        .bind(&ev_kinds)
        .bind(ctx.ref_id)
        .bind(json!({ "source": ctx.source.as_str(), "source_ref": ctx.source_ref, "mode": ctx.mode.as_str() }))
        .bind(ctx.now)
        .execute(&mut *conn)
        .await?;
    }

    let mut seen: HashMap<&str, bool> = HashMap::new();
    let mut out = Vec::with_capacity(rows.len());
    for r in rows {
        let repeat = seen.insert(r.email.as_str(), true).is_some();
        let id = id_of.get(&r.email).copied().unwrap_or_default();
        let outcome = if repeat { Outcome::Skipped } else { outcomes.get(r.email.as_str()).copied().unwrap_or(Outcome::Skipped) };
        out.push(Upserted { id, outcome, repeat_in_batch: repeat });
    }
    // verify on import: the leads of the CSV import, the API and a manual add (not the ones an enrollment makes)
    if ctx.source != LeadSource::Enrollment {
        // every lead of the batch, not only the new ones: a lead that was there before may have no result yet
        let touched: Vec<Uuid> = out.iter().filter(|u| !u.repeat_in_batch && !u.id.is_nil()).map(|u| u.id).collect();
        crate::verify_reoon::auto::enqueue_new(conn, &touched, ctx.now).await?;
    }
    // saved campaign emails of the updated leads may now be out of date
    let updated: Vec<Uuid> = out.iter().filter(|u| u.outcome == Outcome::Updated).map(|u| u.id).collect();
    crate::campaigns::emails::leads_changed(conn, &updated).await?;
    Ok(out)
}
