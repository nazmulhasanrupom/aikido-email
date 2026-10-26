//! Event triggers (build map 9.3): read new events with a cursor, filter them, and build the trigger item.
//!
//! The other engines already write their events in the same transaction as their work, so the triggers need
//! no change in them. Four tables are read:
//! - `lead_events` (bigserial id): created, campaign_added, list_added, updated, interest_changed, step_sent, bounced, unsubscribed, complained
//! - `inbound_messages` (stored replies): read in order of `(created_at, id)`
//! - `campaign_events` (status changes, written since migration 0027)
//! - `lead_imports` (finished imports): read in order of `(finished_at, id)`
//!
//! A cursor is `{ "id": 42 }` for the first and third table, and `{ "at": <time>, "id": <uuid> }` for the others.
//! Reading stops `settle` before now, so a transaction that is still open is not skipped.
use crate::state::AppState;
use chrono::{DateTime, Utc};
use serde_json::{Map, Value, json};
use sqlx::{FromRow, PgConnection};
use uuid::Uuid;

/// Events this young are not read yet (seconds). A row that is written by a slow transaction shows up late.
pub const SETTLE_SECS: f64 = 2.0;
/// Events read per trigger per tick.
pub const BATCH: i64 = 200;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Source {
    Lead,
    Replies,
    Campaigns,
    Imports,
}

pub fn source_of(type_id: &str) -> Option<Source> {
    Some(match type_id {
        "trigger.lead_created"
        | "trigger.lead_added_to_campaign"
        | "trigger.lead_added_to_list"
        | "trigger.lead_updated"
        | "trigger.lead_interest_changed"
        | "trigger.email_sent"
        | "trigger.lead_bounced"
        | "trigger.lead_unsubscribed" => Source::Lead,
        "trigger.reply_received" => Source::Replies,
        "trigger.campaign_status_changed" => Source::Campaigns,
        "trigger.import_finished" => Source::Imports,
        _ => return None,
    })
}

/// The `lead_events.kind` values a lead trigger reads.
pub fn lead_kinds(type_id: &str) -> &'static [&'static str] {
    match type_id {
        "trigger.lead_created" => &["created"],
        "trigger.lead_added_to_campaign" => &["campaign_added"],
        "trigger.lead_added_to_list" => &["list_added"],
        "trigger.lead_updated" => &["updated"],
        "trigger.lead_interest_changed" => &["interest_changed"],
        "trigger.email_sent" => &["step_sent"],
        "trigger.lead_bounced" => &["bounced"],
        "trigger.lead_unsubscribed" => &["unsubscribed", "complained"],
        _ => &[],
    }
}

/// The event types shown in `automation_triggers.event_types`.
pub fn event_types(type_id: &str) -> Vec<String> {
    match source_of(type_id) {
        Some(Source::Lead) => lead_kinds(type_id).iter().map(|s| s.to_string()).collect(),
        Some(Source::Replies) => vec!["inbound_message".into()],
        Some(Source::Campaigns) => vec!["campaign_status".into()],
        Some(Source::Imports) => vec!["import_finished".into()],
        None => vec![],
    }
}

/// One event as read from its table, before the filter.
#[derive(Debug, Clone)]
pub struct RawEvent {
    /// The id in the source table, as text. It goes in the idempotency key of the run.
    pub key: String,
    /// `campaign_added`, `inbound_message`, ...
    pub kind: String,
    pub at: DateTime<Utc>,
    pub lead_id: Option<Uuid>,
    pub ref_id: Option<Uuid>,
    pub detail: Value,
    /// Source specific fields (the message, the import, the campaign statuses).
    pub extra: Value,
}

pub struct Batch {
    pub events: Vec<RawEvent>,
    /// Where to read from next time.
    pub cursor: Value,
}

// ---------- cursors ----------

fn cursor_id(c: &Value) -> i64 {
    c.get("id").and_then(Value::as_i64).unwrap_or(0)
}

fn cursor_pair(c: &Value) -> (DateTime<Utc>, Uuid) {
    let at = c.get("at").and_then(Value::as_str).and_then(|s| s.parse().ok()).unwrap_or(DateTime::<Utc>::UNIX_EPOCH);
    let id = c.get("id").and_then(Value::as_str).and_then(|s| s.parse().ok()).unwrap_or_else(Uuid::nil);
    (at, id)
}

