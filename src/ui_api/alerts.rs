//! The alerts for the bell in the top bar (B10). Each alert is read from the current state,
//! so it goes away by itself when the cause is fixed. Nothing is stored.
use crate::state::AppState;
use chrono::{DateTime, Utc};
use serde::Serialize;
use utoipa::ToSchema;

/// `error` needs action now. `warning` needs a look. `info` is good to know.
#[derive(Serialize, ToSchema, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Error,
    Warning,
    Info,
}

#[derive(Serialize, ToSchema, Clone, Debug)]
pub struct Alert {
    /// Stable while the cause stays: the kind and the thing it is about.
    pub id: String,
    /// `account_disconnected`, `account_error`, `account_reconnect_needed`, `campaign_warning`,
    /// `campaign_no_healthy_account`, `generation_awaiting_review`, `provider_auth_failed`, `ai_budget_reached`,
    /// `import_failed`, `jobs_stuck`, `engine_paused`.
    pub kind: String,
    pub severity: Severity,
    pub title: String,
    pub detail: String,
    /// A path in the web app.
    pub link: String,
    pub since: DateTime<Utc>,
}

/// All kinds, for the notification settings.
pub const KINDS: &[&str] = &[
    "account_disconnected",
    "account_error",
    "account_reconnect_needed",
    "campaign_warning",
    "campaign_no_healthy_account",
    "generation_awaiting_review",
    "provider_auth_failed",
    "ai_budget_reached",
    "import_failed",
    "jobs_stuck",
    "engine_paused",
];

const WS: &str = "/w/default";

fn alert(kind: &str, id_part: &str, severity: Severity, title: String, detail: String, link: String, since: DateTime<Utc>) -> Alert {
    Alert { id: format!("{kind}:{id_part}"), kind: kind.into(), severity, title, detail, link, since }
}

/// Words for `read_status`.
fn read_problem(status: Option<&str>) -> &'static str {
    match status {
        Some("login_failed") => "The mailbox login failed. Check the password.",
        Some("not_configured") => "No IMAP login is saved, so replies cannot be read. Follow-ups wait.",
        _ => "The account has no permission to read the mailbox. Connect it again. Follow-ups wait.",
    }
}

