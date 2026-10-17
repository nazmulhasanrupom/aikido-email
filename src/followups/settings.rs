//! `FollowupSettings`: the rules of the follow-up engine. Stored as `jsonb` in `followup_defaults`.
//! `#[serde(default)]` means a new field needs no migration. Validate on every write.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use utoipa::ToSchema;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct FollowupSettings {
    /// `true`: an address can be in one open enrollment at a time, in all sequences.
    pub one_active_per_email: bool,
    /// 1 to 60. A follow-up that waits in the send queue this long is cancelled and the enrollment fails
    /// with reason `stalled` (the account is paused, the inbox sync is stale for days, ...).
    pub max_wait_days: u32,
    /// 5 to 3600. How often the follow-up scheduler looks for due steps.
    pub tick_secs: u32,
}

impl Default for FollowupSettings {
    fn default() -> Self {
        Self { one_active_per_email: true, max_wait_days: 7, tick_secs: 30 }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct SettingsError {
    pub code: &'static str,
    pub message: String,
}

impl SettingsError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self { code, message: message.into() }
    }
}

impl FollowupSettings {
    pub fn validate(&self) -> Result<(), SettingsError> {
        let range = |name: &str, v: u32, min: u32, max: u32| {
            if (min..=max).contains(&v) {
                Ok(())
            } else {
                Err(SettingsError::new("invalid_setting", format!("{name} must be {min} to {max}.")))
            }
        };
        range("max_wait_days", self.max_wait_days, 1, 60)?;
        range("tick_secs", self.tick_secs, 5, 3600)?;
        Ok(())
    }

    /// Load from `jsonb`. Missing fields get defaults. Unknown fields are ignored.
    pub fn from_stored(value: Value) -> Self {
        serde_json::from_value(value).unwrap_or_default()
    }

    /// PATCH: only the fields in `patch` change. The caller then calls `validate`.
    pub fn merged(&self, patch: &Value) -> Result<Self, SettingsError> {
        let Value::Object(patch) = patch else {
            return Err(SettingsError::new("invalid_settings", "The body must be a JSON object."));
        };
        let Value::Object(mut current) = serde_json::to_value(self).unwrap_or_default() else {
            unreachable!("settings always serialize to an object");
        };
        for (key, value) in patch {
            if !current.contains_key(key) {
                return Err(SettingsError::new("unknown_setting", format!("'{key}' is not a follow-up setting.")));
            }
            current.insert(key.clone(), value.clone());
        }
        serde_json::from_value(Value::Object(current)).map_err(|e| SettingsError::new("invalid_settings", e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn msg(patch: Value) -> String {
        FollowupSettings::default().merged(&patch).and_then(|s| s.validate().map(|_| s)).unwrap_err().message
    }

    #[test]
    fn defaults_are_valid() {
        FollowupSettings::default().validate().unwrap();
    }

    #[test]
    fn old_rows_get_defaults() {
        let s = FollowupSettings::from_stored(json!({ "tick_secs": 60 }));
        assert_eq!(s.tick_secs, 60);
        assert_eq!(s.max_wait_days, 7);
        assert!(s.one_active_per_email);
    }

    #[test]
    fn validation_messages() {
        assert!(msg(json!({ "max_wait_days": 0 })).contains("1 to 60"));
        assert!(msg(json!({ "max_wait_days": 61 })).contains("1 to 60"));
        assert!(msg(json!({ "tick_secs": 4 })).contains("5 to 3600"));
        assert!(msg(json!({ "tick_secs": 3601 })).contains("5 to 3600"));
        assert!(msg(json!({ "one_active_per_email": "yes" })).len() > 3);
    }

    #[test]
    fn unknown_setting_is_refused() {
        assert_eq!(FollowupSettings::default().merged(&json!({ "nope": 1 })).unwrap_err().code, "unknown_setting");
        assert_eq!(FollowupSettings::default().merged(&json!([1])).unwrap_err().code, "invalid_settings");
    }

    #[test]
    fn patch_merges() {
        let s = FollowupSettings::default().merged(&json!({ "one_active_per_email": false })).unwrap();
        assert!(!s.one_active_per_email);
        assert_eq!(s.max_wait_days, 7);
    }
}
