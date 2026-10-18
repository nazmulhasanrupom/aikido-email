//! Workspace settings of the verifier, stored in `verify_defaults` (one row). They outlive an uninstall.
use super::client::Mode;
use serde::{Deserialize, Serialize};
use sqlx::PgExecutor;
use utoipa::ToSchema;

/// Statuses the send rule can never allow: sending to them hurts the inbox.
pub const LOCKED: &[&str] = &["invalid", "disabled", "disposable", "spamtrap"];

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct SendRule {
    /// Reoon statuses (lower case) that may start a campaign.
    pub allowed: Vec<String>,
    /// Leads with no result yet.
    pub allow_not_verified: bool,
}

impl Default for SendRule {
    fn default() -> Self {
        Self { allowed: vec!["safe".into(), "valid".into()], allow_not_verified: true }
    }
}

impl SendRule {
    /// The first locked status in the rule, if any.
    pub fn locked_status(&self) -> Option<&str> {
        self.allowed.iter().map(String::as_str).find(|s| LOCKED.contains(s))
    }
    pub fn normalized(mut self) -> Self {
        for s in &mut self.allowed {
            *s = s.trim().to_ascii_lowercase();
        }
        self.allowed.retain(|s| !s.is_empty());
        self.allowed.sort();
        self.allowed.dedup();
        self
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct VerifySettings {
    /// `power` (checks the inbox, slow) or `quick`.
    pub mode: Mode,
    /// Parallel Reoon calls. 1 to 20.
    pub concurrency: i32,
    /// A result newer than this many days is reused. 0 to 365.
    pub reuse_days: i32,
    /// Seconds between worker ticks. 1 to 300.
    pub tick_secs: i32,
    pub send_rule: SendRule,
    /// Verify new leads at once, when they come from a CSV import, the API or a manual add. Off by default.
    pub auto_verify: bool,
}

impl Default for VerifySettings {
    fn default() -> Self {
        Self { mode: Mode::Power, concurrency: 5, reuse_days: 30, tick_secs: 5, send_rule: SendRule::default(), auto_verify: false }
    }
}

#[derive(Debug)]
pub struct SettingsError {
    pub code: &'static str,
    pub message: String,
}

impl VerifySettings {
    pub fn from_stored(v: serde_json::Value) -> Self {
        serde_json::from_value(v).unwrap_or_default()
    }

    pub fn validate(&self) -> Result<(), SettingsError> {
        let bad = |m: &str| Err(SettingsError { code: "invalid_settings", message: m.into() });
        if !(1..=20).contains(&self.concurrency) {
            return bad("concurrency must be 1 to 20.");
        }
        if !(0..=365).contains(&self.reuse_days) {
            return bad("reuse_days must be 0 to 365.");
        }
        if !(1..=300).contains(&self.tick_secs) {
            return bad("tick_secs must be 1 to 300.");
        }
        if let Some(s) = self.send_rule.locked_status() {
            return Err(SettingsError { code: "status_not_allowed", message: format!("The status '{s}' can not be allowed. Sending to it hurts your inbox.") });
        }
        Ok(())
    }

    /// Merge a PATCH body (only the fields to change) into the current settings.
    pub fn merged(&self, patch: &serde_json::Value) -> Result<Self, SettingsError> {
        let mut base = serde_json::to_value(self).unwrap_or_default();
        let (Some(b), Some(p)) = (base.as_object_mut(), patch.as_object()) else {
            return Err(SettingsError { code: "invalid_settings", message: "Send an object.".into() });
        };
        for (k, v) in p {
            b.insert(k.clone(), v.clone());
        }
        let mut out: Self = serde_json::from_value(base).map_err(|_| SettingsError { code: "invalid_settings", message: "A setting has a wrong type or value.".into() })?;
        out.send_rule = out.send_rule.normalized();
        Ok(out)
    }
}

pub async fn load<'e>(db: impl PgExecutor<'e>) -> Result<VerifySettings, sqlx::Error> {
    let (v,): (serde_json::Value,) = sqlx::query_as("SELECT settings FROM verify_defaults WHERE id = 1").fetch_one(db).await?;
    Ok(VerifySettings::from_stored(v))
}

pub async fn is_paused<'e>(db: impl PgExecutor<'e>) -> Result<bool, sqlx::Error> {
    let (p,): (bool,) = sqlx::query_as("SELECT engine_paused FROM verify_defaults WHERE id = 1").fetch_one(db).await?;
    Ok(p)
}
