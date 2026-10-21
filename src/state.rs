use crate::auth::middleware::LastUsedTracker;
use crate::config::Config;
use crate::crypto::Crypto;
use crate::generator::llm::{LlmFactory, RealLlmFactory};
use crate::leads::verify::{MxResolver, SystemResolver};
use crate::users::password::Passwords;
use chrono::{DateTime, Utc};
use sqlx::PgPool;
use std::sync::{Arc, RwLock};

/// Shared by every route and by the scheduler.
#[derive(Clone)]
pub struct AppState {
    pub pool: PgPool,
    pub crypto: Arc<Crypto>,
    pub config: Arc<Config>,
    pub last_used: Arc<LastUsedTracker>,
    /// The clock. Tests replace it with a fake clock.
    pub clock: Clock,
    /// Looks up MX records for the email check. Tests replace it with a fake.
    pub mx: Arc<dyn MxResolver>,
    /// Builds the client of an LLM provider. Tests replace it with a fake.
    pub llm: Arc<dyn LlmFactory>,
    /// Hashes and checks the passwords of web users.
    pub passwords: Arc<Passwords>,
    /// Looks up SPF, DKIM and DMARC for the domain check. Tests replace the resolver inside it.
    /// Builds the Reoon client for a key. Tests replace it with a fake.
    pub verifier: Arc<dyn crate::verify_reoon::client::VerifierFactory>,
    pub dns: Arc<crate::accounts::dns_check::DnsChecker>,
    /// Fetches portfolio pages for PERSONALIZATION. Tests replace it with a fetcher that has a fake resolver.
    pub fetcher: Arc<crate::net::safe_fetch::SafeFetcher>,
    /// Limits the routes that cost tokens (10 a minute per user).
    pub ai_limiter: Arc<crate::prompting::limit::RateLimiter>,
    /// Sends a test email at once, outside the queue. Tests replace it with a fake.
    pub senders: Arc<dyn crate::sending::worker::SenderFactory>,
    /// Looks up the CNAME of a tracking domain. Tests replace it with a fake.
    pub cname: Arc<dyn crate::tracking::dns::CnameResolver>,
    /// The certificates of the tracking domains, for the HTTPS listener.
    pub certs: Arc<crate::tracking::tls::CertStore>,
    /// The web address the browser last used (a copy of `app_state.site_url`). See `site_url`.
    pub learned_site_url: Arc<RwLock<Option<String>>>,
}

pub type Clock = Arc<dyn Fn() -> DateTime<Utc> + Send + Sync>;

impl AppState {
    pub fn new(pool: PgPool, crypto: Crypto, config: Config) -> Self {
        let passwords = Passwords::new(&crypto);
        let dry_run = config.dry_run;
        Self {
            pool,
            crypto: Arc::new(crypto),
            passwords,
            config: Arc::new(config),
            last_used: Arc::new(LastUsedTracker::default()),
            clock: Arc::new(Utc::now),
            mx: Arc::new(SystemResolver::default()),
            llm: Arc::new(RealLlmFactory::default()),
            verifier: Arc::new(crate::verify_reoon::client::RealVerifierFactory::default()),
            dns: Arc::new(crate::accounts::dns_check::DnsChecker::default()),
            fetcher: Arc::new(crate::net::safe_fetch::SafeFetcher::default()),
            ai_limiter: Arc::new(crate::prompting::limit::RateLimiter::default()),
            senders: Arc::new(crate::sending::worker::RealSenders { dry_run }),
            cname: Arc::new(crate::tracking::dns::SystemCname::default()),
            certs: Arc::new(crate::tracking::tls::CertStore::default()),
            learned_site_url: Arc::new(RwLock::new(None)),
        }
    }

    pub fn now(&self) -> DateTime<Utc> {
        (self.clock)()
    }

    /// The web address of the app, with no `/` at the end: `AIKIDO_PUBLIC_URL`, else the address
    /// the app was last opened at in a browser. `None` before anyone has used the app.
    pub fn site_url(&self) -> Option<String> {
        self.config.public_url.clone().or_else(|| self.learned_site_url.read().ok()?.clone())
    }

    /// Read the saved address at start.
    pub async fn load_site_url(&self) -> Result<(), sqlx::Error> {
        let saved: Option<String> = sqlx::query_scalar("SELECT site_url FROM app_state WHERE id = 1").fetch_one(&self.pool).await?;
        if let Ok(mut w) = self.learned_site_url.write() {
            *w = saved;
        }
        Ok(())
    }

    /// Keep the origin of a checked request from our own page (`https://host[:port]`).
    /// Does nothing when `AIKIDO_PUBLIC_URL` is set or the address is the same.
    pub async fn learn_site_url(&self, origin: &str) {
        if self.config.public_url.is_some() || !(origin.starts_with("https://") || origin.starts_with("http://")) {
            return;
        }
        if self.learned_site_url.read().ok().is_some_and(|u| u.as_deref() == Some(origin)) {
            return;
        }
        match sqlx::query("UPDATE app_state SET site_url = $1 WHERE id = 1").bind(origin).execute(&self.pool).await {
            Ok(_) => {
                if let Ok(mut w) = self.learned_site_url.write() {
                    *w = Some(origin.to_string());
                }
                tracing::info!(site_url = origin, "unsubscribe links now use this web address");
            }
            Err(e) => tracing::warn!(error = %e, "could not save the web address"),
        }
    }
}
