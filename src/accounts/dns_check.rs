//! Domain check (UI update map v2, I1 and B18): SPF, DKIM, DMARC and MX of the sending domain, in green, orange and red,
//! with the DNS record to add. Answers are kept for one hour.
//!
//! Honest limit: DKIM keys live at a selector the sender chose, and DNS can not list them. We look at the common
//! selectors and at one the user types. Not finding one is orange, not red, unless the user typed the selector.
use crate::leads::verify::{MxAnswer, MxResolver};
use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use serde::Serialize;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use utoipa::ToSchema;

pub const CACHE_MINS: i64 = 60;
const LOOKUP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(4);
/// Selectors that big mail providers and tools use.
pub const COMMON_SELECTORS: [&str; 9] = ["google", "selector1", "selector2", "default", "k1", "s1", "s2", "mail", "dkim"];

#[derive(Clone, Debug, PartialEq)]
pub enum TxtAnswer {
    Records(Vec<String>),
    /// The name has no TXT record (or does not exist).
    None,
    /// Timeout or DNS error: we do not know.
    Unknown,
}

#[derive(Clone, Debug, PartialEq)]
pub enum IpAnswer {
    Ips(Vec<std::net::Ipv4Addr>),
    /// The name has no A record (or does not exist).
    None,
    /// Timeout or DNS error: we do not know.
    Unknown,
}

#[async_trait]
pub trait TxtResolver: Send + Sync {
    async fn txt(&self, name: &str) -> TxtAnswer;
    /// The A records (IPv4) of a name. The health check uses it for the website and the blacklists.
    async fn ipv4(&self, name: &str) -> IpAnswer;
}

/// The real resolver: the system DNS settings. Made on first use.
#[derive(Default)]
pub struct SystemTxt {
    inner: tokio::sync::OnceCell<Option<hickory_resolver::TokioResolver>>,
}

impl SystemTxt {
    async fn resolver(&self) -> Option<&hickory_resolver::TokioResolver> {
        self.inner.get_or_init(|| async { hickory_resolver::Resolver::builder_tokio().and_then(|b| b.build()).ok() }).await.as_ref()
    }
}

#[async_trait]
impl TxtResolver for SystemTxt {
    async fn ipv4(&self, name: &str) -> IpAnswer {
        let Some(resolver) = self.resolver().await else { return IpAnswer::Unknown };
        let fqdn = format!("{name}.");
        match tokio::time::timeout(LOOKUP_TIMEOUT, resolver.ipv4_lookup(fqdn.as_str())).await {
            Ok(Ok(found)) => {
                let ips: Vec<std::net::Ipv4Addr> = found
                    .answers()
                    .iter()
                    .filter_map(|r| match &r.data {
                        hickory_resolver::proto::rr::RData::A(a) => Some(a.0),
                        _ => None,
                    })
                    .collect();
                if ips.is_empty() { IpAnswer::None } else { IpAnswer::Ips(ips) }
            }
            Ok(Err(e)) if e.is_nx_domain() || e.is_no_records_found() => IpAnswer::None,
            _ => IpAnswer::Unknown,
        }
    }

    async fn txt(&self, name: &str) -> TxtAnswer {
        let Some(resolver) = self.resolver().await else { return TxtAnswer::Unknown };
        let fqdn = format!("{name}.");
        match tokio::time::timeout(LOOKUP_TIMEOUT, resolver.txt_lookup(fqdn.as_str())).await {
            Ok(Ok(found)) => {
                let records: Vec<String> = found
                    .answers()
                    .iter()
                    .filter_map(|r| match &r.data {
                        hickory_resolver::proto::rr::RData::TXT(t) => Some(t.txt_data.iter().map(|d| String::from_utf8_lossy(d).into_owned()).collect::<String>()),
                        _ => None,
                    })
                    .collect();
                if records.is_empty() { TxtAnswer::None } else { TxtAnswer::Records(records) }
            }
            Ok(Err(e)) if e.is_nx_domain() || e.is_no_records_found() => TxtAnswer::None,
            _ => TxtAnswer::Unknown,
        }
    }
}

