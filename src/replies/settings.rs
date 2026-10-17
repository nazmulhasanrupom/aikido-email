//! `ReplySettings`: every reply-check rule lives in this one struct. Stored as `jsonb`.
//! `#[serde(default)]` means a new field needs no migration. Validate on every write.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use utoipa::ToSchema;

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum StopScope {
    Email,
    Domain,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum OnAutoReply {
    Ignore,
    Stop,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct ReplySettings {
    /// Turns reply check on or off for this account.
    pub enabled: bool,
    /// 30 to 3600.
    pub poll_interval_secs: u32,
    /// `inbox` and/or `spam`.
    pub folders: Vec<String>,
    /// 0 to 90. The first sync reads back this many days.
    pub initial_lookback_days: u32,
    /// 1 to 180. Sender-address matches count only for sends this recent.
    pub match_window_days: u32,
    /// 10 to 1000. Max full downloads per run.
    pub max_messages_per_sync: u32,
    /// On reply: stop this address (`email`) or everyone at that company too (`domain`).
    pub stop_scope: StopScope,
    /// `ignore`: follow-ups continue. `stop`: treat an auto-reply like a reply.
    pub on_auto_reply: OnAutoReply,
    pub unsubscribe_keywords: Vec<String>,
    /// 1 to 200. Longer replies are never labeled `unsubscribe`.
    pub unsubscribe_max_words: u32,
    pub auto_reply_subject_patterns: Vec<String>,
    pub bounce_subject_patterns: Vec<String>,
    /// 1 to 10. Soft bounces to one address in 30 days before it becomes a hard suppression.
    pub soft_bounce_limit: u32,
    /// `5.7.x` blocks count toward the bounce-rate pause.
    pub count_blocks_in_bounce_rate: bool,
}

fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| s.to_string()).collect()
}

