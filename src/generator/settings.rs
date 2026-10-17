//! `GeneratorSettings`: the rules of the generator. Stored as `jsonb` in `generator_defaults`.
//! `#[serde(default)]` means a new field needs no migration. Validate on every write.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use utoipa::ToSchema;
use uuid::Uuid;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum OnFailure {
    /// No AI text at due time: use the fallback text of each slot.
    Fallback,
    /// No AI text at due time: hold the step (up to `max_wait_mins`), then use the fallback text.
    Wait,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SpamCheck {
    Warn,
    Fail,
}

/// USD per 1 million tokens.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct Price {
    pub input: f64,
    pub output: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct GeneratorSettings {
    /// The provider for per-lead slots (a cheap, fast model).
    pub slot_provider_id: Option<Uuid>,
    /// The provider for the sequence writer (a stronger model).
    pub draft_provider_id: Option<Uuid>,
    /// 1 to 72. A step is written when it is due within this many hours.
    pub lookahead_hours: u32,
    pub on_failure: OnFailure,
    /// 1 to 10080. With `on_failure = wait`: after this, the fallback text is used (or the enrollment fails if there is none).
    pub max_wait_mins: u32,
    /// 0 to 120. How long a due step with no AI text yet is held before `on_failure` applies. It gives the worker
    /// time to write the text of a step that was due the moment the lead was enrolled. 0 = apply `on_failure` at once.
    pub due_grace_mins: u32,
    /// `true`: every new text waits for your approval before it can send.
    pub review_required: bool,
    /// 500 to 20000. The lead context is cut to this many characters.
    pub context_max_chars: u32,
    /// 50 to 4000. Per call.
    pub max_output_tokens: u32,
    /// 0 to 2. Left out for models that do not accept it.
    pub temperature: f64,
    /// 1 to 10. Tries for one call (rate limit, server error, timeout), and tries for one text.
    pub max_attempts: u32,
    /// 1 to 32. Calls at the same time, per server copy.
    pub concurrency: u32,
    /// 0 = no limit. Input + output tokens of today (UTC), from `llm_calls`.
    pub daily_token_budget: u64,
    /// For example `en`. Used in the prompt and for a light check (warning only).
    pub language: Option<String>,
    pub banned_phrases: Vec<String>,
    pub spam_words: Vec<String>,
    pub spam_check: SpamCheck,
    /// 1 to 3600. How often the worker looks for steps to write.
    pub tick_secs: u32,
    /// Price per model id, to show money in `/usage`.
    pub prices: BTreeMap<String, Price>,
}

pub fn default_banned_phrases() -> Vec<String> {
    [
        "as an ai",
        "language model",
        "i hope this email finds you well",
        "i hope this finds you well",
        "i hope this message finds you",
        "i cannot assist",
        "i can't assist",
    ]
    .map(String::from)
    .to_vec()
}

pub fn default_spam_words() -> Vec<String> {
    ["act now", "buy now", "click here", "100% free", "risk-free", "guaranteed", "limited time offer", "winner", "!!!", "$$$"].map(String::from).to_vec()
}

impl Default for GeneratorSettings {
    fn default() -> Self {
        Self {
            slot_provider_id: None,
            draft_provider_id: None,
            lookahead_hours: 6,
            on_failure: OnFailure::Fallback,
            max_wait_mins: 120,
            due_grace_mins: 5,
            review_required: false,
            context_max_chars: 4000,
            max_output_tokens: 400,
            temperature: 0.7,
            max_attempts: 3,
            concurrency: 4,
            daily_token_budget: 0,
            language: None,
            banned_phrases: default_banned_phrases(),
            spam_words: default_spam_words(),
            spam_check: SpamCheck::Warn,
            tick_secs: 10,
            prices: BTreeMap::new(),
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

impl GeneratorSettings {
    pub fn validate(&self) -> Result<(), SettingsError> {
        let range = |name: &str, v: u64, min: u64, max: u64| {
            if (min..=max).contains(&v) { Ok(()) } else { Err(SettingsError::new("invalid_setting", format!("{name} must be {min} to {max}."))) }
        };
        range("lookahead_hours", self.lookahead_hours as u64, 1, 72)?;
        range("max_wait_mins", self.max_wait_mins as u64, 1, 10_080)?;
        range("due_grace_mins", self.due_grace_mins as u64, 0, 120)?;
        range("context_max_chars", self.context_max_chars as u64, 500, 20_000)?;
        range("max_output_tokens", self.max_output_tokens as u64, 50, 4000)?;
        range("max_attempts", self.max_attempts as u64, 1, 10)?;
        range("concurrency", self.concurrency as u64, 1, 32)?;
        range("tick_secs", self.tick_secs as u64, 1, 3600)?;
        if !(0.0..=2.0).contains(&self.temperature) {
            return Err(SettingsError::new("invalid_setting", "temperature must be 0 to 2."));
        }
        if let Some(l) = &self.language {
            let ok = (2..=8).contains(&l.len()) && l.chars().all(|c| c.is_ascii_alphabetic() || c == '-');
            if !ok {
                return Err(SettingsError::new("invalid_setting", "language must be a short code like en or pt-BR."));
            }
        }
        for (name, list) in [("banned_phrases", &self.banned_phrases), ("spam_words", &self.spam_words)] {
            if list.len() > 200 || list.iter().any(|p| p.trim().is_empty() || p.chars().count() > 100) {
                return Err(SettingsError::new("invalid_setting", format!("{name} can have up to 200 entries of 1 to 100 characters.")));
            }
        }
        for (model, p) in &self.prices {
            if model.trim().is_empty() || !(p.input >= 0.0 && p.output >= 0.0) || !p.input.is_finite() || !p.output.is_finite() {
                return Err(SettingsError::new("invalid_setting", "prices must be a model id with input and output of 0 or more."));
            }
        }
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
                return Err(SettingsError::new("unknown_setting", format!("'{key}' is not a generator setting.")));
            }
            current.insert(key.clone(), value.clone());
        }
        serde_json::from_value(Value::Object(current)).map_err(|e| SettingsError::new("invalid_settings", e.to_string()))
    }

    /// The words that count as a banned phrase / spam word, lower case.
    pub fn lowered(list: &[String]) -> Vec<String> {
        list.iter().map(|p| p.trim().to_lowercase()).filter(|p| !p.is_empty()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn msg(patch: Value) -> String {
        GeneratorSettings::default().merged(&patch).and_then(|s| s.validate().map(|_| s)).unwrap_err().message
    }

    #[test]
    fn defaults_are_valid() {
        GeneratorSettings::default().validate().unwrap();
    }

    #[test]
    fn old_rows_get_defaults() {
        let s = GeneratorSettings::from_stored(json!({ "lookahead_hours": 12 }));
        assert_eq!(s.lookahead_hours, 12);
        assert_eq!(s.max_attempts, 3);
        assert_eq!(s.on_failure, OnFailure::Fallback);
    }

    #[test]
    fn validation_messages() {
        assert!(msg(json!({ "lookahead_hours": 0 })).contains("1 to 72"));
        assert!(msg(json!({ "lookahead_hours": 73 })).contains("1 to 72"));
        assert!(msg(json!({ "max_wait_mins": 0 })).contains("max_wait_mins"));
        assert!(msg(json!({ "context_max_chars": 100 })).contains("500 to 20000"));
        assert!(msg(json!({ "max_output_tokens": 10 })).contains("50 to 4000"));
        assert!(msg(json!({ "temperature": 3.0 })).contains("temperature"));
        assert!(msg(json!({ "concurrency": 0 })).contains("1 to 32"));
        assert!(msg(json!({ "max_attempts": 11 })).contains("1 to 10"));
        assert!(msg(json!({ "language": "english please" })).contains("language"));
        assert!(msg(json!({ "banned_phrases": [""] })).contains("banned_phrases"));
        assert!(msg(json!({ "on_failure": "explode" })).len() > 3);
        assert!(msg(json!({ "prices": { "m": { "input": -1.0, "output": 1.0 } } })).contains("prices"));
    }

    #[test]
    fn unknown_setting_is_refused() {
        assert_eq!(GeneratorSettings::default().merged(&json!({ "nope": 1 })).unwrap_err().code, "unknown_setting");
        assert_eq!(GeneratorSettings::default().merged(&json!([1])).unwrap_err().code, "invalid_settings");
    }

    #[test]
    fn patch_merges_and_a_provider_can_be_cleared() {
        let id = Uuid::now_v7();
        let s = GeneratorSettings::default().merged(&json!({ "slot_provider_id": id, "on_failure": "wait" })).unwrap();
        assert_eq!(s.slot_provider_id, Some(id));
        assert_eq!(s.on_failure, OnFailure::Wait);
        assert_eq!(s.max_wait_mins, 120);
        let cleared = s.merged(&json!({ "slot_provider_id": null })).unwrap();
        assert_eq!(cleared.slot_provider_id, None);
    }
}