/// A resolver for tests: answers from a table.
pub mod fake {
    use super::*;

    #[derive(Default)]
    pub struct FakeTxt {
        table: Mutex<HashMap<String, Vec<String>>>,
        ips: Mutex<HashMap<String, Vec<std::net::Ipv4Addr>>>,
    }

    impl FakeTxt {
        pub fn new() -> Arc<Self> {
            Arc::new(Self::default())
        }
        pub fn set(&self, name: &str, records: &[&str]) {
            self.table.lock().unwrap().insert(name.to_string(), records.iter().map(|s| s.to_string()).collect());
        }
        /// The A records of a name. A name not set has none.
        pub fn set_ip(&self, name: &str, ips: &[&str]) {
            self.ips.lock().unwrap().insert(name.to_string(), ips.iter().map(|s| s.parse().unwrap()).collect());
        }
    }

    #[async_trait]
    impl TxtResolver for FakeTxt {
        async fn txt(&self, name: &str) -> TxtAnswer {
            match self.table.lock().unwrap().get(name) {
                Some(r) => TxtAnswer::Records(r.clone()),
                None => TxtAnswer::None,
            }
        }
        async fn ipv4(&self, name: &str) -> IpAnswer {
            match self.ips.lock().unwrap().get(name) {
                Some(r) => IpAnswer::Ips(r.clone()),
                None => IpAnswer::None,
            }
        }
    }
}

#[derive(Serialize, ToSchema, Clone, Debug, PartialEq)]
pub struct Check {
    /// `ok` (green), `warn` (orange), `fail` (red) or `unknown` (the DNS did not answer).
    pub status: String,
    pub summary: String,
    /// What we found.
    pub found: Option<String>,
    /// The DNS record to add: `TXT`, the host name, and the value.
    pub fix: Option<String>,
    /// DKIM: the selector where the key was found.
    pub selector: Option<String>,
}

#[derive(Serialize, ToSchema, Clone, Debug)]
pub struct DnsReport {
    pub domain: String,
    pub checked_at: DateTime<Utc>,
    pub spf: Check,
    pub dkim: Check,
    pub dmarc: Check,
    pub mx: Check,
    /// The A record: the domain points at a real website. Not needed to send, so it is not in `overall`.
    pub a: Check,
    /// The worst of SPF, DKIM, DMARC and MX: `ok`, `warn`, `fail` or `unknown`.
    pub overall: String,
}

pub struct DnsChecker {
    txt: Arc<dyn TxtResolver>,
    cache: Mutex<HashMap<(String, String), DnsReport>>,
    /// Blacklist answers by the names that were looked at. Kept for one hour too.
    pub(crate) blacklist_cache: Mutex<HashMap<String, super::health::Blacklists>>,
}

impl DnsChecker {
    pub fn new(txt: Arc<dyn TxtResolver>) -> Self {
        Self { txt, cache: Mutex::new(HashMap::new()), blacklist_cache: Mutex::new(HashMap::new()) }
    }
    pub fn resolver(&self) -> &dyn TxtResolver {
        self.txt.as_ref()
    }
}

impl Default for DnsChecker {
    fn default() -> Self {
        Self::new(Arc::new(SystemTxt::default()))
    }
}

fn check(status: &str, summary: impl Into<String>) -> Check {
    Check { status: status.into(), summary: summary.into(), found: None, fix: None, selector: None }
}

/// The SPF include that goes with a provider (`gmail`, `microsoft`, `smtp`).
fn spf_include(provider: &str) -> &'static str {
    match provider {
        "gmail" => "include:_spf.google.com",
        "microsoft" => "include:spf.protection.outlook.com",
        _ => "include:YOUR-MAIL-PROVIDER",
    }
}