fn pair_cursor(at: DateTime<Utc>, id: Uuid) -> Value {
    json!({ "at": at, "id": id })
}

/// Where a trigger starts when it is switched on: after the newest event now, or `backfill_hours` back.
pub async fn initial_cursor(conn: &mut PgConnection, source: Source, now: DateTime<Utc>, backfill_hours: i64) -> Result<Value, sqlx::Error> {
    let since = now - chrono::Duration::hours(backfill_hours.max(0));
    Ok(match source {
        Source::Lead | Source::Campaigns => {
            let table = if source == Source::Lead { "lead_events" } else { "campaign_events" };
            let id: i64 = if backfill_hours > 0 {
                sqlx::query_scalar(&format!("SELECT COALESCE((SELECT min(id) - 1 FROM {table} WHERE at >= $1), (SELECT COALESCE(max(id), 0) FROM {table}))"))
                    .bind(since)
                    .fetch_one(&mut *conn)
                    .await?
            } else {
                sqlx::query_scalar(&format!("SELECT COALESCE(max(id), 0) FROM {table}")).fetch_one(&mut *conn).await?
            };
            json!({ "id": id })
        }
        Source::Replies | Source::Imports => {
            let (col, table, extra) = if source == Source::Replies { ("created_at", "inbound_messages", "") } else { ("finished_at", "lead_imports", "WHERE finished_at IS NOT NULL") };
            if backfill_hours > 0 {
                pair_cursor(since, Uuid::nil())
            } else {
                let newest: Option<(DateTime<Utc>, Uuid)> = sqlx::query_as(&format!("SELECT {col}, id FROM {table} {extra} ORDER BY {col} DESC, id DESC LIMIT 1")).fetch_optional(&mut *conn).await?;
                match newest {
                    Some((at, id)) => pair_cursor(at, id),
                    None => {
                        let db_now: DateTime<Utc> = sqlx::query_scalar("SELECT now()").fetch_one(&mut *conn).await?;
                        pair_cursor(db_now, Uuid::nil())
                    }
                }
            }
        }
    })
}

// ---------- reading ----------

fn strings(v: Option<&Value>, default: &[&str]) -> Vec<String> {
    match v.and_then(Value::as_array) {
        Some(a) => a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect(),
        None => default.iter().map(|s| s.to_string()).collect(),
    }
}

fn uuid_param(params: &Value, key: &str) -> Option<Uuid> {
    params.get(key).and_then(Value::as_str).and_then(|s| s.trim().parse().ok())
}

