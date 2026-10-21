//! Tracking domains: one per sending domain. `nazmul@acme.com` gives `acme.com`, and the tracker is served from
//! `media.acme.com`, a CNAME the user adds that points to this server.
//!
//! Status: `not_connected` (no CNAME), `checking` (CNAME found, certificate in progress), `connected` (ready) and
//! `error` (the CNAME was removed, or the certificate failed). A loop checks the DNS every 5 minutes for 72 hours
//! (`check_until`), a connected domain once a day, and renews a certificate 30 days before it ends.
use super::dns::{CnameAnswer, normalize};
use super::{CHECK_WINDOW_HOURS, CONNECTED_CHECK_HOURS, DNS_CHECK_MINS, RENEW_BEFORE_DAYS};
use crate::error::{ApiError, ApiResult};
use crate::state::AppState;
use chrono::{DateTime, Duration, Utc};
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, PgPool};
use utoipa::ToSchema;
use uuid::Uuid;

#[derive(sqlx::Type, Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, ToSchema)]
#[sqlx(type_name = "tracking_domain_status", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum Status {
    NotConnected,
    Checking,
    Connected,
    Error,
}

#[derive(FromRow, Debug, Clone)]
pub struct Row {
    pub id: Uuid,
    pub sending_domain: String,
    pub subdomain: String,
    pub cname_target: String,
    pub status: Status,
    pub cert_expires_at: Option<DateTime<Utc>>,
    pub last_checked_at: Option<DateTime<Utc>>,
    pub error_message: Option<String>,
    pub check_until: DateTime<Utc>,
    pub cert_attempts: i32,
    pub next_cert_attempt_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub has_cert: bool,
}

pub const COLS: &str = "id, sending_domain, subdomain, cname_target, status, cert_expires_at, last_checked_at, error_message, check_until,
    cert_attempts, next_cert_attempt_at, created_at, (cert IS NOT NULL AND cert_key IS NOT NULL) AS has_cert";

impl Row {
    pub fn host(&self) -> String {
        format!("{}.{}", self.subdomain, self.sending_domain)
    }
}

/// The record the user adds in their DNS provider.
#[derive(Serialize, ToSchema, Clone, Debug)]
pub struct DnsRecord {
    /// `CNAME`
    #[serde(rename = "type")]
    pub kind: String,
    /// For example `media`.
    pub name: String,
    /// The Aikido server host. `null` when the server does not know it yet: set `AIKIDO_TRACKING_CNAME_TARGET`.
    pub value: Option<String>,
}

#[derive(Serialize, ToSchema, Clone, Debug)]
pub struct DomainView {
    pub id: Uuid,
    pub sending_domain: String,
    pub subdomain: String,
    /// The tracking host, for example `media.acme.com`.
    pub host: String,
    pub status: Status,
    pub record: DnsRecord,
    pub last_checked_at: Option<DateTime<Utc>>,
    /// Why the domain is not connected: what the DNS check saw, or why the certificate failed.
    pub error_message: Option<String>,
    pub cert_expires_at: Option<DateTime<Utc>>,
    /// The 5 minute DNS check ran for 72 hours and found nothing. It stops until "Check now".
    pub not_found: bool,
}

pub fn view(st: &AppState, r: &Row) -> DomainView {
    let value = st.config.tracking_cname_target().or_else(|| Some(r.cname_target.clone()).filter(|t| !t.is_empty()));
    DomainView {
        id: r.id,
        sending_domain: r.sending_domain.clone(),
        subdomain: r.subdomain.clone(),
        host: r.host(),
        status: r.status,
        record: DnsRecord { kind: "CNAME".into(), name: r.subdomain.clone(), value },
        last_checked_at: r.last_checked_at,
        error_message: r.error_message.clone(),
        cert_expires_at: r.cert_expires_at,
        not_found: matches!(r.status, Status::NotConnected | Status::Error) && st.now() >= r.check_until,
    }
}

// ---------- names ----------