fn spf(answer: &TxtAnswer, domain: &str, provider: &str) -> Check {
    let fix = format!("TXT @ \"v=spf1 {} ~all\" (host: {domain})", spf_include(provider));
    match answer {
        TxtAnswer::Unknown => check("unknown", "The DNS did not answer. Try again in a minute."),
        TxtAnswer::None => Check { fix: Some(fix), ..check("fail", "No SPF record. Mail may land in spam.") },
        TxtAnswer::Records(r) => {
            let spfs: Vec<&String> = r.iter().filter(|t| t.trim_start().to_ascii_lowercase().starts_with("v=spf1")).collect();
            match spfs.as_slice() {
                [] => Check { fix: Some(fix), ..check("fail", "No SPF record. Mail may land in spam.") },
                [one] => {
                    let l = one.to_ascii_lowercase();
                    let c = if l.contains("+all") || l.contains(" ?all") {
                        check("warn", "The SPF record allows every server. Use ~all or -all at the end.")
                    } else if !(l.contains("~all") || l.contains("-all") || l.contains("redirect=")) {
                        check("warn", "The SPF record has no ~all or -all at the end.")
                    } else {
                        check("ok", "SPF is set.")
                    };
                    Check { found: Some((*one).clone()), ..c }
                }
                _ => Check { found: Some(spfs.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(" | ")), fix: Some("Keep one SPF record and merge the includes into it.".into()), ..check("fail", "More than one SPF record. Receivers ignore all of them.") },
            }
        }
    }
}

fn dmarc(answer: &TxtAnswer, domain: &str) -> Check {
    let fix = format!("TXT _dmarc \"v=DMARC1; p=none; rua=mailto:postmaster@{domain}\" (host: _dmarc.{domain})");
    match answer {
        TxtAnswer::Unknown => check("unknown", "The DNS did not answer. Try again in a minute."),
        TxtAnswer::None => Check { fix: Some(fix), ..check("fail", "No DMARC record. Gmail and Yahoo ask for one.") },
        TxtAnswer::Records(r) => match r.iter().find(|t| t.trim_start().to_ascii_lowercase().starts_with("v=dmarc1")) {
            None => Check { fix: Some(fix), ..check("fail", "No DMARC record. Gmail and Yahoo ask for one.") },
            Some(rec) => {
                let l = rec.to_ascii_lowercase().replace(' ', "");
                let c = if !l.contains("p=") {
                    check("warn", "The DMARC record has no policy (p=).")
                } else if l.contains("p=none") {
                    check("ok", "DMARC is set (p=none: it only reports). Fine to start.")
                } else {
                    check("ok", "DMARC is set.")
                };
                Check { found: Some(rec.clone()), ..c }
            }
        },
    }
}

fn worst(checks: &[&Check]) -> String {
    for level in ["fail", "warn", "unknown"] {
        if checks.iter().any(|c| c.status == level) {
            return level.to_string();
        }
    }
    "ok".to_string()
}

fn a_check(a: &IpAnswer, domain: &str) -> Check {
    match a {
        IpAnswer::Ips(ips) => Check { found: Some(ips.iter().map(|i| i.to_string()).collect::<Vec<_>>().join(", ")), ..check("ok", "The domain points at a website.") },
        IpAnswer::None => Check {
            fix: Some(format!("Point {domain} at a website (an A record). A domain with no website looks new or fake to spam filters.")),
            ..check("warn", "The domain has no website (no A record).")
        },
        IpAnswer::Unknown => check("unknown", "The DNS did not answer. Try again in a minute."),
    }
}

fn mx_check(a: MxAnswer, domain: &str) -> Check {
    match a {
        MxAnswer::Has => check("ok", "The domain has a mail server."),
        MxAnswer::No => Check { fix: Some(format!("Add the MX records of your mail provider to {domain}.")), ..check("fail", "The domain has no mail server (MX). Replies can not arrive.") },
        MxAnswer::Unknown => check("unknown", "The DNS did not answer. Try again in a minute."),
    }
}

