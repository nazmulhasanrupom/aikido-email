//! Account health: one score from 0 to 100 for each email account.
//!
//! - DNS health (0 to 100): MX 40, SPF 20, DKIM 20, DMARC 10, A 10. A check that is `ok` gives all its points,
//!   `warn` gives half, `fail` and `unknown` give none.
//! - Sending power (0 to 100): the mean of sending performance (the share of send attempts that went out) and
//!   bounce management (hard bounces, blocks and spam complaints). It uses the last 30 days, and needs at least
//!   `MIN_SENDS` sent emails. With fewer, it is `null` and the score is the DNS health only.
//! - Blacklists: the domain on domain lists, and the SMTP server's IP on IP lists. Every listing takes
//!   `LISTING_PENALTY` points off the score.
//!
//! The score is the mean of DNS health and sending power. "Ready to send" is DNS health of at least `READY_MIN`
//! and no listing.
//!
//! Honest limits: Gmail and Microsoft accounts send from the provider's servers, so only the domain is looked up.
//! Spamhaus refuses queries that come through big public DNS servers; we count such an answer as "did not answer",
//! never as "listed".
use super::dns_check::{Check, DnsReport, IpAnswer};
use super::model::{EmailAccount, Provider};
use crate::state::AppState;
use chrono::{DateTime, Duration, Utc};
use serde::Serialize;
use std::net::Ipv4Addr;
use utoipa::ToSchema;

pub const READY_MIN: i32 = 60;
pub const MIN_SENDS: i64 = 10;
pub const WINDOW_DAYS: i64 = 30;
pub const LISTING_PENALTY: i32 = 20;

/// Lists that take an IP (reversed, in front of the zone).
pub const IP_LISTS: [&str; 11] = [
    "zen.spamhaus.org",
    "bl.spamcop.net",
    "b.barracudacentral.org",
    "dnsbl.sorbs.net",
    "psbl.surriel.com",
    "bl.mailspike.net",
    "dnsbl-1.uceprotect.net",
    "ix.dnsbl.manitu.net",
    "dnsbl.dronebl.org",
    "db.wpbl.info",
    "all.s5h.net",
];
/// Lists that take a domain name.
pub const DOMAIN_LISTS: [&str; 4] = ["dbl.spamhaus.org", "multi.surbl.org", "multi.uribl.com", "uribl.spameatingmonkey.net"];

#[derive(Serialize, ToSchema, Clone, Debug)]
pub struct DnsItem {
    /// `mx`, `spf`, `dkim`, `dmarc` or `a`.
    pub key: String,
    pub label: String,
    /// The most points this record can give.
    pub points: i32,
    /// The points it gives now.
    pub earned: i32,
    pub check: Check,
}

#[derive(Serialize, ToSchema, Clone, Debug)]
pub struct DnsHealth {
    pub score: i32,
    pub items: Vec<DnsItem>,
}

#[derive(Serialize, ToSchema, Clone, Debug)]
pub struct Metric {
    pub score: i32,
    pub detail: String,
}

#[derive(Serialize, ToSchema, Clone, Debug)]
pub struct SendingPower {
    /// `null`: fewer than 10 emails sent in the last 30 days.
    pub score: Option<i32>,
    pub performance: Option<Metric>,
    pub bounces: Option<Metric>,
    /// The last 30 days.
    pub sent: i64,
    pub failed: i64,
    pub bounced: i64,
    pub complained: i64,
    pub first_sent_at: Option<DateTime<Utc>>,
    /// `perfect_setup`, `maturity`, `perfect_performance`.
    pub bonuses: Vec<String>,
}

#[derive(Serialize, ToSchema, Clone, Debug, PartialEq)]
pub struct Listing {
    pub list: String,
    /// The IP or domain that is on the list.
    pub target: String,
    /// The code the list answered.
    pub answer: String,
}

