//! Reads env vars. Nothing else in the code reads the environment for engine settings.
use crate::auth::api_keys::Scope;
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Clone)]
pub struct Config {
    pub database_url: String,
    pub bind_addr: String,
    /// Base URL for OAuth redirect links. Needed only for the web OAuth flow.
    pub public_url: Option<String>,
    pub data_dir: PathBuf,
    /// `AIKIDO_ENCRYPTION_KEY` (base64). If empty, a key file in `data_dir` is used.
    pub encryption_key: Option<String>,
    pub scheduler_tick_secs: u64,
    pub max_parallel_accounts: usize,
    pub dry_run: bool,
    /// Reply check: the poller loop speed, in seconds.
    pub reply_tick_secs: u64,
    pub max_parallel_syncs: usize,
    /// Max size of one raw email we download.
    pub max_message_bytes: usize,
    /// `false` turns the reply poller off (useful in tests).
    pub reply_check_enabled: bool,
    /// `false` turns the follow-up scheduler off (useful in tests).
    pub followups_enabled: bool,
    /// Run the CSV import worker.
    pub leads_enabled: bool,
    /// `false` turns the AI generation worker off (this copy writes no AI text).
    pub generator_enabled: bool,
    /// `AIKIDO_INSECURE_COOKIES=true`: the session cookie works over plain HTTP on any host.
    /// Only for a trusted network. A warning is logged at every start.
    pub insecure_cookies: bool,
    /// `AIKIDO_TRUST_PROXY=true`: the client IP is the last `X-Forwarded-For` entry. Set it only behind your own proxy.
    pub trust_proxy: bool,
    /// Open tracking: the public tracking server and its certificates.
    pub tracking: TrackingConfig,
    /// Keys set by the operator (`AIKIDO_<SCOPE>_API_KEY`).
    pub env_api_keys: HashMap<Scope, String>,
}

pub const LETS_ENCRYPT: &str = "https://acme-v02.api.letsencrypt.org/directory";

/// Open tracking settings (`AIKIDO_TRACKING_*`, `AIKIDO_ACME_*`).
#[derive(Clone, Debug)]
pub struct TrackingConfig {
    /// `AIKIDO_TRACKING_CNAME_TARGET`: the host that the CNAME of a tracking domain must point to.
    /// Default: the host of `AIKIDO_PUBLIC_URL`.
    pub cname_target: Option<String>,
    /// `AIKIDO_TRACKING_HTTPS_ADDR` (for example `0.0.0.0:443`): a public HTTPS listener for the tracking domains.
    /// With it, the server gets and renews the certificates itself. Without it, your own reverse proxy must serve
    /// HTTPS for the tracking domains, and a domain is `connected` as soon as its CNAME is found.
    pub https_addr: Option<String>,
    /// `AIKIDO_TRACKING_HTTP_ADDR` (for example `0.0.0.0:80`): plain HTTP. Let's Encrypt checks a domain here (HTTP-01).
    pub http_addr: Option<String>,
    /// `AIKIDO_ACME_DIRECTORY`: Let's Encrypt production by default. Use the staging address for tests.
    pub acme_directory: String,
    /// `AIKIDO_ACME_EMAIL`: where Let's Encrypt writes about an expiring certificate. Optional.
    pub acme_email: Option<String>,
    /// `AIKIDO_TRACKING_ENABLED=false`: this copy does not check domains or renew certificates.
    pub enabled: bool,
    /// `AIKIDO_TRACKING_PROXY_CIDRS`: more address ranges (comma separated) that count as Apple Mail Privacy Protection.
    pub proxy_cidrs: Vec<String>,
}

impl Default for TrackingConfig {
    fn default() -> Self {
        Self { cname_target: None, https_addr: None, http_addr: None, acme_directory: LETS_ENCRYPT.into(), acme_email: None, enabled: true, proxy_cidrs: Vec::new() }
    }
}

// A hand-written Debug, so the keys never land in a log by accident.
impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Config")
            .field("bind_addr", &self.bind_addr)
            .field("public_url", &self.public_url)
            .field("data_dir", &self.data_dir)
            .field("scheduler_tick_secs", &self.scheduler_tick_secs)
            .field("max_parallel_accounts", &self.max_parallel_accounts)
            .field("dry_run", &self.dry_run)
            .field("reply_tick_secs", &self.reply_tick_secs)
            .field("max_parallel_syncs", &self.max_parallel_syncs)
            .finish_non_exhaustive()
    }
}

fn opt(name: &str) -> Option<String> {
    std::env::var(name).ok().map(|v| v.trim().to_string()).filter(|v| !v.is_empty())
}