/// Read the events after `cursor`. `app_settle` is `now - settle` on the app clock (for tables that store the app time);
/// `settle_secs` is the same wait for the table that stores the database time.
pub async fn read_batch(conn: &mut PgConnection, type_id: &str, params: &Value, cursor: &Value, limit: i64, app_settle: DateTime<Utc>, settle_secs: f64) -> Result<Batch, sqlx::Error> {
    let limit = limit.clamp(1, BATCH);
    match source_of(type_id) {
        Some(Source::Lead) => {
            let kinds: Vec<String> = lead_kinds(type_id).iter().map(|s| s.to_string()).collect();
            let from = cursor_id(cursor);
            #[derive(FromRow)]
            struct Row {
                id: i64,
                lead_id: Uuid,
                kind: String,
                ref_id: Option<Uuid>,
                detail: Value,
                at: DateTime<Utc>,
            }
            let rows: Vec<Row> = sqlx::query_as("SELECT id, lead_id, kind, ref_id, detail, at FROM lead_events WHERE id > $1 AND kind = ANY($2) AND at <= $3 ORDER BY id LIMIT $4")
                .bind(from)
                .bind(&kinds)
                .bind(app_settle)
                .bind(limit)
                .fetch_all(&mut *conn)
                .await?;
            let next = if rows.len() as i64 >= limit {
                rows.last().map_or(from, |r| r.id)
            } else {
                // nothing more of our kind: jump over the events of other kinds
                let newest: Option<i64> = sqlx::query_scalar("SELECT max(id) FROM lead_events WHERE at <= $1").bind(app_settle).fetch_one(&mut *conn).await?;
                newest.unwrap_or(from).max(rows.last().map_or(from, |r| r.id)).max(from)
            };
            let events = rows.into_iter().map(|r| RawEvent { key: r.id.to_string(), kind: r.kind, at: r.at, lead_id: Some(r.lead_id), ref_id: r.ref_id, detail: r.detail, extra: Value::Null }).collect();
            Ok(Batch { events, cursor: json!({ "id": next }) })
        }
        Some(Source::Campaigns) => {
            let from = cursor_id(cursor);
            #[derive(FromRow)]
            struct Row {
                id: i64,
                campaign_id: Uuid,
                from_status: Option<String>,
                to_status: String,
                at: DateTime<Utc>,
            }
            let rows: Vec<Row> = sqlx::query_as("SELECT id, campaign_id, from_status, to_status, at FROM campaign_events WHERE id > $1 AND at <= $2 ORDER BY id LIMIT $3")
                .bind(from)
                .bind(app_settle)
                .bind(limit)
                .fetch_all(&mut *conn)
                .await?;
            let next = rows.last().map_or(from, |r| r.id);
            let events = rows
                .into_iter()
                .map(|r| RawEvent {
                    key: r.id.to_string(),
                    kind: "campaign_status".into(),
                    at: r.at,
                    lead_id: None,
                    ref_id: Some(r.campaign_id),
                    detail: json!({ "from": r.from_status, "to": r.to_status }),
                    extra: Value::Null,
                })
                .collect();
            Ok(Batch { events, cursor: json!({ "id": next }) })
        }
        Some(Source::Replies) => {
            let (from_at, from_id) = cursor_pair(cursor);
            let labels = strings(params.get("labels"), &["reply"]);
            let campaign = uuid_param(params, "campaign_id");
            #[derive(FromRow)]
            struct Row {
                id: Uuid,
                account_id: Uuid,
                message_id_header: Option<String>,
                references_ids: Vec<String>,
                provider_thread_id: Option<String>,
                from_email: String,
                subject: Option<String>,
                received_at: DateTime<Utc>,
                label: String,
                lead_id: Option<Uuid>,
                campaign_id: Option<Uuid>,
                reply_text: Option<String>,
                created_at: DateTime<Utc>,
            }
            let rows: Vec<Row> = sqlx::query_as(
                "SELECT id, account_id, message_id_header, references_ids, provider_thread_id, from_email::text AS from_email, subject, received_at, label::text AS label, \
                        lead_id, campaign_id, reply_text, created_at \
                   FROM inbound_messages \
                  WHERE (created_at, id) > ($1, $2) AND created_at <= now() - make_interval(secs => $3) AND label::text = ANY($4) AND ($5::uuid IS NULL OR campaign_id = $5) \
                  ORDER BY created_at, id LIMIT $6",
            )
            .bind(from_at)
            .bind(from_id)
            .bind(settle_secs)
            .bind(&labels)
            .bind(campaign)
            .bind(limit)
            .fetch_all(&mut *conn)
            .await?;
            let mut next = rows.last().map_or((from_at, from_id), |r| (r.created_at, r.id));
            if (rows.len() as i64) < limit {
                let newest: Option<(DateTime<Utc>, Uuid)> =
                    sqlx::query_as("SELECT created_at, id FROM inbound_messages WHERE created_at <= now() - make_interval(secs => $1) ORDER BY created_at DESC, id DESC LIMIT 1")
                        .bind(settle_secs)
                        .fetch_optional(&mut *conn)
                        .await?;
                if let Some(n) = newest.filter(|n| *n > next) {
                    next = n;
                }
            }
            let events = rows
                .into_iter()
                .map(|r| RawEvent {
                    key: r.id.to_string(),
                    kind: "inbound_message".into(),
                    at: r.received_at,
                    lead_id: r.lead_id,
                    ref_id: r.campaign_id,
                    detail: Value::Null,
                    extra: json!({
                        "id": r.id, "label": r.label, "subject": r.subject, "text": r.reply_text, "from_email": r.from_email, "account_id": r.account_id,
                        "lead_id": r.lead_id, "campaign_id": r.campaign_id, "message_id_header": r.message_id_header, "references_ids": r.references_ids,
                        "provider_thread_id": r.provider_thread_id, "received_at": r.received_at
                    }),
                })
                .collect();
            Ok(Batch { events, cursor: pair_cursor(next.0, next.1) })
        }
        Some(Source::Imports) => {
            let (from_at, from_id) = cursor_pair(cursor);
            let statuses = strings(params.get("status"), &["done"]);
            #[derive(FromRow)]
            struct Row {
                id: Uuid,
                status: String,
                created: i32,
                updated: i32,
                skipped: i32,
                invalid: i32,
                list_id: Option<Uuid>,
                campaign_id: Option<Uuid>,
                finished_at: DateTime<Utc>,
            }
            let rows: Vec<Row> = sqlx::query_as(
                "SELECT id, status::text AS status, created, updated, skipped, invalid, list_id, campaign_id, finished_at FROM lead_imports \
                  WHERE finished_at IS NOT NULL AND (finished_at, id) > ($1, $2) AND finished_at <= $3 AND status::text = ANY($4) \
                  ORDER BY finished_at, id LIMIT $5",
            )
            .bind(from_at)
            .bind(from_id)
            .bind(app_settle)
            .bind(&statuses)
            .bind(limit)
            .fetch_all(&mut *conn)
            .await?;
            let mut next = rows.last().map_or((from_at, from_id), |r| (r.finished_at, r.id));
            if (rows.len() as i64) < limit {
                let newest: Option<(DateTime<Utc>, Uuid)> =
                    sqlx::query_as("SELECT finished_at, id FROM lead_imports WHERE finished_at IS NOT NULL AND finished_at <= $1 ORDER BY finished_at DESC, id DESC LIMIT 1")
                        .bind(app_settle)
                        .fetch_optional(&mut *conn)
                        .await?;
                if let Some(n) = newest.filter(|n| *n > next) {
                    next = n;
                }
            }
            let events = rows
                .into_iter()
                .map(|r| RawEvent {
                    key: r.id.to_string(),
                    kind: "import_finished".into(),
                    at: r.finished_at,
                    lead_id: None,
                    ref_id: Some(r.id),
                    detail: Value::Null,
                    extra: json!({ "id": r.id, "status": r.status, "created": r.created, "updated": r.updated, "skipped": r.skipped, "invalid": r.invalid, "list_id": r.list_id, "campaign_id": r.campaign_id }),
                })
                .collect();
            Ok(Batch { events, cursor: pair_cursor(next.0, next.1) })
        }
        None => Ok(Batch { events: vec![], cursor: cursor.clone() }),
    }
}

