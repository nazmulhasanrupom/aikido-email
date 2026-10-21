//! One inbound email through the pipeline: parse, classify, match, then store and act.
//! Everything that writes happens in ONE transaction, and the actions run only if the insert made a
//! new row. This is what makes it safe to process the same email twice.
use super::actions::{self, ActionContext};
use super::classify::classify_raw;
use super::dsn;
use super::matcher::{self, MatchInput};
use super::model::{INBOUND_COLS, InboundMessage, Label, MatchMethod};
use super::parse::{HeaderMap, message_ids};
use super::quote::strip_quoted;
use super::settings::ReplySettings;
use crate::accounts::model::EmailAccount;
use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

/// One email from the inbox, ready for the pipeline.
pub struct Inbound<'a> {
    pub account: &'a EmailAccount,
    /// The sync folder: `inbox`, `spam` or `all`.
    pub folder: &'a str,
    pub provider_message_id: &'a str,
    pub provider_thread_id: Option<&'a str>,
    /// The raw MIME. With `truncated`, only the headers.
    pub raw: &'a [u8],
    pub truncated: bool,
    /// Headers from the listing step. Used when the raw email cannot be parsed.
    pub headers: Option<&'a HeaderMap>,
    /// When the provider got the email.
    pub received_at: Option<DateTime<Utc>>,
}

#[derive(Debug)]
pub enum Ingested {
    /// Stored. `actions` is the number of actions that ran.
    Stored { id: Uuid, label: Label, actions: usize },
    /// The email was already stored. Nothing ran.
    Duplicate,
    /// A reply, auto-reply or unsubscribe that does not link to our sends. Not stored.
    Dropped,
}

pub async fn ingest(
    pool: &PgPool,
    inbound: &Inbound<'_>,
    settings: &ReplySettings,
    exempt_domains: &[String],
    now: DateTime<Utc>,
) -> Result<Ingested, sqlx::Error> {
    let account = inbound.account;
    let (parsed, c) = classify_raw(inbound.raw, settings);

    // What we know about the email, from the parsed email or, if that failed, from the listing headers.
    let hdr = |name: &str| inbound.headers.and_then(|h| h.get(name));
    let (from_email, from_name, subject, in_reply_to, references) = match &parsed {
        Some(p) => (
            p.from_email.clone(),
            p.from_name.clone(),
            p.subject.clone(),
            p.in_reply_to.clone(),
            p.references.clone(),
        ),
        None => (
            hdr("from").and_then(|v| dsn::addresses_in_text(v).into_iter().next()),
            None,
            hdr("subject").cloned(),
            hdr("in-reply-to").and_then(|v| message_ids(v).into_iter().next()),
            hdr("references").map(|v| message_ids(v)).unwrap_or_default(),
        ),
    };
    let from_email = from_email.unwrap_or_else(|| "unknown@invalid".into());
    let bounce = c.bounce.clone();
    let candidates = match (&parsed, &bounce) {
        (Some(p), Some(b)) if b.recipient.is_none() => dsn::extract_bounce(p).1,
        _ => vec![],
    };

    let mut tx = pool.begin().await?;
    let m = matcher::find_match(
        &mut tx,
        &MatchInput {
            account_id: Some(account.id),
            from_email: &from_email,
            in_reply_to: in_reply_to.as_deref(),
            references: &references,
            thread_id: inbound.provider_thread_id,
            original_message_id: bounce.as_ref().and_then(|b| b.original_message_id.as_deref()),
            bounce_recipient: bounce.as_ref().and_then(|b| b.recipient.as_deref()),
            bounce_candidates: &candidates,
            match_window_days: settings.match_window_days,
        },
        now,
    )
    .await?;
    if c.label.needs_match() && m.job.is_none() {
        return Ok(Ingested::Dropped);
    }

    // The address a bounce is about: the report, else the body address we sent to, else the matched send.
    let bounce_recipient = bounce
        .as_ref()
        .and_then(|b| b.recipient.clone())
        .or(m.resolved_recipient.clone())
        .or_else(|| {
            let by_id = matches!(m.method, MatchMethod::DsnOriginal | MatchMethod::InReplyTo | MatchMethod::References | MatchMethod::Thread);
            (bounce.is_some() && by_id).then(|| m.job.as_ref().map(|j| j.to_email.clone())).flatten()
        });
    let reply_text = match (&parsed, c.label) {
        (Some(p), Label::Reply | Label::AutoReply | Label::Unsubscribe) => Some(strip_quoted(&p.text)),
        _ => None,
    };
    let received_at = inbound.received_at.or(parsed.as_ref().and_then(|p| p.date)).unwrap_or(now);
    let id = Uuid::now_v7();

    let inserted = sqlx::query(
        "INSERT INTO inbound_messages (id, account_id, folder, provider_message_id, message_id_header, in_reply_to, references_ids,
            provider_thread_id, from_email, from_name, subject, received_at, label, label_reason, match_method, matched_job_id,
            lead_id, campaign_id, bounce_recipient, bounce_status, bounce_diagnostic, reply_text, truncated, created_at, updated_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9::citext, $10, $11, $12, $13, $14, $15, $16, $17, $18, $19::citext, $20, $21, $22, $23, $24, $24)
         ON CONFLICT (account_id, provider_message_id) DO NOTHING",
    )
    .bind(id)
    .bind(account.id)
    .bind(inbound.folder)
    .bind(inbound.provider_message_id)
    .bind(parsed.as_ref().and_then(|p| p.message_id.clone()))
    .bind(&in_reply_to)
    .bind(&references)
    .bind(inbound.provider_thread_id)
    .bind(&from_email)
    .bind(&from_name)
    .bind(&subject)
    .bind(received_at)
    .bind(c.label)
    .bind(&c.reason)
    .bind(m.method)
    .bind(m.job.as_ref().map(|j| j.id))
    .bind(m.job.as_ref().and_then(|j| j.lead_id))
    .bind(m.job.as_ref().and_then(|j| j.campaign_id))
    .bind(&bounce_recipient)
    .bind(bounce.as_ref().and_then(|b| b.status.clone()))
    .bind(bounce.as_ref().and_then(|b| b.diagnostic.clone()))
    .bind(&reply_text)
    .bind(inbound.truncated)
    .bind(now)
    .execute(&mut *tx)
    .await?;
    if inserted.rows_affected() == 0 {
        return Ok(Ingested::Duplicate);
    }

    let actions = if c.label == Label::Unknown {
        // A parse error: stored, event only. No suppression, no cancel.
        crate::sending::queue::log_event(
            &mut tx,
            m.job.as_ref().map(|j| j.id),
            account.id,
            "inbound_unknown",
            serde_json::json!({ "inbound_message_id": id, "label": "unknown", "reason": c.reason }),
            now,
        )
        .await?;
        vec![]
    } else {
        actions::apply(
            &mut tx,
            &ActionContext {
                account_id: account.id,
                own_email: &account.email,
                message_id: id,
                label: c.label,
                from_email: &from_email,
                bounce_recipient: bounce_recipient.as_deref(),
                matched: m.job.as_ref(),
                settings,
                exempt_domains,
                now,
            },
        )
        .await?
    };
    sqlx::query("UPDATE inbound_messages SET actions = $2 WHERE id = $1")
        .bind(id)
        .bind(serde_json::to_value(&actions).unwrap_or(Value::Array(vec![])))
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    tracing::info!(account_id = %account.id, message_id = %id, label = ?c.label, matched = m.job.is_some(), method = ?m.method, actions = actions.len(), "inbound email stored");
    Ok(Ingested::Stored { id, label: c.label, actions: actions.len() })
}