pub fn domain_of_email(email: &str) -> Option<String> {
    let d = email.rsplit_once('@')?.1.trim().trim_end_matches('.').to_ascii_lowercase();
    (!d.is_empty() && d.contains('.')).then_some(d)
}

/// Names the user may not pick: they look like tracking to a spam filter, or they would break the mail of the domain.
const BLOCKED_EXACT: [&str; 5] = ["track", "tracking", "click", "open", "pixel"];
const BLOCKED_WORDS: [&str; 3] = ["track", "click", "pixel"];
const RESERVED: [&str; 10] = ["www", "mail", "smtp", "imap", "pop", "pop3", "mx", "autodiscover", "autoconfig", "webmail"];

pub fn validate_subdomain(s: &str) -> Result<String, String> {
    let s = s.trim().to_ascii_lowercase();
    if s.is_empty() || s.len() > 40 {
        return Err("The name must have 1 to 40 characters.".into());
    }
    if !s.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-') || s.starts_with('-') || s.ends_with('-') {
        return Err("Use only letters, digits and hyphens (one name, no dots). It cannot start or end with a hyphen.".into());
    }
    if BLOCKED_EXACT.contains(&s.as_str()) || BLOCKED_WORDS.iter().any(|w| s.contains(w)) {
        return Err("This name looks like tracking to a spam filter. Use a plain name such as media, img, assets or files.".into());
    }
    if RESERVED.contains(&s.as_str()) {
        return Err("This name is often used for the mail of the domain. Pick another, such as media, img, assets or files.".into());
    }
    Ok(s)
}

// ---------- store ----------

/// Make a row for every sending domain that has none yet. The domains come from the connected accounts.
pub async fn sync(st: &AppState) -> Result<(), sqlx::Error> {
    let now = st.now();
    let target = st.config.tracking_cname_target().unwrap_or_default();
    let domains: Vec<(String,)> = sqlx::query_as(
        "SELECT DISTINCT lower(split_part(email::text, '@', 2)) FROM email_accounts WHERE deleted_at IS NULL AND split_part(email::text, '@', 2) LIKE '%.%'",
    )
    .fetch_all(&st.pool)
    .await?;
    for (d,) in domains {
        sqlx::query(
            "INSERT INTO tracking_domains (id, sending_domain, cname_target, check_until, created_at) VALUES ($1, $2, $3, $4, $5) ON CONFLICT (sending_domain) DO NOTHING",
        )
        .bind(Uuid::now_v7())
        .bind(d)
        .bind(&target)
        .bind(now + Duration::hours(CHECK_WINDOW_HOURS))
        .bind(now)
        .execute(&st.pool)
        .await?;
    }
    Ok(())
}

pub async fn list(st: &AppState) -> Result<Vec<Row>, sqlx::Error> {
    sqlx::query_as(&format!("SELECT {COLS} FROM tracking_domains ORDER BY sending_domain")).fetch_all(&st.pool).await
}

pub async fn get(pool: &PgPool, id: Uuid) -> Result<Option<Row>, sqlx::Error> {
    sqlx::query_as(&format!("SELECT {COLS} FROM tracking_domains WHERE id = $1")).bind(id).fetch_optional(pool).await
}

pub async fn by_sending_domain(pool: &PgPool, domain: &str) -> Result<Option<Row>, sqlx::Error> {
    sqlx::query_as(&format!("SELECT {COLS} FROM tracking_domains WHERE sending_domain = $1")).bind(domain).fetch_optional(pool).await
}

/// The domains of the accounts that a campaign sends from.
pub async fn of_campaign(pool: &PgPool, campaign_id: Uuid) -> Result<Vec<Row>, sqlx::Error> {
    sqlx::query_as(&format!(
        "SELECT {COLS} FROM tracking_domains WHERE sending_domain IN (
            SELECT lower(split_part(a.email::text, '@', 2)) FROM campaign_accounts ca JOIN email_accounts a ON a.id = ca.account_id
             WHERE ca.campaign_id = $1 AND a.deleted_at IS NULL) ORDER BY sending_domain"
    ))
    .bind(campaign_id)
    .fetch_all(pool)
    .await
}