fn parse_or<T: std::str::FromStr>(name: &str, default: T) -> anyhow::Result<T> {
    match opt(name) {
        None => Ok(default),
        Some(v) => v.parse().map_err(|_| anyhow::anyhow!("{name} has a bad value: {v}")),
    }
}

impl Config {
    pub fn from_env() -> anyhow::Result<Self> {
        let database_url = opt("DATABASE_URL")
            .ok_or_else(|| anyhow::anyhow!("DATABASE_URL is not set. The server cannot start without it."))?;
        let mut env_api_keys = HashMap::new();
        for scope in Scope::ALL {
            if let Some(v) = opt(&scope.env_var()) {
                env_api_keys.insert(*scope, v);
            }
        }
        Ok(Self {
            database_url,
            bind_addr: opt("AIKIDO_BIND_ADDR").unwrap_or_else(|| "0.0.0.0:8080".into()),
            public_url: opt("AIKIDO_PUBLIC_URL").map(|u| u.trim_end_matches('/').to_string()),
            data_dir: PathBuf::from(opt("AIKIDO_DATA_DIR").unwrap_or_else(|| "./data".into())),
            encryption_key: opt("AIKIDO_ENCRYPTION_KEY"),
            scheduler_tick_secs: parse_or("AIKIDO_SCHEDULER_TICK_SECS", 15)?,
            max_parallel_accounts: parse_or("AIKIDO_MAX_PARALLEL_ACCOUNTS", 20)?,
            dry_run: parse_or("AIKIDO_DRY_RUN", false)?,
            reply_tick_secs: parse_or("AIKIDO_REPLY_TICK_SECS", 15)?,
            max_parallel_syncs: parse_or("AIKIDO_MAX_PARALLEL_SYNCS", 10)?,
            max_message_bytes: parse_or("AIKIDO_MAX_MESSAGE_BYTES", 10_485_760)?,
            reply_check_enabled: parse_or("AIKIDO_REPLY_CHECK_ENABLED", true)?,
            followups_enabled: parse_or("AIKIDO_FOLLOWUPS_ENABLED", true)?,
            leads_enabled: parse_or("AIKIDO_LEADS_ENABLED", true)?,
            generator_enabled: parse_or("AIKIDO_GENERATOR_ENABLED", true)?,
            insecure_cookies: parse_or("AIKIDO_INSECURE_COOKIES", false)?,
            trust_proxy: parse_or("AIKIDO_TRUST_PROXY", false)?,
            tracking: TrackingConfig {
                cname_target: opt("AIKIDO_TRACKING_CNAME_TARGET").map(|h| h.trim_end_matches('.').to_ascii_lowercase()),
                https_addr: opt("AIKIDO_TRACKING_HTTPS_ADDR"),
                http_addr: opt("AIKIDO_TRACKING_HTTP_ADDR"),
                acme_directory: opt("AIKIDO_ACME_DIRECTORY").unwrap_or_else(|| LETS_ENCRYPT.into()),
                acme_email: opt("AIKIDO_ACME_EMAIL"),
                enabled: parse_or("AIKIDO_TRACKING_ENABLED", true)?,
                proxy_cidrs: opt("AIKIDO_TRACKING_PROXY_CIDRS").map(|v| v.split(',').map(|c| c.trim().to_string()).filter(|c| !c.is_empty()).collect()).unwrap_or_default(),
            },
            env_api_keys,
        })
    }
}

impl Config {
    /// The browser address of the app is plain HTTP on a host that is not this machine.
    /// A `Secure` cookie is not stored there, so the login would not work.
    pub fn plain_http_remote(&self) -> bool {
        let Some(url) = &self.public_url else { return false };
        let Some(rest) = url.strip_prefix("http://") else { return false };
        let host = rest.split(['/', ':']).next().unwrap_or("");
        let host = if rest.starts_with('[') { "[::1]" } else { host };
        !self.insecure_cookies && !matches!(host, "localhost" | "127.0.0.1" | "[::1]")
    }

    /// The host a tracking domain must point to: `AIKIDO_TRACKING_CNAME_TARGET`, else the host of `AIKIDO_PUBLIC_URL`.
    pub fn tracking_cname_target(&self) -> Option<String> {
        if let Some(t) = &self.tracking.cname_target {
            return Some(t.clone());
        }
        let rest = self.public_url.as_deref()?.split_once("://")?.1;
        let authority = rest.split(['/', '?', '#']).next()?;
        // an IP address cannot be the target of a CNAME
        let host = authority.split(':').next()?;
        (!host.is_empty() && !authority.starts_with('[') && host.parse::<std::net::Ipv4Addr>().is_err()).then(|| host.to_ascii_lowercase())
    }

    pub fn https(&self) -> bool {
        self.public_url.as_deref().is_some_and(|u| u.starts_with("https"))
    }
}