// ---------- the filter ----------

fn str_list(params: &Value, key: &str) -> Option<Vec<String>> {
    params.get(key).and_then(Value::as_array).map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect())
}

/// Does the event pass the filter that needs no database? The rest (list membership, the campaign of a sent
/// email) is checked after the item is built, in `late_filter`.
pub fn early_filter(type_id: &str, params: &Value, workflow_id: Uuid, ev: &RawEvent) -> bool {
    // changes made by this workflow itself (default: skip them)
    if params.get("ignore_own_changes").and_then(Value::as_bool).unwrap_or(true) {
        let own = format!("automation:{workflow_id}");
        if ev.detail.get("actor").and_then(Value::as_str) == Some(own.as_str()) {
            return false;
        }
    }
    let detail_str = |k: &str| ev.detail.get(k).and_then(Value::as_str);
    match type_id {
        "trigger.lead_created" => match str_list(params, "source").filter(|l| !l.is_empty()) {
            Some(sources) => detail_str("source").is_some_and(|s| sources.iter().any(|x| x == s)),
            None => true,
        },
        "trigger.lead_added_to_campaign" => match uuid_param(params, "campaign_id") {
            Some(c) => ev.ref_id == Some(c),
            None => true,
        },
        "trigger.lead_added_to_list" => match uuid_param(params, "list_id") {
            Some(l) => ev.ref_id == Some(l),
            None => true,
        },
        "trigger.lead_updated" => match str_list(params, "fields").filter(|l| !l.is_empty()) {
            Some(fields) => !changed_fields(type_id, params, ev).is_empty() && fields.iter().any(|f| changed_fields(type_id, params, ev).contains(f)),
            None => true,
        },
        "trigger.lead_interest_changed" => match str_list(params, "to").filter(|l| !l.is_empty()) {
            Some(to) => ev.detail.get("to").and_then(Value::as_str).is_some_and(|t| to.iter().any(|x| x == t)),
            None => true,
        },
        "trigger.email_sent" => match params.get("step_number").and_then(Value::as_i64) {
            Some(n) => ev.detail.get("step_number").and_then(Value::as_i64) == Some(n),
            None => true,
        },
        "trigger.lead_bounced" => {
            let kinds = str_list(params, "kinds").unwrap_or_else(|| vec!["hard".into()]);
            kinds.iter().any(|k| *k == bounce_kind(ev))
        }
        "trigger.campaign_status_changed" => {
            let campaign_ok = uuid_param(params, "campaign_id").is_none_or(|c| ev.ref_id == Some(c));
            let to_ok = str_list(params, "to").filter(|l| !l.is_empty()).is_none_or(|to| detail_str("to").is_some_and(|t| to.iter().any(|x| x == t)));
            campaign_ok && to_ok
        }
        _ => true,
    }
}