/// Change the subdomain. A new name needs a new CNAME and a new certificate, so the row starts over.
pub async fn set_subdomain(st: &AppState, id: Uuid, name: &str) -> ApiResult<Row> {
    let name = validate_subdomain(name).map_err(|m| ApiError::unprocessable("invalid_subdomain", m))?;
    let now = st.now();
    let Some(row) = get(&st.pool, id).await? else { return Err(ApiError::not_found("not_found", "No tracking domain with this id.")) };
    if row.subdomain != name {
        sqlx::query(
            "UPDATE tracking_domains SET subdomain = $2, status = 'not_connected', cert = NULL, cert_key = NULL, cert_expires_at = NULL, error_message = NULL,
                    last_checked_at = NULL, check_until = $3, cert_attempts = 0, next_cert_attempt_at = NULL WHERE id = $1",
        )
        .bind(id)
        .bind(&name)
        .bind(now + Duration::hours(CHECK_WINDOW_HOURS))
        .execute(&st.pool)
        .await?;
        let _ = st.certs.refresh(st).await;
    }
    get(&st.pool, id).await?.ok_or_else(ApiError::internal)
}

/// "Check now": look at the DNS at once, and start the 72 hour window again. A certificate is asked for in the background.
pub async fn check_now(st: &AppState, id: Uuid) -> ApiResult<Row> {
    if get(&st.pool, id).await?.is_none() {
        return Err(ApiError::not_found("not_found", "No tracking domain with this id."));
    }
    let now = st.now();
    sqlx::query("UPDATE tracking_domains SET check_until = $2, cert_attempts = 0, next_cert_attempt_at = NULL WHERE id = $1")
        .bind(id)
        .bind(now + Duration::hours(CHECK_WINDOW_HOURS))
        .execute(&st.pool)
        .await?;
    let dns_only = Step::DnsOnly;
    advance(st, id, dns_only).await?;
    let row = get(&st.pool, id).await?.ok_or_else(ApiError::internal)?;
    if row.status == Status::Checking {
        let st2 = st.clone();
        tokio::spawn(async move {
            if let Err(e) = advance(&st2, id, Step::Full).await {
                tracing::warn!(domain_id = %id, error = %format!("{e:#}"), "certificate step failed");
            }
        });
    }
    Ok(row)
}

// ---------- the check ----------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Step {
    /// Look at the DNS and set the status. Do not ask for a certificate (the request of "Check now" must be fast).
    DnsOnly,
    /// DNS, then a certificate if one is needed.
    Full,
}

#[derive(Debug, PartialEq)]
pub enum Verdict {
    Found,
    Missing(String),
    Unknown,
}

pub fn verdict(answer: &CnameAnswer, host: &str, target: &str) -> Verdict {
    let target = normalize(target);
    match answer {
        CnameAnswer::Unknown => Verdict::Unknown,
        CnameAnswer::Targets(t) if t.iter().any(|x| *x == target) => Verdict::Found,
        CnameAnswer::Targets(t) => Verdict::Missing(format!("{host} points to {}, not to {target}. Change the CNAME record.", t.first().cloned().unwrap_or_default())),
        CnameAnswer::None => Verdict::Missing(format!("No CNAME record found for {host} yet. DNS changes can take a while to spread.")),
    }
}

/// Does this certificate need to be asked for (again)?
pub fn needs_cert(has_cert: bool, expires: Option<DateTime<Utc>>, now: DateTime<Utc>) -> bool {
    !has_cert || expires.is_none_or(|e| e <= now + Duration::days(RENEW_BEFORE_DAYS))
}

pub fn cert_valid(has_cert: bool, expires: Option<DateTime<Utc>>, now: DateTime<Utc>) -> bool {
    has_cert && expires.is_some_and(|e| e > now)
}

/// Wait before the next certificate try: 1 hour, then 2, 4 ... up to 24.
pub fn retry_wait(attempts: i32) -> Duration {
    Duration::hours(1i64 << (attempts.clamp(1, 6) - 1)).min(Duration::hours(24))
}

