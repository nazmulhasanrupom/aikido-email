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
}

pub type Clock = Arc<dyn Fn() -> DateTime<Utc> + Send + Sync>;

impl AppState {
    pub fn new(pool: PgPool, crypto: Crypto, config: Config) -> Self {
        let passwords = Passwords::new(&crypto);
        Self {
            pool,
            crypto: Arc::new(crypto),
            passwords,
            config: Arc::new(config),
            last_used: Arc::new(LastUsedTracker::default()),
            clock: Arc::new(Utc::now),
            mx: Arc::new(SystemResolver::default()),
            llm: Arc::new(RealLlmFactory::default()),
        }
    }

    pub fn now(&self) -> DateTime<Utc> {
        (self.clock)()
    }
}