/// `hard` for a hard bounce, `soft` for a block (the mail server refused for now).
pub fn bounce_kind(ev: &RawEvent) -> &'static str {
    if ev.detail.get("block").and_then(Value::as_bool).unwrap_or(false) || ev.detail.get("reason").and_then(Value::as_str) == Some("bounce_block") { "soft" } else { "hard" }
}

/// The field names an `updated` event lists (`detail.fields`).
pub fn changed_fields(_type_id: &str, _params: &Value, ev: &RawEvent) -> Vec<String> {
    ev.detail.get("fields").and_then(Value::as_array).map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect()).unwrap_or_default()
}

// ---------- the item ----------

async fn lead_json(conn: &mut PgConnection, id: Uuid) -> Result<Option<Value>, sqlx::Error> {
    let Some(lead) = crate::leads::repo::get(conn, id).await? else { return Ok(None) };
    let detail = crate::leads::repo::detail(conn, lead).await?;
    Ok(serde_json::to_value(detail).ok())
}

pub async fn campaign_json(conn: &mut PgConnection, id: Uuid) -> Result<Option<Value>, sqlx::Error> {
    let row: Option<(Uuid, String, String, String)> = sqlx::query_as("SELECT id, name, status::text, content_mode FROM campaigns WHERE id = $1").bind(id).fetch_optional(conn).await?;
    Ok(row.map(|(id, name, status, content_mode)| json!({ "id": id, "name": name, "status": status, "content_mode": content_mode })))
}

async fn list_json(conn: &mut PgConnection, id: Uuid) -> Result<Option<Value>, sqlx::Error> {
    let row: Option<(Uuid, String)> = sqlx::query_as("SELECT id, name FROM lead_lists WHERE id = $1").bind(id).fetch_optional(conn).await?;
    Ok(row.map(|(id, name)| json!({ "id": id, "name": name })))
}

/// The `event` object every trigger item has.
fn event_object(ev: &RawEvent, type_name: &str) -> Value {
    json!({ "id": ev.key, "type": type_name, "at": ev.at })
}

pub enum Built {
    Item(Map<String, Value>),
    /// The lead (or campaign, or list) is gone. No run is made.
    Deleted,
    /// The event does not pass a check that needs the item.
    Filtered,
}

