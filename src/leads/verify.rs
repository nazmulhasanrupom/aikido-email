//! Email check. Syntax always runs (`email::normalize`). The MX check looks up the mail servers of a
//! domain: one lookup per domain, cached 7 days in `email_domains`, 3 seconds per lookup, 20 at a time.
//! A timeout or a DNS error is not "no MX": the lead stays `syntax_ok`.
use super::email;
use super::model::Verification;
use crate::state::AppState;
use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use futures::StreamExt;
use serde::Serialize;
use sqlx::PgPool;
use std::collections::{HashMap, HashSet};
use std::time::Duration as StdDuration;
use utoipa::ToSchema;
use uuid::Uuid;

pub const CACHE_DAYS: i64 = 7;
pub const LOOKUP_TIMEOUT: StdDuration = StdDuration::from_secs(3);
pub const PARALLEL_LOOKUPS: usize = 20;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MxAnswer {
    /// The domain has a mail server (an MX record, or an address record as a fallback).
    Has,
    /// The domain does not exist, or has no mail server.
    No,
    /// Timeout or DNS error: we do not know.
    Unknown,
}

/// Looks up the mail servers of a domain. Tests replace it with `fake::FakeMx` (`AppState::mx`).
#[async_trait]
pub trait MxResolver: Send + Sync {
    async fn lookup(&self, domain: &str) -> MxAnswer;
}

/// The real resolver: the system DNS settings (`/etc/resolv.conf`). Made on first use.
#[derive(Default)]
pub struct SystemResolver {
    inner: tokio::sync::OnceCell<Option<hickory_resolver::TokioResolver>>,
}

#[async_trait]
impl MxResolver for SystemResolver {
    async fn lookup(&self, domain: &str) -> MxAnswer {
        let resolver = self
            .inner
            .get_or_init(|| async {
                match hickory_resolver::Resolver::builder_tokio().and_then(|b| b.build()) {
                    Ok(r) => Some(r),
                    Err(e) => {
                        tracing::warn!(error = %e, "DNS resolver could not start: MX checks are off");
                        None
                    }
                }
            })
            .await;
        let Some(resolver) = resolver else { return MxAnswer::Unknown };
        match tokio::time::timeout(LOOKUP_TIMEOUT, lookup_with(resolver, domain)).await {
            Ok(a) => a,
            Err(_) => MxAnswer::Unknown,
        }
    }
}

async fn lookup_with(resolver: &hickory_resolver::TokioResolver, domain: &str) -> MxAnswer {
    let name = format!("{domain}.");
    match resolver.mx_lookup(name.as_str()).await {
        Ok(mx) => {
            // a lone "." target is a "null MX" (RFC 7505): the domain says it takes no mail
            if mx.answers().iter().any(|r| matches!(&r.data, hickory_resolver::proto::rr::RData::MX(m) if !m.exchange.is_root())) {
                return MxAnswer::Has;
            }
            MxAnswer::No
        }
        Err(e) if e.is_nx_domain() => MxAnswer::No,
        // no MX record: mail goes to the address record (RFC 5321, section 5.1)
        Err(e) if e.is_no_records_found() => match resolver.lookup_ip(name.as_str()).await {
            Ok(_) => MxAnswer::Has,
            Err(e) if e.is_nx_domain() || e.is_no_records_found() => MxAnswer::No,
            Err(e) => {
                tracing::debug!(error = %e, "address lookup failed");
                MxAnswer::Unknown
            }
        },
        Err(e) => {
            tracing::debug!(error = %e, "MX lookup failed");
            MxAnswer::Unknown
        }
    }
}

/// A resolver for tests: answers from a table, and remembers every lookup.
pub mod fake {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[derive(Clone)]
    pub struct FakeMx {
        answers: Arc<Mutex<HashMap<String, MxAnswer>>>,
        default: MxAnswer,
        calls: Arc<Mutex<Vec<String>>>,
    }