#[derive(Serialize, ToSchema, Clone, Debug)]
pub struct Blacklists {
    pub checked_at: DateTime<Utc>,
    /// The domain and IPs that were looked up.
    pub targets: Vec<String>,
    /// How many lookups were made.
    pub checked: i32,
    pub listed: Vec<Listing>,
    /// Lookups the list did not answer (timeout, or it refused the query).
    pub unknown: i32,
    /// Why some IPs were not looked up.
    pub note: Option<String>,
}

#[derive(Serialize, ToSchema, Clone, Debug)]
pub struct HealthReport {
    pub domain: String,
    pub checked_at: DateTime<Utc>,
    /// 0 to 100.
    pub score: i32,
    /// DNS health of at least `ready_min` and no blacklist listing.
    pub ready: bool,
    pub ready_min: i32,
    pub dns: DnsHealth,
    pub sending: SendingPower,
    pub blacklists: Blacklists,
}

fn earned(points: i32, c: &Check) -> i32 {
    match c.status.as_str() {
        "ok" => points,
        "warn" => points / 2,
        _ => 0,
    }
}

pub fn dns_health(r: &DnsReport) -> DnsHealth {
    let items: Vec<DnsItem> = [("mx", "MX record", 40, &r.mx), ("spf", "SPF record", 20, &r.spf), ("dkim", "DKIM record", 20, &r.dkim), ("dmarc", "DMARC record", 10, &r.dmarc), ("a", "A record", 10, &r.a)]
        .into_iter()
        .map(|(key, label, points, c)| DnsItem { key: key.into(), label: label.into(), points, earned: earned(points, c), check: c.clone() })
        .collect();
    DnsHealth { score: items.iter().map(|i| i.earned).sum(), items }
}

/// The share of send attempts that went out, as a score.
pub fn performance(sent: i64, failed: i64) -> Metric {
    let tries = sent + failed;
    let score = if tries == 0 { 100 } else { (sent as f64 * 100.0 / tries as f64).round() as i32 };
    Metric { score, detail: format!("{sent} of {tries} send attempts went out.") }
}

/// 100, minus 20 points per 1% of bounces and 100 points per 1% of spam complaints.
/// So 5% bounces, or 1% complaints, give 0. Google asks for complaints under 0.3%.
pub fn bounce_score(sent: i64, bounced: i64, complained: i64) -> Metric {
    if sent == 0 {
        return Metric { score: 100, detail: "Nothing sent yet.".into() };
    }
    let b = bounced as f64 * 100.0 / sent as f64;
    let c = complained as f64 * 100.0 / sent as f64;
    let score = (100.0 - b * 20.0 - c * 100.0).round().clamp(0.0, 100.0) as i32;
    Metric { score, detail: format!("{b:.1}% bounces, {c:.2}% spam complaints.") }
}

/// The score from its three parts.
pub fn total(dns: i32, sending: Option<i32>, listings: usize) -> i32 {
    let base = match sending {
        Some(s) => ((dns + s) as f64 / 2.0).round() as i32,
        None => dns,
    };
    (base - LISTING_PENALTY * listings as i32).max(0)
}

/// What a DNS blacklist answer means. `Some(code)`: listed. `None`: not listed, or the list refused to answer.
/// Lists answer 127.0.0.2 and up when a name is listed. 127.0.0.1, 127.255.255.x and x.x.x.255 are error codes
/// (for example "you asked through a public DNS server").
pub fn listing_code(ips: &[Ipv4Addr]) -> Option<Ipv4Addr> {
    ips.iter().copied().find(|ip| {
        let o = ip.octets();
        o[0] == 127 && *ip != Ipv4Addr::new(127, 0, 0, 1) && !(o[1] == 255 && o[2] == 255) && o[3] != 255
    })
}

fn is_public(ip: Ipv4Addr) -> bool {
    !(ip.is_private() || ip.is_loopback() || ip.is_link_local() || ip.is_unspecified() || ip.is_documentation() || ip.is_broadcast())
}

fn reversed(ip: Ipv4Addr) -> String {
    let o = ip.octets();
    format!("{}.{}.{}.{}", o[3], o[2], o[1], o[0])
}

