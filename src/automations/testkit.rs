//! Helpers for unit tests that need an `AppState` but no database (the pool never connects).
use crate::config::Config;
use crate::crypto::Crypto;
use crate::state::AppState;
use std::collections::HashMap;

pub fn config() -> Config {
    Config {
        database_url: String::new(),
        bind_addr: "127.0.0.1:0".into(),
        public_url: Some("http://localhost:8080".into()),
        data_dir: std::env::temp_dir().join(format!("aikido-unit-{}", uuid::Uuid::now_v7())),
        encryption_key: None,
        scheduler_tick_secs: 15,
        max_parallel_accounts: 20,
        dry_run: false,
        reply_tick_secs: 15,
        max_parallel_syncs: 10,
        max_message_bytes: 10_485_760,
        reply_check_enabled: false,
        followups_enabled: false,
        leads_enabled: false,
        generator_enabled: false,
        insecure_cookies: false,
        trust_proxy: false,
        tracking: Default::default(),
        dev_tools: true,
        env_api_keys: HashMap::new(),
    }
}

/// A state whose pool is lazy: it opens no connection until a query runs. Tests that use it must not run a query.
pub fn lazy_state() -> AppState {
    let pool = sqlx::postgres::PgPoolOptions::new().connect_lazy("postgres://nobody@127.0.0.1:1/none").expect("a lazy pool");
    AppState::new(pool, Crypto::from_key_bytes(&[7u8; 32]).unwrap(), config())
}