/// Build the trigger item of an event (build map 7.1). The lead is read now, not as it was at the time of the event.
pub async fn build_item(conn: &mut PgConnection, type_id: &str, params: &Value, ev: &RawEvent) -> Result<Built, sqlx::Error> {
    let mut item = Map::new();
    let type_name = ev.kind.as_str();
    item.insert("event".into(), event_object(ev, type_name));

    // the lead, for every lead trigger and for a reply that is matched to one
    let needs_lead = source_of(type_id) == Some(Source::Lead);
    if needs_lead {
        let Some(id) = ev.lead_id else { return Ok(Built::Deleted) };
        let Some(lead) = lead_json(conn, id).await? else { return Ok(Built::Deleted) };
        item.insert("lead".into(), lead);
    }

    match type_id {
        "trigger.lead_created" => {
            if let Some(list_id) = uuid_param(params, "list_id") {
                let lead = ev.lead_id.unwrap_or_default();
                let member: Option<i32> = sqlx::query_scalar("SELECT 1 FROM lead_list_members WHERE list_id = $1 AND lead_id = $2").bind(list_id).bind(lead).fetch_optional(&mut *conn).await?;
                if member.is_none() {
                    return Ok(Built::Filtered);
                }
            }
        }
        "trigger.lead_added_to_campaign" => {
            let Some(campaign) = ev.ref_id else { return Ok(Built::Deleted) };
            let Some(c) = campaign_json(conn, campaign).await? else { return Ok(Built::Deleted) };
            item.insert("campaign".into(), c);
        }
        "trigger.lead_added_to_list" => {
            let Some(list) = ev.ref_id else { return Ok(Built::Deleted) };
            let Some(l) = list_json(conn, list).await? else { return Ok(Built::Deleted) };
            item.insert("list".into(), l);
        }
        "trigger.lead_updated" => {
            let changed = changed_fields(type_id, params, ev);
            let wanted = str_list(params, "fields").filter(|l| !l.is_empty());
            let shown: Vec<String> = match wanted {
                Some(w) => changed.into_iter().filter(|f| w.contains(f)).collect(),
                None => changed,
            };
            item.insert("changed".into(), json!(shown));
        }
        "trigger.lead_interest_changed" => {
            item.insert("from".into(), ev.detail.get("from").cloned().unwrap_or(Value::Null));
            item.insert("to".into(), ev.detail.get("to").cloned().unwrap_or(Value::Null));
        }
        "trigger.email_sent" => {
            let Some(job_id) = ev.ref_id else { return Ok(Built::Deleted) };
            let job: Option<(Uuid, String, String, Option<DateTime<Utc>>, Uuid, Option<Uuid>)> =
                sqlx::query_as("SELECT id, to_email::text, subject, sent_at, account_id, campaign_id FROM send_jobs WHERE id = $1").bind(job_id).fetch_optional(&mut *conn).await?;
            let Some((id, to_email, subject, sent_at, account_id, campaign_id)) = job else { return Ok(Built::Deleted) };
            if let Some(want) = uuid_param(params, "campaign_id") {
                if campaign_id != Some(want) {
                    return Ok(Built::Filtered);
                }
            }
            let step = ev.detail.get("step_number").cloned().unwrap_or(Value::Null);
            item.insert("job".into(), json!({ "id": id, "to_email": to_email, "subject": subject, "step_number": step, "sent_at": sent_at, "account_id": account_id }));
            let campaign = match campaign_id {
                Some(c) => campaign_json(conn, c).await?,
                None => None,
            };
            item.insert("campaign".into(), campaign.unwrap_or(Value::Null));
        }
        "trigger.lead_bounced" => {
            item.insert("bounce".into(), json!({ "kind": bounce_kind(ev), "at": ev.at }));
        }
        "trigger.lead_unsubscribed" => {
            item.insert("reason".into(), json!(ev.kind));
        }
        "trigger.reply_received" => {
            item.insert("message".into(), ev.extra.clone());
            let lead = match ev.lead_id {
                Some(id) => lead_json(conn, id).await?,
                None => None,
            };
            item.insert("lead".into(), lead.unwrap_or(Value::Null));
            let campaign = match ev.ref_id {
                Some(id) => campaign_json(conn, id).await?,
                None => None,
            };
            item.insert("campaign".into(), campaign.unwrap_or(Value::Null));
        }
        "trigger.campaign_status_changed" => {
            let Some(campaign) = ev.ref_id else { return Ok(Built::Deleted) };
            let Some(c) = campaign_json(conn, campaign).await? else { return Ok(Built::Deleted) };
            item.insert("campaign".into(), c);
            item.insert("from".into(), ev.detail.get("from").cloned().unwrap_or(Value::Null));
            item.insert("to".into(), ev.detail.get("to").cloned().unwrap_or(Value::Null));
        }
        "trigger.import_finished" => {
            item.insert("import".into(), ev.extra.clone());
        }
        _ => {}
    }
    Ok(Built::Item(item))
}

/// The trigger item for a test: the same shape as a real one, built from a lead the user picked (build map 10.2).
/// `event.id` is `test`.
pub async fn test_item(st: &AppState, type_id: &str, lead_id: Option<Uuid>, campaign_id: Option<Uuid>, list_id: Option<Uuid>, message_id: Option<Uuid>) -> Result<Option<Map<String, Value>>, sqlx::Error> {
    let mut conn = st.pool.acquire().await?;
    let now = st.now();
    let mut ev = RawEvent { key: "test".into(), kind: type_id.trim_start_matches("trigger.").into(), at: now, lead_id, ref_id: campaign_id.or(list_id), detail: json!({}), extra: Value::Null };
    if type_id == "trigger.lead_added_to_list" {
        ev.ref_id = list_id;
    }
    if type_id == "trigger.reply_received" {
        let Some(id) = message_id else { return Ok(None) };
        let row: Option<(Option<Uuid>, Option<Uuid>)> = sqlx::query_as("SELECT lead_id, campaign_id FROM inbound_messages WHERE id = $1").bind(id).fetch_optional(&mut *conn).await?;
        let Some((lead, campaign)) = row else { return Ok(None) };
        let b = read_one_message(&mut conn, id).await?;
        ev.lead_id = lead;
        ev.ref_id = campaign;
        ev.extra = b.unwrap_or(Value::Null);
    }
    match build_item(&mut conn, type_id, &json!({}), &ev).await? {
        Built::Item(mut m) => {
            if let Some(e) = m.get_mut("event").and_then(Value::as_object_mut) {
                e.insert("id".into(), json!("test"));
            }
            Ok(Some(m))
        }
        _ => Ok(None),
    }
}