/// Look up the domain and the IPs on the blacklists. Answers are kept for one hour.
async fn blacklists(st: &AppState, domain: &str, ips: &[Ipv4Addr], note: Option<String>, now: DateTime<Utc>) -> Blacklists {
    let key = format!("{domain}|{}", ips.iter().map(|i| i.to_string()).collect::<Vec<_>>().join(","));
    if let Some(b) = st.dns.blacklist_cache.lock().unwrap().get(&key) {
        if now - b.checked_at < Duration::minutes(super::dns_check::CACHE_MINS) {
            return b.clone();
        }
    }
    let mut queries: Vec<(String, String, String)> = DOMAIN_LISTS.iter().map(|l| (l.to_string(), domain.to_string(), format!("{domain}.{l}"))).collect();
    for ip in ips {
        queries.extend(IP_LISTS.iter().map(|l| (l.to_string(), ip.to_string(), format!("{}.{l}", reversed(*ip)))));
    }
    let resolver = st.dns.resolver();
    let answers = futures::future::join_all(queries.iter().map(|q| resolver.ipv4(&q.2))).await;
    let mut listed = vec![];
    let mut unknown = 0;
    for ((list, target, _), a) in queries.iter().zip(answers) {
        match a {
            IpAnswer::Ips(found) => match listing_code(&found) {
                Some(code) => listed.push(Listing { list: list.clone(), target: target.clone(), answer: code.to_string() }),
                None => unknown += 1,
            },
            IpAnswer::None => {}
            IpAnswer::Unknown => unknown += 1,
        }
    }
    let mut targets = vec![domain.to_string()];
    targets.extend(ips.iter().map(|i| i.to_string()));
    let b = Blacklists { checked_at: now, targets, checked: queries.len() as i32, listed, unknown, note };
    st.dns.blacklist_cache.lock().unwrap().insert(key, b.clone());
    b
}

/// The public IPs of the account's SMTP server, and a note when there are none to look up.
async fn sending_ips(st: &AppState, a: &EmailAccount) -> (Vec<Ipv4Addr>, Option<String>) {
    if a.provider != Provider::Smtp {
        let who = if a.provider == Provider::Gmail { "Google" } else { "Microsoft" };
        return (vec![], Some(format!("{who} sends this account's mail from its own servers. Only the domain is looked up.")));
    }
    let Some(host) = a.smtp_host.as_deref().map(str::trim).filter(|h| !h.is_empty()) else {
        return (vec![], Some("The account has no SMTP server. Only the domain is looked up.".into()));
    };
    if crate::sending::senders::smtp::is_local_host(host) {
        return (vec![], Some(format!("{host} is a local server. Only the domain is looked up.")));
    }
    let ips: Vec<Ipv4Addr> = match host.parse::<Ipv4Addr>() {
        Ok(ip) => vec![ip],
        Err(_) => match st.dns.resolver().ipv4(host).await {
            IpAnswer::Ips(ips) => ips,
            _ => return (vec![], Some(format!("The IP of {host} was not found. Only the domain is looked up."))),
        },
    };
    let public: Vec<Ipv4Addr> = ips.into_iter().filter(|i| is_public(*i)).take(3).collect();
    if public.is_empty() {
        return (vec![], Some(format!("{host} is a local server. Only the domain is looked up.")));
    }
    (public, None)
}