impl DnsChecker {
    /// Check a domain. `provider` is `gmail`, `microsoft` or `smtp`; `selector` is a DKIM selector the user typed.
    pub async fn check(&self, mx: &dyn MxResolver, domain: &str, provider: &str, selector: Option<&str>, now: DateTime<Utc>) -> DnsReport {
        let domain = domain.trim().trim_end_matches('.').to_ascii_lowercase();
        let typed = selector.map(str::trim).filter(|s| !s.is_empty()).map(str::to_ascii_lowercase);
        let key = (domain.clone(), format!("{provider}|{}", typed.clone().unwrap_or_default()));
        if let Some(r) = self.cache.lock().unwrap().get(&key) {
            if now - r.checked_at < Duration::minutes(CACHE_MINS) {
                return r.clone();
            }
        }
        let mut selectors: Vec<String> = typed.iter().cloned().collect();
        selectors.extend(COMMON_SELECTORS.iter().map(|s| s.to_string()).filter(|s| Some(s) != typed.as_ref()));
        let dmarc_name = format!("_dmarc.{domain}");
        let dkim_names: Vec<String> = selectors.iter().map(|s| format!("{s}._domainkey.{domain}")).collect();
        let (spf_a, dmarc_a, mx_a, a_a, dkim_a) = tokio::join!(
            self.txt.txt(&domain),
            self.txt.txt(&dmarc_name),
            mx.lookup(&domain),
            self.txt.ipv4(&domain),
            futures::future::join_all(dkim_names.iter().map(|n| self.txt.txt(n)))
        );
        // DKIM: the first selector with a key
        let mut dkim = None;
        let mut unknown = false;
        for (sel, a) in selectors.iter().zip(&dkim_a) {
            match a {
                TxtAnswer::Records(r) => {
                    if let Some(rec) = r.iter().find(|t| {
                        let l = t.to_ascii_lowercase();
                        l.contains("v=dkim1") || l.contains("p=")
                    }) {
                        dkim = Some(Check { found: Some(rec.chars().take(120).collect()), selector: Some(sel.clone()), ..check("ok", format!("DKIM key found at selector \"{sel}\".")) });
                        break;
                    }
                }
                TxtAnswer::Unknown => unknown = true,
                TxtAnswer::None => {}
            }
        }
        let dkim = dkim.unwrap_or_else(|| {
            if unknown {
                check("unknown", "The DNS did not answer. Try again in a minute.")
            } else if typed.is_some() {
                Check {
                    fix: Some(format!("Publish the DKIM key your mail provider gives you at {}._domainkey.{domain}.", typed.clone().unwrap_or_default())),
                    ..check("fail", "No DKIM key at the selector you typed.")
                }
            } else {
                let how = match provider {
                    "gmail" => "In Google Workspace Admin: Apps, Gmail, Authenticate email. Then add the TXT record it shows.",
                    "microsoft" => "In Microsoft 365 Defender: Email authentication, DKIM. Add the two CNAME records it shows.",
                    _ => "Ask your mail provider for the DKIM record and add it.",
                };
                Check { fix: Some(how.into()), ..check("warn", "No DKIM key at the common selectors. If yours has another name, type it and check again.") }
            }
        });
        let spf = spf(&spf_a, &domain, provider);
        let dmarc = dmarc(&dmarc_a, &domain);
        let mx = mx_check(mx_a, &domain);
        let a = a_check(&a_a, &domain);
        let overall = worst(&[&spf, &dkim, &dmarc, &mx]);
        let report = DnsReport { domain, checked_at: now, spf, dkim, dmarc, mx, a, overall };
        self.cache.lock().unwrap().insert(key, report.clone());
        report
    }
}