pub async fn get(pool: &PgPool, id: Uuid) -> Result<Option<InboundMessage>, sqlx::Error> {
    sqlx::query_as(&format!("SELECT {INBOUND_COLS} FROM inbound_messages WHERE id = $1")).bind(id).fetch_optional(pool).await
}

#[derive(Debug)]
pub enum RelabelError {
    NotFound,
    Db(sqlx::Error),
}

impl From<sqlx::Error> for RelabelError {
    fn from(e: sqlx::Error) -> Self {
        RelabelError::Db(e)
    }
}

/// Fix a label by hand. Undo what the old label did (the suppressions this email made, the marks on the job),
/// then run the actions of the new label. Cancelled jobs are not restored: requeue them on purpose.
pub async fn relabel(pool: &PgPool, id: Uuid, label: Label, now: DateTime<Utc>) -> Result<InboundMessage, RelabelError> {
    let mut tx = pool.begin().await?;
    let row: Option<InboundMessage> =
        sqlx::query_as(&format!("SELECT {INBOUND_COLS} FROM inbound_messages WHERE id = $1 FOR UPDATE")).bind(id).fetch_optional(&mut *tx).await?;
    let Some(row) = row else { return Err(RelabelError::NotFound) };

    let account: EmailAccount = sqlx::query_as(&format!(
        "SELECT {} FROM email_accounts WHERE id = $1",
        crate::accounts::model::ACCOUNT_COLS
    ))
    .bind(row.account_id)
    .fetch_one(&mut *tx)
    .await?;
    let settings = super::repo::load_settings(&mut tx, row.account_id).await?;
    let exempt = crate::sending::queue::load_settings(&mut tx, row.account_id).await?.per_domain_exempt;

    actions::undo(&mut tx, id, &row.actions).await?;
    sqlx::query(
        "UPDATE inbound_messages SET label = $2, label_source = 'manual', label_reason = 'set by hand', updated_at = $3 WHERE id = $1",
    )
    .bind(id)
    .bind(label)
    .bind(now)
    .execute(&mut *tx)
    .await?;
    let matched: Option<matcher::MatchedJob> = match row.matched_job_id {
        Some(job_id) => sqlx::query_as(
            "SELECT id, account_id, lead_id, campaign_id, to_email::text AS to_email, recipient_domain, replied_at FROM send_jobs WHERE id = $1",
        )
        .bind(job_id)
        .fetch_optional(&mut *tx)
        .await?,
        None => None,
    };
    let done = if label == Label::Unknown {
        vec![]
    } else {
        actions::apply(
            &mut tx,
            &ActionContext {
                account_id: row.account_id,
                own_email: &account.email,
                message_id: id,
                label,
                from_email: &row.from_email,
                bounce_recipient: row.bounce_recipient.as_deref(),
                matched: matched.as_ref(),
                settings: &settings,
                exempt_domains: &exempt,
                now,
            },
        )
        .await?
    };
    sqlx::query("UPDATE inbound_messages SET actions = $2 WHERE id = $1")
        .bind(id)
        .bind(serde_json::to_value(&done).unwrap_or(Value::Array(vec![])))
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(get(pool, id).await?.ok_or(RelabelError::NotFound)?)
}