/// Do the check of one domain. One server copy at a time (a lease on the row).
pub async fn advance(st: &AppState, id: Uuid, step: Step) -> anyhow::Result<()> {
    let now = st.now();
    let got: Option<(Uuid,)> = sqlx::query_as("UPDATE tracking_domains SET lease_until = $2 WHERE id = $1 AND (lease_until IS NULL OR lease_until < $3) RETURNING id")
        .bind(id)
        .bind(now + Duration::minutes(10))
        .bind(now)
        .fetch_optional(&st.pool)
        .await?;
    if got.is_none() {
        return Ok(());
    }
    let result = advance_inner(st, id, step).await;
    sqlx::query("UPDATE tracking_domains SET lease_until = NULL WHERE id = $1").bind(id).execute(&st.pool).await?;
    result
}

async fn advance_inner(st: &AppState, id: Uuid, step: Step) -> anyhow::Result<()> {
    let now = st.now();
    let Some(row) = get(&st.pool, id).await? else { return Ok(()) };
    let host = row.host();
    let target = st.config.tracking_cname_target().unwrap_or_else(|| row.cname_target.clone());
    if target.is_empty() {
        sqlx::query("UPDATE tracking_domains SET last_checked_at = $2, error_message = $3 WHERE id = $1")
            .bind(id)
            .bind(now)
            .bind("The server does not know its own host yet. Set AIKIDO_TRACKING_CNAME_TARGET (or AIKIDO_PUBLIC_URL).")
            .execute(&st.pool)
            .await?;
        return Ok(());
    }
    if target != row.cname_target {
        sqlx::query("UPDATE tracking_domains SET cname_target = $2 WHERE id = $1").bind(id).bind(&target).execute(&st.pool).await?;
    }
    let answer = st.cname.cname(&host).await;
    tracing::debug!(%host, ?answer, status = ?row.status, "tracking domain checked");
    match verdict(&answer, &host, &target) {
        Verdict::Unknown => {
            sqlx::query("UPDATE tracking_domains SET last_checked_at = $2 WHERE id = $1").bind(id).bind(now).execute(&st.pool).await?;
        }
        Verdict::Missing(msg) => {
            // a domain that was working (or on its way) turns red; one that never worked stays red or not connected
            let (status, window) = match row.status {
                Status::Connected | Status::Checking => (Status::Error, Some(now + Duration::hours(CHECK_WINDOW_HOURS))),
                Status::Error => (Status::Error, None),
                Status::NotConnected => (Status::NotConnected, None),
            };
            let msg = if matches!(row.status, Status::Connected | Status::Checking) { format!("The CNAME record was removed or changed. {msg}") } else { msg };
            if status != row.status {
                tracing::warn!(%host, from = ?row.status, to = ?status, reason = %msg, "tracking domain status changed");
            }
            sqlx::query("UPDATE tracking_domains SET status = $2, error_message = $3, last_checked_at = $4, check_until = COALESCE($5, check_until) WHERE id = $1")
                .bind(id)
                .bind(status)
                .bind(msg)
                .bind(now)
                .bind(window)
                .execute(&st.pool)
                .await?;
        }
        Verdict::Found => found(st, &row, &host, step, now).await?,
    }
    Ok(())
}