impl Default for ReplySettings {
    fn default() -> Self {
        Self {
            enabled: true,
            poll_interval_secs: 120,
            folders: strings(&["inbox", "spam"]),
            initial_lookback_days: 14,
            match_window_days: 30,
            max_messages_per_sync: 200,
            stop_scope: StopScope::Email,
            on_auto_reply: OnAutoReply::Ignore,
            unsubscribe_keywords: strings(&[
                "unsubscribe", "remove me", "take me off", "stop emailing", "stop sending",
                "do not contact", "don't contact", "opt out", "opt-out",
            ]),
            unsubscribe_max_words: 30,
            auto_reply_subject_patterns: strings(&[
                "automatic reply", "auto reply", "auto-reply", "autoreply", "out of office",
                "out of the office", "ooo:", "on vacation", "away from the office",
                "abwesenheitsnotiz", "réponse automatique", "respuesta automática", "risposta automatica",
            ]),
            bounce_subject_patterns: strings(&[
                "undeliverable", "delivery status notification", "mail delivery failed",
                "returned mail", "delivery failure", "failure notice", "undelivered mail",
            ]),
            soft_bounce_limit: 3,
            count_blocks_in_bounce_rate: true,
        }
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

fn check_range(name: &'static str, value: u32, min: u32, max: u32) -> Result<(), SettingsError> {
    if (min..=max).contains(&value) {
        Ok(())
    } else {
        Err(SettingsError::new("invalid_setting", format!("{name} must be {min} to {max}.")))
    }
}

fn check_list(name: &'static str, list: &[String]) -> Result<(), SettingsError> {
    if list.len() > 100 {
        return Err(SettingsError::new("invalid_setting", format!("{name} can have at most 100 items.")));
    }
    for item in list {
        let len = item.trim().chars().count();
        if len == 0 {
            return Err(SettingsError::new("invalid_setting", format!("{name} must not have an empty item.")));
        }
        if item.chars().count() > 100 {
            return Err(SettingsError::new("invalid_setting", format!("Each item of {name} must be 1 to 100 characters.")));
        }
    }
    Ok(())
}

impl ReplySettings {
    /// Checks every rule.
    pub fn validate(&self) -> Result<(), SettingsError> {
        check_range("poll_interval_secs", self.poll_interval_secs, 30, 3600)?;
        if self.folders.is_empty() {
            return Err(SettingsError::new("invalid_setting", "folders must not be empty."));
        }
        if let Some(bad) = self.folders.iter().find(|f| !matches!(f.as_str(), "inbox" | "spam")) {
            return Err(SettingsError::new("invalid_setting", format!("'{bad}' is not a folder. Use 'inbox' or 'spam'.")));
        }
        let mut seen = self.folders.clone();
        seen.sort();
        seen.dedup();
        if seen.len() != self.folders.len() {
            return Err(SettingsError::new("invalid_setting", "folders must not repeat a folder."));
        }
        check_range("initial_lookback_days", self.initial_lookback_days, 0, 90)?;
        check_range("match_window_days", self.match_window_days, 1, 180)?;
        check_range("max_messages_per_sync", self.max_messages_per_sync, 10, 1000)?;
        check_range("unsubscribe_max_words", self.unsubscribe_max_words, 1, 200)?;
        check_range("soft_bounce_limit", self.soft_bounce_limit, 1, 10)?;
        check_list("unsubscribe_keywords", &self.unsubscribe_keywords)?;
        check_list("auto_reply_subject_patterns", &self.auto_reply_subject_patterns)?;
        check_list("bounce_subject_patterns", &self.bounce_subject_patterns)?;
        Ok(())
    }

    /// Load from `jsonb`. Missing fields get defaults. Unknown fields are ignored.
    pub fn from_stored(value: Value) -> Self {
        serde_json::from_value(value).unwrap_or_default()
    }

    /// PATCH: only the fields in `patch` change. Merge; the caller then calls `validate`.
    pub fn merged(&self, patch: &Value) -> Result<Self, SettingsError> {
        let Value::Object(patch) = patch else {
            return Err(SettingsError::new("invalid_settings", "The body must be a JSON object."));
        };
        let Value::Object(mut current) = serde_json::to_value(self).unwrap_or_default() else {
            unreachable!("settings always serialize to an object");
        };
        for (key, value) in patch {
            if !current.contains_key(key) {
                return Err(SettingsError::new("unknown_setting", format!("'{key}' is not a reply setting.")));
            }
            current.insert(key.clone(), value.clone());
        }
        serde_json::from_value(Value::Object(current)).map_err(|e| SettingsError::new("invalid_settings", e.to_string()))
    }

    /// Lowercase copies for the pure classifier.
    pub fn lower(list: &[String]) -> Vec<String> {
        list.iter().map(|s| s.trim().to_lowercase()).filter(|s| !s.is_empty()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn defaults_are_valid() {
        ReplySettings::default().validate().unwrap();
    }

    #[test]
    fn old_rows_get_defaults() {
        let s = ReplySettings::from_stored(json!({ "poll_interval_secs": 60 }));
        assert_eq!(s.poll_interval_secs, 60);
        assert_eq!(s.soft_bounce_limit, 3);
    }

    fn msg(patch: Value) -> String {
        ReplySettings::default().merged(&patch).and_then(|s| s.validate().map(|_| s)).unwrap_err().message
    }

    #[test]
    fn validation_messages() {
        assert!(msg(json!({ "poll_interval_secs": 29 })).contains("30 to 3600"));
        assert!(msg(json!({ "poll_interval_secs": 3601 })).contains("30 to 3600"));
        assert!(msg(json!({ "folders": [] })).contains("not be empty"));
        assert!(msg(json!({ "folders": ["sent"] })).contains("not a folder"));
        assert!(msg(json!({ "folders": ["inbox", "inbox"] })).contains("repeat"));
        assert!(msg(json!({ "initial_lookback_days": 91 })).contains("0 to 90"));
        assert!(msg(json!({ "match_window_days": 0 })).contains("1 to 180"));
        assert!(msg(json!({ "max_messages_per_sync": 9 })).contains("10 to 1000"));
        assert!(msg(json!({ "unsubscribe_max_words": 0 })).contains("1 to 200"));
        assert!(msg(json!({ "soft_bounce_limit": 11 })).contains("1 to 10"));
        assert!(msg(json!({ "unsubscribe_keywords": [""] })).contains("empty item"));
        assert!(msg(json!({ "unsubscribe_keywords": ["  "] })).contains("empty item"));
        assert!(msg(json!({ "bounce_subject_patterns": ["x".repeat(101)] })).contains("1 to 100"));
        assert!(msg(json!({ "auto_reply_subject_patterns": vec!["x"; 101] })).contains("at most 100"));
        assert!(msg(json!({ "stop_scope": "everyone" })).contains("unknown variant"));
    }

    #[test]
    fn unknown_setting_is_refused() {
        let e = ReplySettings::default().merged(&json!({ "nope": 1 })).unwrap_err();
        assert_eq!(e.code, "unknown_setting");
    }

    #[test]
    fn patch_merges() {
        let s = ReplySettings::default().merged(&json!({ "stop_scope": "domain", "on_auto_reply": "stop" })).unwrap();
        assert_eq!(s.stop_scope, StopScope::Domain);
        assert_eq!(s.on_auto_reply, OnAutoReply::Stop);
        assert_eq!(s.soft_bounce_limit, 3);
        s.validate().unwrap();
    }
}