async fn sending_power(st: &AppState, a: &EmailAccount, dns_score: i32, now: DateTime<Utc>) -> sqlx::Result<SendingPower> {
    let since = now - Duration::days(WINDOW_DAYS);
    let (sent, failed, bounced): (i64, i64, i64) = sqlx::query_as(
        "SELECT count(*) FILTER (WHERE status = 'sent'),
                count(*) FILTER (WHERE status IN ('failed', 'stuck')),
                count(*) FILTER (WHERE status = 'sent' AND bounced_at IS NOT NULL AND bounce_kind IN ('hard', 'block'))
           FROM send_jobs WHERE account_id = $1 AND COALESCE(sent_at, updated_at) >= $2",
    )
    .bind(a.id)
    .bind(since)
    .fetch_one(&st.pool)
    .await?;
    let (complained,): (i64,) = sqlx::query_as(
        "SELECT count(DISTINCT lower(from_email::text)) FROM inbound_messages WHERE account_id = $1 AND label = 'complaint' AND received_at >= $2",
    )
    .bind(a.id)
    .bind(since)
    .fetch_one(&st.pool)
    .await?;
    let (first_sent_at,): (Option<DateTime<Utc>>,) =
        sqlx::query_as("SELECT min(sent_at) FROM send_jobs WHERE account_id = $1 AND status = 'sent'").bind(a.id).fetch_one(&st.pool).await?;
    let enough = sent >= MIN_SENDS;
    let performance = enough.then(|| performance(sent, failed));
    let bounces = enough.then(|| bounce_score(sent, bounced, complained));
    let score = match (&performance, &bounces) {
        (Some(p), Some(b)) => Some(((p.score + b.score) as f64 / 2.0).round() as i32),
        _ => None,
    };
    let mut bonuses = vec![];
    if dns_score == 100 {
        bonuses.push("perfect_setup".to_string());
    }
    if first_sent_at.is_some_and(|f| now - f >= Duration::days(WINDOW_DAYS)) {
        bonuses.push("maturity".to_string());
    }
    if score == Some(100) {
        bonuses.push("perfect_performance".to_string());
    }
    Ok(SendingPower { score, performance, bounces, sent, failed, bounced, complained, first_sent_at, bonuses })
}

pub async fn report(st: &AppState, a: &EmailAccount, selector: Option<&str>) -> sqlx::Result<HealthReport> {
    let now = st.now();
    let domain = a.email.rsplit('@').next().unwrap_or_default().trim().to_ascii_lowercase();
    let dns_report = st.dns.check(st.mx.as_ref(), &domain, a.provider.as_str(), selector, now).await;
    let dns = dns_health(&dns_report);
    let (ips, note) = sending_ips(st, a).await;
    let (sending, blacklists) = tokio::join!(sending_power(st, a, dns.score, now), blacklists(st, &domain, &ips, note, now));
    let sending = sending?;
    let score = total(dns.score, sending.score, blacklists.listed.len());
    let ready = dns.score >= READY_MIN && blacklists.listed.is_empty();
    Ok(HealthReport { domain: dns_report.domain, checked_at: now, score, ready, ready_min: READY_MIN, dns, sending, blacklists })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> Ipv4Addr {
        s.parse().unwrap()
    }

    #[test]
    fn a_list_code_counts_but_an_error_code_does_not() {
        assert_eq!(listing_code(&[ip("127.0.0.2")]), Some(ip("127.0.0.2")));
        assert_eq!(listing_code(&[ip("127.0.1.2")]), Some(ip("127.0.1.2")), "Spamhaus DBL spam domain");
        assert_eq!(listing_code(&[ip("127.0.0.1")]), None, "URIBL and SURBL: query refused");
        assert_eq!(listing_code(&[ip("127.255.255.254")]), None, "Spamhaus: public resolver");
        assert_eq!(listing_code(&[ip("127.0.1.255")]), None, "Spamhaus DBL: bad query");
        assert_eq!(listing_code(&[ip("93.184.216.34")]), None, "a wildcard answer is not a listing");
    }

    #[test]
    fn the_scores_follow_the_rules() {
        assert_eq!(performance(98, 2).score, 98);
        assert_eq!(bounce_score(100, 0, 0).score, 100);
        assert_eq!(bounce_score(100, 2, 0).score, 60, "2% bounces");
        assert_eq!(bounce_score(1000, 0, 3).score, 70, "0.3% complaints");
        assert_eq!(bounce_score(100, 10, 0).score, 0);
        assert_eq!(total(100, Some(80), 0), 90);
        assert_eq!(total(70, None, 0), 70, "no sending data: DNS only");
        assert_eq!(total(100, Some(100), 2), 60, "two listings");
        assert_eq!(total(10, None, 1), 0);
        assert_eq!(reversed(ip("1.2.3.4")), "4.3.2.1");
        assert!(!is_public(ip("10.0.0.5")) && is_public(ip("8.8.8.8")));
    }
}