async fn found(st: &AppState, row: &Row, host: &str, step: Step, now: DateTime<Utc>) -> anyhow::Result<()> {
    let id = row.id;
    let managed = st.config.tracking.https_addr.is_some();
    if !managed {
        // your own reverse proxy serves HTTPS: nothing for us to wait for
        if row.status != Status::Connected {
            tracing::info!(%host, "tracking domain connected (CNAME found, no certificate needed here)");
        }
        sqlx::query("UPDATE tracking_domains SET status = 'connected', error_message = NULL, last_checked_at = $2, cert_attempts = 0 WHERE id = $1").bind(id).bind(now).execute(&st.pool).await?;
        return Ok(());
    }
    let valid = cert_valid(row.has_cert, row.cert_expires_at, now);
    if !needs_cert(row.has_cert, row.cert_expires_at, now) {
        sqlx::query("UPDATE tracking_domains SET status = 'connected', error_message = NULL, last_checked_at = $2, cert_attempts = 0 WHERE id = $1").bind(id).bind(now).execute(&st.pool).await?;
        return Ok(());
    }
    let must_wait = row.next_cert_attempt_at.is_some_and(|t| t > now);
    if must_wait || step == Step::DnsOnly {
        // A usable certificate keeps the domain green while a renewal waits. No certificate: yellow, or red after a failure.
        let status = if valid { Status::Connected } else if row.status == Status::Error { Status::Error } else { Status::Checking };
        sqlx::query("UPDATE tracking_domains SET status = $2, last_checked_at = $3 WHERE id = $1").bind(id).bind(status).bind(now).execute(&st.pool).await?;
        return Ok(());
    }
    if !valid {
        sqlx::query("UPDATE tracking_domains SET status = 'checking', error_message = NULL, last_checked_at = $2 WHERE id = $1").bind(id).bind(now).execute(&st.pool).await?;
    }
    match super::acme::issue(st, host).await {
        Ok(cert) => {
            let sealed_cert = super::seal(&st.crypto, cert.chain_pem.as_bytes(), id.as_bytes()).map_err(anyhow::Error::msg)?;
            let sealed_key = super::seal(&st.crypto, cert.key_pem.as_bytes(), id.as_bytes()).map_err(anyhow::Error::msg)?;
            sqlx::query(
                "UPDATE tracking_domains SET status = 'connected', cert = $2, cert_key = $3, cert_expires_at = $4, error_message = NULL, cert_attempts = 0,
                        next_cert_attempt_at = NULL, last_checked_at = $5 WHERE id = $1",
            )
            .bind(id)
            .bind(sealed_cert)
            .bind(sealed_key)
            .bind(cert.expires_at)
            .bind(now)
            .execute(&st.pool)
            .await?;
            if let Err(e) = st.certs.refresh(st).await {
                tracing::warn!(error = %format!("{e:#}"), "could not load the new certificate into the HTTPS listener");
            }
            tracing::info!(%host, expires = %cert.expires_at, "tracking certificate saved");
        }
        Err(e) => {
            let attempts = row.cert_attempts + 1;
            let msg = format!("The HTTPS certificate failed: {e}");
            tracing::warn!(%host, attempts, error = %e, "tracking certificate failed");
            // a renewal that fails keeps the domain green while the old certificate lasts
            let status = if valid { Status::Connected } else { Status::Error };
            sqlx::query("UPDATE tracking_domains SET status = $2, error_message = $3, cert_attempts = $4, next_cert_attempt_at = $5, last_checked_at = $6 WHERE id = $1")
                .bind(id)
                .bind(status)
                .bind(msg)
                .bind(attempts)
                .bind(now + retry_wait(attempts))
                .bind(now)
                .execute(&st.pool)
                .await?;
        }
    }
    Ok(())
}

/// The domains that are due for a check now.
pub async fn due(pool: &PgPool, now: DateTime<Utc>) -> Result<Vec<Uuid>, sqlx::Error> {
    let rows: Vec<(Uuid,)> = sqlx::query_as(
        "SELECT id FROM tracking_domains
          WHERE (lease_until IS NULL OR lease_until < $1) AND (
                (status IN ('not_connected', 'error') AND $1 < check_until AND (last_checked_at IS NULL OR last_checked_at < $1 - make_interval(mins => $2)))
             OR (status = 'checking' AND (last_checked_at IS NULL OR last_checked_at < $1 - make_interval(mins => $2)))
             OR (status = 'connected' AND (last_checked_at IS NULL OR last_checked_at < $1 - make_interval(hours => $3))))
          ORDER BY last_checked_at NULLS FIRST LIMIT 200",
    )
    .bind(now)
    .bind(DNS_CHECK_MINS as i32)
    .bind(CONNECTED_CHECK_HOURS as i32)
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(|r| r.0).collect())
}