pub async fn collect(st: &AppState) -> Result<Vec<Alert>, sqlx::Error> {
    let p = &st.pool;
    let now = st.now();
    let mut out: Vec<Alert> = Vec::new();

    // accounts
    let rows: Vec<(uuid::Uuid, String, String, Option<String>, bool, Option<String>, DateTime<Utc>)> = sqlx::query_as(
        "SELECT id, email::text, status::text, last_error, can_read, read_status, updated_at FROM email_accounts
          WHERE deleted_at IS NULL AND (status IN ('disconnected', 'error') OR (status = 'active' AND NOT can_read))
          ORDER BY email",
    )
    .fetch_all(p)
    .await?;
    for (id, email, status, last_error, can_read, read_status, at) in rows {
        let link = format!("{WS}/accounts/{id}");
        let detail = last_error.clone().unwrap_or_default();
        match (status.as_str(), can_read) {
            ("disconnected", _) => out.push(alert(
                "account_disconnected",
                &id.to_string(),
                Severity::Error,
                format!("{email} is disconnected"),
                if detail.is_empty() { "Connect it again.".into() } else { detail },
                link,
                at,
            )),
            ("error", _) => out.push(alert(
                "account_error",
                &id.to_string(),
                Severity::Error,
                format!("Sending stopped for {email}"),
                if detail.is_empty() { "The account has a problem. Open it to see why.".into() } else { detail },
                link,
                at,
            )),
            (_, false) => out.push(alert(
                "account_reconnect_needed",
                &id.to_string(),
                Severity::Warning,
                format!("We cannot see replies for {email}"),
                read_problem(read_status.as_deref()).into(),
                format!("{link}/reply-check"),
                at,
            )),
            _ => {}
        }
    }

    // campaigns
    let rows: Vec<(uuid::Uuid, String, String, DateTime<Utc>)> =
        sqlx::query_as("SELECT id, name, warning, updated_at FROM campaigns WHERE status = 'active' AND warning IS NOT NULL ORDER BY name")
            .fetch_all(p)
            .await?;
    for (id, name, warning, at) in rows {
        let no_account = warning.starts_with("No account of the pool is healthy");
        out.push(alert(
            if no_account { "campaign_no_healthy_account" } else { "campaign_warning" },
            &id.to_string(),
            if no_account { Severity::Error } else { Severity::Warning },
            if no_account { format!("{name} has no healthy account") } else { format!("{name} is stuck") },
            warning,
            format!("{WS}/campaigns/{id}/settings"),
            at,
        ));
    }

    // AI texts waiting for a person
    let rows: Vec<(uuid::Uuid, String, i64, DateTime<Utc>)> = sqlx::query_as(
        "SELECT c.id, c.name, count(*), min(g.created_at) FROM generations g
           JOIN sequences s ON s.id = g.sequence_id JOIN campaigns c ON c.id = s.campaign_id
          WHERE g.status = 'pending_review' GROUP BY c.id, c.name ORDER BY c.name",
    )
    .fetch_all(p)
    .await?;
    for (id, name, n, since) in rows {
        out.push(alert(
            "generation_awaiting_review",
            &id.to_string(),
            Severity::Info,
            format!("{n} AI {} to review in {name}", if n == 1 { "text" } else { "texts" }),
            "Emails wait until you approve them.".into(),
            format!("{WS}/campaigns/{id}/followups"),
            since,
        ));
    }

    // AI providers and budgets
    let rows: Vec<(uuid::Uuid, String, DateTime<Utc>)> =
        sqlx::query_as("SELECT id, name, updated_at FROM llm_providers WHERE status = 'auth_failed' ORDER BY name").fetch_all(p).await?;
    for (id, name, at) in rows {
        out.push(alert(
            "provider_auth_failed",
            &id.to_string(),
            Severity::Error,
            format!("The AI provider {name} refused the key"),
            "AI parts use their fallback text until you save a working key.".into(),
            format!("{WS}/settings/integrations/ai-models"),
            at,
        ));
    }
    let settings = {
        let mut conn = p.acquire().await?;
        crate::generator::repo::load_settings(&mut conn).await?
    };
    if settings.daily_token_budget > 0 {
        let mut conn = p.acquire().await?;
        let used = crate::generator::repo::tokens_today(&mut conn, now).await?;
        if used as u64 >= settings.daily_token_budget {
            out.push(alert(
                "ai_budget_reached",
                "daily",
                Severity::Warning,
                "The daily AI token limit is used up".into(),
                format!("{used} of {} tokens used today. AI parts use their fallback text until tomorrow.", settings.daily_token_budget),
                format!("{WS}/settings/integrations/ai-models"),
                now,
            ));
        }
    }
    let rows: Vec<(uuid::Uuid, String, i64, i64)> = sqlx::query_as(
        "SELECT c.id, c.name, gc.token_budget,
                COALESCE((SELECT sum(input_tokens + output_tokens) FROM llm_calls l WHERE l.campaign_id = c.id), 0)::bigint
           FROM generator_campaigns gc JOIN campaigns c ON c.id = gc.campaign_id
          WHERE gc.token_budget IS NOT NULL AND c.status IN ('active', 'paused') ORDER BY c.name",
    )
    .fetch_all(p)
    .await?;
    for (id, name, budget, used) in rows {
        if used >= budget {
            out.push(alert(
                "ai_budget_reached",
                &id.to_string(),
                Severity::Warning,
                format!("The AI token limit of {name} is used up"),
                format!("{used} of {budget} tokens used."),
                format!("{WS}/campaigns/{id}/settings"),
                now,
            ));
        }
    }

    // imports and stuck jobs
    let rows: Vec<(uuid::Uuid, Option<String>, Option<String>, DateTime<Utc>)> = sqlx::query_as(
        "SELECT id, file_name, error, COALESCE(finished_at, created_at) FROM lead_imports
          WHERE status = 'failed' AND created_at > $1 - interval '7 days' ORDER BY created_at DESC",
    )
    .bind(now)
    .fetch_all(p)
    .await?;
    for (id, file, error, at) in rows {
        out.push(alert(
            "import_failed",
            &id.to_string(),
            Severity::Warning,
            format!("The import of {} failed", file.unwrap_or_else(|| "a file".into())),
            error.unwrap_or_else(|| "Open the import to see why.".into()),
            format!("{WS}/leads?tab=imports"),
            at,
        ));
    }
    let (stuck, since): (i64, Option<DateTime<Utc>>) =
        sqlx::query_as("SELECT count(*), min(updated_at) FROM send_jobs WHERE status = 'stuck'").fetch_one(p).await?;
    if stuck > 0 {
        out.push(alert(
            "jobs_stuck",
            "all",
            Severity::Error,
            format!("{stuck} {} stuck", if stuck == 1 { "email is" } else { "emails are" }),
            "The server stopped while sending. Check if they were delivered before you send them again.".into(),
            format!("{WS}/accounts"),
            since.unwrap_or(now),
        ));
    }

    // engine switches
    for (engine, label, sql) in [
        ("sending", "Sending", "SELECT engine_paused, updated_at FROM sending_defaults WHERE id = 1"),
        ("replies", "The reply check", "SELECT engine_paused, updated_at FROM reply_defaults WHERE id = 1"),
        ("followups", "Follow-ups", "SELECT engine_paused, updated_at FROM followup_defaults WHERE id = 1"),
        ("feeder", "The campaign feeder", "SELECT feeder_paused, updated_at FROM lead_defaults WHERE id = 1"),
        ("generator", "The AI writer", "SELECT engine_paused, updated_at FROM generator_defaults WHERE id = 1"),
    ] {
        let (paused, at): (bool, DateTime<Utc>) = sqlx::query_as(sql).fetch_one(p).await?;
        if paused {
            out.push(alert(
                "engine_paused",
                engine,
                Severity::Warning,
                format!("{label} is paused for everyone"),
                "Nothing new is done by this part until it is resumed.".into(),
                format!("{WS}/status"),
                at,
            ));
        }
    }

    out.sort_by(|a, b| a.severity.cmp(&b.severity).then(b.since.cmp(&a.since)).then(a.id.cmp(&b.id)));
    Ok(out)
}