    impl FakeMx {
        /// Every domain not in the table gets `default`.
        pub fn new(default: MxAnswer) -> Self {
            Self { answers: Default::default(), default, calls: Default::default() }
        }
        pub fn set(&self, domain: &str, answer: MxAnswer) {
            self.answers.lock().unwrap().insert(domain.to_string(), answer);
        }
        /// The domains that were looked up, in order.
        pub fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl MxResolver for FakeMx {
        async fn lookup(&self, domain: &str) -> MxAnswer {
            self.calls.lock().unwrap().push(domain.to_string());
            self.answers.lock().unwrap().get(domain).copied().unwrap_or(self.default)
        }
    }
}

#[derive(Serialize, ToSchema, Default, Debug, PartialEq, Clone)]
pub struct VerifyReport {
    /// Domains with a mail server.
    pub mx_ok: i64,
    /// Domains with no mail server.
    pub no_mx: i64,
    /// Domains that could not be checked (timeout, DNS error, or out of time). Their leads keep their status.
    pub unchecked: i64,
    /// Leads whose verification status is different now (only set by the `verify` action).
    pub leads_changed: i64,
}

/// The answers for `domains` (lower case, no repeats). Uses the cache; looks up the rest, 20 at a time, and
/// saves the sure answers. With `force`, the cache is ignored. Stops looking up after `budget`: the domains
/// not reached are missing from the result. Settings `mx_check = false` makes it return nothing.
pub async fn check_domains(st: &AppState, domains: &[String], force: bool, budget: StdDuration) -> Result<HashMap<String, bool>, sqlx::Error> {
    let mut out: HashMap<String, bool> = HashMap::new();
    if domains.is_empty() {
        return Ok(out);
    }
    let now = st.now();
    if !force {
        let cached: Vec<(String, bool)> = sqlx::query_as("SELECT domain, has_mx FROM email_domains WHERE domain = ANY($1) AND checked_at > $2")
            .bind(domains)
            .bind(now - Duration::days(CACHE_DAYS))
            .fetch_all(&st.pool)
            .await?;
        out.extend(cached);
    }
    let todo: Vec<String> = domains.iter().filter(|d| !out.contains_key(*d)).cloned().collect();
    let deadline = tokio::time::Instant::now() + budget;
    let resolver = st.mx.clone();
    let mut answers = futures::stream::iter(todo)
        .map(move |d| {
            let resolver = resolver.clone();
            async move {
                let a = if tokio::time::Instant::now() >= deadline { MxAnswer::Unknown } else { resolver.lookup(&d).await };
                (d, a)
            }
        })
        .buffer_unordered(PARALLEL_LOOKUPS);
    let mut sure: Vec<(String, bool)> = Vec::new();
    while let Some((d, a)) = answers.next().await {
        match a {
            MxAnswer::Has => sure.push((d, true)),
            MxAnswer::No => sure.push((d, false)),
            MxAnswer::Unknown => {}
        }
    }
    if !sure.is_empty() {
        let (names, flags): (Vec<String>, Vec<bool>) = sure.iter().cloned().unzip();
        sqlx::query(
            "INSERT INTO email_domains (domain, has_mx, checked_at) SELECT d, h, $3 FROM unnest($1::text[], $2::bool[]) AS t(d, h)
             ON CONFLICT (domain) DO UPDATE SET has_mx = EXCLUDED.has_mx, checked_at = EXCLUDED.checked_at",
        )
        .bind(&names)
        .bind(&flags)
        .bind(now)
        .execute(&st.pool)
        .await?;
        out.extend(sure);
    }
    Ok(out)
}

/// Put the answers on the leads of those domains. A lead marked `invalid` stays `invalid`.
pub async fn apply(pool: &PgPool, answers: &HashMap<String, bool>) -> Result<(), sqlx::Error> {
    if answers.is_empty() {
        return Ok(());
    }
    let (names, flags): (Vec<String>, Vec<bool>) = answers.iter().map(|(d, h)| (d.clone(), *h)).unzip();
    sqlx::query(
        "UPDATE leads l SET verification = CASE WHEN t.h THEN 'mx_ok'::lead_verification ELSE 'no_mx'::lead_verification END
           FROM unnest($1::text[], $2::bool[]) AS t(d, h)
          WHERE l.email_domain = t.d AND l.verification <> 'invalid'
            AND l.verification IS DISTINCT FROM CASE WHEN t.h THEN 'mx_ok'::lead_verification ELSE 'no_mx'::lead_verification END",
    )
    .bind(&names)
    .bind(&flags)
    .execute(pool)
    .await?;
    Ok(())
}

/// Check the MX of these domains and update their leads. Does nothing if `mx_check` is off.
pub async fn verify_domains(st: &AppState, domains: &[String], force: bool, budget: StdDuration) -> Result<VerifyReport, sqlx::Error> {
    let settings = super::routes::load_settings(&st.pool).await?;
    let mut unique: Vec<String> = domains.iter().map(|d| d.to_lowercase()).collect::<HashSet<_>>().into_iter().collect();
    unique.sort();
    if !settings.mx_check || unique.is_empty() {
        return Ok(VerifyReport::default());
    }
    let answers = check_domains(st, &unique, force, budget).await?;
    apply(&st.pool, &answers).await?;
    let mx_ok = answers.values().filter(|h| **h).count() as i64;
    let no_mx = answers.len() as i64 - mx_ok;
    Ok(VerifyReport { mx_ok, no_mx, unchecked: unique.len() as i64 - answers.len() as i64, leads_changed: 0 })
}

/// The `verify` action: syntax first (a stored address that fails is marked `invalid`), then the MX check
/// of the domains. The cache is ignored, so a fixed DNS record is seen at once.
pub async fn verify_leads(st: &AppState, ids: &[Uuid], budget: StdDuration, now: DateTime<Utc>) -> Result<VerifyReport, sqlx::Error> {
    let before: Vec<(Uuid, Verification)> = sqlx::query_as("SELECT id, verification FROM leads WHERE id = ANY($1)").bind(ids).fetch_all(&st.pool).await?;
    let mut report = check_leads(st, ids, budget, now).await?;
    let after: HashMap<Uuid, Verification> = sqlx::query_as::<_, (Uuid, Verification)>("SELECT id, verification FROM leads WHERE id = ANY($1)").bind(ids).fetch_all(&st.pool).await?.into_iter().collect();
    report.leads_changed = before.iter().filter(|(id, v)| after.get(id).is_some_and(|a| a != v)).count() as i64;
    Ok(report)
}

async fn check_leads(st: &AppState, ids: &[Uuid], budget: StdDuration, now: DateTime<Utc>) -> Result<VerifyReport, sqlx::Error> {
    let rows: Vec<(Uuid, String, String)> = sqlx::query_as("SELECT id, email, email_domain FROM leads WHERE id = ANY($1)").bind(ids).fetch_all(&st.pool).await?;
    let bad: Vec<Uuid> = rows.iter().filter(|(_, e, _)| email::normalize(e).is_err()).map(|r| r.0).collect();
    if !bad.is_empty() {
        sqlx::query("UPDATE leads SET verification = $2, updated_at = $3 WHERE id = ANY($1)").bind(&bad).bind(Verification::Invalid).bind(now).execute(&st.pool).await?;
    }
    let bad: HashSet<Uuid> = bad.into_iter().collect();
    let domains: Vec<String> = rows.iter().filter(|(id, _, _)| !bad.contains(id)).map(|r| r.2.clone()).collect();
    verify_domains(st, &domains, true, budget).await
}