async fn read_one_message(conn: &mut PgConnection, id: Uuid) -> Result<Option<Value>, sqlx::Error> {
    #[allow(clippy::type_complexity)]
    let r: Option<(Uuid, String, Option<String>, Option<String>, String, Uuid, Option<Uuid>, Option<Uuid>, Option<String>, Vec<String>, Option<String>, DateTime<Utc>)> = sqlx::query_as(
        "SELECT id, label::text, subject, reply_text, from_email::text, account_id, lead_id, campaign_id, message_id_header, references_ids, provider_thread_id, received_at FROM inbound_messages WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(conn)
    .await?;
    Ok(r.map(|(id, label, subject, text, from_email, account_id, lead_id, campaign_id, mid, refs, thread, received_at)| {
        json!({ "id": id, "label": label, "subject": subject, "text": text, "from_email": from_email, "account_id": account_id, "lead_id": lead_id, "campaign_id": campaign_id,
                "message_id_header": mid, "references_ids": refs, "provider_thread_id": thread, "received_at": received_at })
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(kind: &str, detail: Value) -> RawEvent {
        RawEvent { key: "1".into(), kind: kind.into(), at: Utc::now(), lead_id: Some(Uuid::now_v7()), ref_id: None, detail, extra: Value::Null }
    }

    #[test]
    fn every_event_trigger_has_a_source() {
        for t in [
            "trigger.lead_created", "trigger.lead_added_to_campaign", "trigger.lead_added_to_list", "trigger.lead_updated", "trigger.lead_interest_changed",
            "trigger.reply_received", "trigger.email_sent", "trigger.lead_bounced", "trigger.lead_unsubscribed", "trigger.campaign_status_changed", "trigger.import_finished",
        ] {
            assert!(source_of(t).is_some(), "{t}");
            assert!(!event_types(t).is_empty(), "{t}");
        }
        for t in ["trigger.manual", "trigger.schedule", "trigger.cron", "trigger.webhook", "web.fetch_page"] {
            assert!(source_of(t).is_none(), "{t}");
        }
        assert_eq!(lead_kinds("trigger.lead_unsubscribed"), &["unsubscribed", "complained"]);
    }

    #[test]
    fn a_workflow_skips_its_own_changes_unless_told_not_to() {
        let wf = Uuid::now_v7();
        let own = ev("campaign_added", json!({ "actor": format!("automation:{wf}"), "automation_run_id": "r1" }));
        let other = ev("campaign_added", json!({ "actor": format!("automation:{}", Uuid::now_v7()) }));
        let plain = ev("campaign_added", json!({}));
        let p = json!({});
        assert!(!early_filter("trigger.lead_added_to_campaign", &p, wf, &own));
        assert!(early_filter("trigger.lead_added_to_campaign", &p, wf, &other), "another workflow's change starts this one");
        assert!(early_filter("trigger.lead_added_to_campaign", &p, wf, &plain));
        assert!(early_filter("trigger.lead_added_to_campaign", &json!({ "ignore_own_changes": false }), wf, &own));
    }

    #[test]
    fn campaign_list_and_source_filters() {
        let wf = Uuid::now_v7();
        let (c1, c2) = (Uuid::now_v7(), Uuid::now_v7());
        let mut e = ev("campaign_added", json!({}));
        e.ref_id = Some(c1);
        assert!(early_filter("trigger.lead_added_to_campaign", &json!({ "campaign_id": c1.to_string() }), wf, &e));
        assert!(!early_filter("trigger.lead_added_to_campaign", &json!({ "campaign_id": c2.to_string() }), wf, &e));
        assert!(early_filter("trigger.lead_added_to_campaign", &json!({ "campaign_id": "" }), wf, &e), "an empty id means any campaign");
        let mut l = ev("list_added", json!({}));
        l.ref_id = Some(c1);
        assert!(early_filter("trigger.lead_added_to_list", &json!({ "list_id": c1.to_string() }), wf, &l));
        assert!(!early_filter("trigger.lead_added_to_list", &json!({ "list_id": c2.to_string() }), wf, &l));
        let created = ev("created", json!({ "source": "csv" }));
        assert!(early_filter("trigger.lead_created", &json!({ "source": ["api", "csv"] }), wf, &created));
        assert!(!early_filter("trigger.lead_created", &json!({ "source": ["api"] }), wf, &created));
        assert!(early_filter("trigger.lead_created", &json!({ "source": [] }), wf, &created));
    }

    #[test]
    fn updated_interest_and_step_filters() {
        let wf = Uuid::now_v7();
        let upd = ev("updated", json!({ "fields": ["job_title", "phone"] }));
        assert!(early_filter("trigger.lead_updated", &json!({}), wf, &upd));
        assert!(early_filter("trigger.lead_updated", &json!({ "fields": ["phone"] }), wf, &upd));
        assert!(!early_filter("trigger.lead_updated", &json!({ "fields": ["website"] }), wf, &upd));
        assert_eq!(changed_fields("trigger.lead_updated", &json!({}), &upd), vec!["job_title", "phone"]);
        let none = ev("updated", json!({}));
        assert!(!early_filter("trigger.lead_updated", &json!({ "fields": ["phone"] }), wf, &none));
        let int = ev("interest_changed", json!({ "from": null, "to": "interested" }));
        assert!(early_filter("trigger.lead_interest_changed", &json!({ "to": ["interested", "meeting"] }), wf, &int));
        assert!(!early_filter("trigger.lead_interest_changed", &json!({ "to": ["not_interested"] }), wf, &int));
        let sent = ev("step_sent", json!({ "step_number": 2 }));
        assert!(early_filter("trigger.email_sent", &json!({ "step_number": 2 }), wf, &sent));
        assert!(!early_filter("trigger.email_sent", &json!({ "step_number": 1 }), wf, &sent));
        assert!(early_filter("trigger.email_sent", &json!({}), wf, &sent));
    }

    #[test]
    fn bounces_are_hard_unless_the_server_only_blocked() {
        let wf = Uuid::now_v7();
        let hard = ev("bounced", json!({ "reason": "hard_bounce" }));
        let soft = ev("bounced", json!({ "reason": "bounce_block", "block": true }));
        assert_eq!(bounce_kind(&hard), "hard");
        assert_eq!(bounce_kind(&soft), "soft");
        assert!(early_filter("trigger.lead_bounced", &json!({}), wf, &hard), "the default is hard bounces");
        assert!(!early_filter("trigger.lead_bounced", &json!({}), wf, &soft));
        assert!(early_filter("trigger.lead_bounced", &json!({ "kinds": ["hard", "soft"] }), wf, &soft));
    }

    #[test]
    fn campaign_status_filters_by_campaign_and_new_status() {
        let wf = Uuid::now_v7();
        let c = Uuid::now_v7();
        let mut e = ev("campaign_status", json!({ "from": "draft", "to": "active" }));
        e.ref_id = Some(c);
        assert!(early_filter("trigger.campaign_status_changed", &json!({}), wf, &e));
        assert!(early_filter("trigger.campaign_status_changed", &json!({ "campaign_id": c.to_string(), "to": ["active"] }), wf, &e));
        assert!(!early_filter("trigger.campaign_status_changed", &json!({ "to": ["paused"] }), wf, &e));
        assert!(!early_filter("trigger.campaign_status_changed", &json!({ "campaign_id": Uuid::now_v7().to_string() }), wf, &e));
    }

    #[test]
    fn cursors_round_trip() {
        assert_eq!(cursor_id(&json!({ "id": 42 })), 42);
        assert_eq!(cursor_id(&json!({})), 0);
        let (at, id) = (Utc::now(), Uuid::now_v7());
        assert_eq!(cursor_pair(&pair_cursor(at, id)), (at, id));
        assert_eq!(cursor_pair(&json!({})), (DateTime::<Utc>::UNIX_EPOCH, Uuid::nil()));
    }
}
