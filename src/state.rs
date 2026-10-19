use crate::auth::middleware::LastUsedTracker;
use crate::config::Config;
use crate::crypto::Crypto;
use crate::generator::llm::{LlmFactory, RealLlmFactory};
use crate::leads::verify::{MxResolver, SystemResolver};
use crate::users::password::Passwords;
use chrono::{DateTime, Utc};
use sqlx::PgPool;
use std::sync::Arc;

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
        }
    }

    pub fn now(&self) -> DateTime<Utc> {
        (self.clock)()
    }
}