/// One round: make rows for new domains, then check what is due. Four domains at a time (a certificate takes a while).
pub async fn tick(st: &AppState) -> anyhow::Result<usize> {
    sync(st).await?;
    let ids = due(&st.pool, st.now()).await?;
    let n = ids.len();
    futures::stream::iter(ids)
        .for_each_concurrent(4, |id| async move {
            if let Err(e) = advance(st, id, Step::Full).await {
                tracing::warn!(domain_id = %id, error = %format!("{e:#}"), "tracking domain check failed");
            }
        })
        .await;
    Ok(n)
}

pub async fn run(st: AppState, cancel: tokio_util::sync::CancellationToken) {
    // the certificates that are already saved
    if let Err(e) = st.certs.refresh(&st).await {
        tracing::warn!(error = %format!("{e:#}"), "could not load the saved tracking certificates");
    }
    loop {
        if let Err(e) = tick(&st).await {
            tracing::warn!(error = %format!("{e:#}"), "tracking domain round failed");
        }
        tokio::select! {
            _ = cancel.cancelled() => return,
            _ = tokio::time::sleep(std::time::Duration::from_secs(30)) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn the_domain_comes_from_the_email() {
        assert_eq!(domain_of_email("Nazmul@Acme.COM").as_deref(), Some("acme.com"));
        assert_eq!(domain_of_email("a@mail.acme.co.uk").as_deref(), Some("mail.acme.co.uk"));
        assert_eq!(domain_of_email("nodomain"), None);
        assert_eq!(domain_of_email("a@localhost"), None);
    }

    #[test]
    fn subdomain_names_are_checked() {
        for ok in ["media", "img", "Assets", "files", "cdn-2", "m1"] {
            assert!(validate_subdomain(ok).is_ok(), "{ok}");
        }
        assert_eq!(validate_subdomain(" Media ").unwrap(), "media");
        for bad in ["track", "tracking", "click", "open", "pixel", "tracker", "clicks", "my-pixel", "", "-a", "a-", "a.b", "a b", "a_b", "mail", "www", &"x".repeat(41)] {
            assert!(validate_subdomain(bad).is_err(), "{bad}");
        }
        // "open" is only blocked as a whole name
        assert!(validate_subdomain("opendoor").is_ok());
    }

    #[test]
    fn the_verdict_compares_the_cname_target() {
        let host = "media.acme.com";
        assert_eq!(verdict(&CnameAnswer::Targets(vec!["app.aikido.io".into()]), host, "App.Aikido.io."), Verdict::Found);
        assert!(matches!(verdict(&CnameAnswer::Targets(vec!["other.com".into()]), host, "app.aikido.io"), Verdict::Missing(m) if m.contains("other.com")));
        assert!(matches!(verdict(&CnameAnswer::None, host, "app.aikido.io"), Verdict::Missing(_)));
        assert_eq!(verdict(&CnameAnswer::Unknown, host, "app.aikido.io"), Verdict::Unknown);
    }

    #[test]
    fn a_certificate_is_renewed_30_days_before_it_ends() {
        let now = Utc.with_ymd_and_hms(2026, 10, 21, 0, 0, 0).unwrap();
        assert!(needs_cert(false, None, now));
        assert!(!needs_cert(true, Some(now + Duration::days(31)), now));
        assert!(needs_cert(true, Some(now + Duration::days(29)), now));
        assert!(cert_valid(true, Some(now + Duration::days(1)), now));
        assert!(!cert_valid(true, Some(now - Duration::days(1)), now));
        assert!(!cert_valid(false, Some(now + Duration::days(1)), now));
    }

    #[test]
    fn the_wait_between_certificate_tries_grows_to_a_day() {
        assert_eq!(retry_wait(1), Duration::hours(1));
        assert_eq!(retry_wait(2), Duration::hours(2));
        assert_eq!(retry_wait(3), Duration::hours(4));
        assert_eq!(retry_wait(6), Duration::hours(24));
        assert_eq!(retry_wait(20), Duration::hours(24));
    }
}
