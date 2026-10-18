//! `SendingSettings`: every sending rule lives in this one struct.
//! It is stored as `jsonb`. `#[serde(default)]` on the struct means a new field needs no
//! migration: old rows get the default value. Validate on every write.
use chrono::NaiveDate;
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use utoipa::ToSchema;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct ListUnsubscribe {
    /// Example: `unsubscribe@example.com`
    pub mailto: Option<String>,
    /// Example: `https://example.com/unsubscribe`
    pub url: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct SendingSettings {
    /// ISO weekday numbers. 1 = Monday, 7 = Sunday.
    pub send_days: Vec<u8>,
    /// IANA time zone name, for example `Asia/Dhaka`.
    pub timezone: String,
    /// `HH:MM`, local time. Start is included.
    pub window_start: String,
    /// `HH:MM`, local time. End is not included.
    pub window_end: String,
    pub base_delay_secs: u32,
    pub jitter_min_secs: u32,
    pub jitter_max_secs: u32,
    pub daily_limit: u32,
    pub window_start_jitter_mins: u32,
    pub daily_limit_jitter_pct: u32,
    pub hourly_limit: u32,
    /// `YYYY-MM-DD` in the account time zone.
    pub blocked_dates: Vec<NaiveDate>,
    pub warmup_enabled: bool,
    pub warmup_start: u32,
    pub warmup_step: u32,
    pub per_domain_daily_limit: u32,
    pub per_domain_exempt: Vec<String>,
    pub same_recipient_cooldown_hours: u32,
    pub max_attempts: u32,
    pub retry_backoff_mins: Vec<u32>,
    pub pause_after_failures: u32,
    pub pause_bounce_rate_pct: u32,
    pub bounce_window: u32,
    /// A follow-up waits unless the inbox was read this many minutes ago or less. 0 = off.
    pub reply_check_max_age_mins: u32,
    /// When an account has both a first email and a follow-up ready, send the follow-up first.
    /// Finish the threads that already started before new ones open under the same daily cap.
    pub followups_first: bool,
    pub list_unsubscribe: Option<ListUnsubscribe>,
    /// HTML added under every email of the account. May hold `{{unsubscribe}}`, `{{sender_name}}`, `{{sender_email}}`.
    pub signature_html: String,
}

impl Default for SendingSettings {
    fn default() -> Self {
        Self {
            send_days: vec![1, 2, 3, 4, 5],
            timezone: "UTC".into(),
            window_start: "09:00".into(),
            window_end: "17:00".into(),
            base_delay_secs: 300,
            jitter_min_secs: 120,
            jitter_max_secs: 600,
            daily_limit: 30,
            window_start_jitter_mins: 15,
            daily_limit_jitter_pct: 0,
            hourly_limit: 10,
            blocked_dates: vec![],
            warmup_enabled: false,
            warmup_start: 5,
            warmup_step: 2,
            per_domain_daily_limit: 5,
            per_domain_exempt: ["gmail.com", "outlook.com", "hotmail.com", "yahoo.com", "icloud.com"]
                .map(String::from)
                .to_vec(),
            same_recipient_cooldown_hours: 24,
            max_attempts: 3,
            retry_backoff_mins: vec![15, 60],
            pause_after_failures: 3,
            pause_bounce_rate_pct: 5,
            bounce_window: 50,
            reply_check_max_age_mins: 15,
            followups_first: true,
            list_unsubscribe: None,
            signature_html: String::new(),
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

pub fn parse_hhmm(s: &str) -> Option<chrono::NaiveTime> {
    chrono::NaiveTime::parse_from_str(s, "%H:%M").ok()
}

impl SendingSettings {
    pub fn tz(&self) -> Tz {
        self.timezone.parse().unwrap_or(Tz::UTC)
    }

    pub fn window(&self) -> (chrono::NaiveTime, chrono::NaiveTime) {
        use chrono::NaiveTime;
        (
            parse_hhmm(&self.window_start).unwrap_or(NaiveTime::from_hms_opt(9, 0, 0).unwrap()),
            parse_hhmm(&self.window_end).unwrap_or(NaiveTime::from_hms_opt(17, 0, 0).unwrap()),
        )
    }

    pub fn is_domain_exempt(&self, domain: &str) -> bool {
        self.per_domain_exempt.iter().any(|d| d.eq_ignore_ascii_case(domain))
    }

    /// Checks every rule. Returns warnings (not errors) on success.
    pub fn validate(&self) -> Result<Vec<String>, SettingsError> {
        let mut warnings = Vec::new();
        if self.timezone.parse::<Tz>().is_err() {
            return Err(SettingsError::new(
                "invalid_timezone",
                format!("Timezone '{}' is not valid. Use an IANA name like 'Asia/Dhaka'.", self.timezone),
            ));
        }
        if self.send_days.is_empty() {
            return Err(SettingsError::new("invalid_send_days", "send_days must not be empty."));
        }
        if self.send_days.iter().any(|d| !(1..=7).contains(d)) {
            return Err(SettingsError::new("invalid_send_days", "send_days values must be 1 (Monday) to 7 (Sunday)."));
        }
        let mut seen = self.send_days.clone();
        seen.sort_unstable();
        seen.dedup();
        if seen.len() != self.send_days.len() {
            return Err(SettingsError::new("invalid_send_days", "send_days must not repeat a day."));
        }
        let (Some(start), Some(end)) = (parse_hhmm(&self.window_start), parse_hhmm(&self.window_end)) else {
            return Err(SettingsError::new("invalid_window", "window_start and window_end must look like 'HH:MM', for example '09:00'."));
        };
        if start >= end {
            return Err(SettingsError::new(
                "invalid_window",
                "window_start must be before window_end. Overnight windows are not supported.",
            ));
        }
        if self.jitter_min_secs > self.jitter_max_secs {
            return Err(SettingsError::new("invalid_jitter", "jitter_min_secs must not be more than jitter_max_secs."));
        }
        if self.base_delay_secs < 60 {
            return Err(SettingsError::new("invalid_base_delay", "base_delay_secs must be at least 60."));
        }
        if !(1..=500).contains(&self.daily_limit) {
            return Err(SettingsError::new("invalid_daily_limit", "daily_limit must be 1 to 500."));
        }
        if self.daily_limit > 50 {
            warnings.push(format!(
                "daily_limit {} is high. Sending more than 50 emails a day from one inbox risks spam filters.",
                self.daily_limit
            ));
        }
        if self.daily_limit_jitter_pct > 50 {
            return Err(SettingsError::new("invalid_daily_limit_jitter", "daily_limit_jitter_pct must be 0 to 50."));
        }
        if !(1..=100).contains(&self.pause_bounce_rate_pct) {
            return Err(SettingsError::new("invalid_bounce_rate", "pause_bounce_rate_pct must be 1 to 100."));
        }
        if self.hourly_limit < 1 {
            return Err(SettingsError::new("invalid_hourly_limit", "hourly_limit must be at least 1."));
        }
        if self.window_start_jitter_mins > 600 {
            return Err(SettingsError::new("invalid_window_jitter", "window_start_jitter_mins must be 0 to 600."));
        }
        if self.warmup_start < 1 {
            return Err(SettingsError::new("invalid_warmup", "warmup_start must be at least 1."));
        }
        if self.per_domain_daily_limit < 1 {
            return Err(SettingsError::new("invalid_domain_limit", "per_domain_daily_limit must be at least 1."));
        }
        if self.max_attempts < 1 {
            return Err(SettingsError::new("invalid_max_attempts", "max_attempts must be at least 1."));
        }
        if (self.retry_backoff_mins.len() as u32) < self.max_attempts - 1 {
            return Err(SettingsError::new(
                "invalid_retry_backoff",
                format!(
                    "retry_backoff_mins needs at least {} values (max_attempts - 1).",
                    self.max_attempts - 1
                ),
            ));
        }
        if self.pause_after_failures < 1 {
            return Err(SettingsError::new("invalid_pause_after_failures", "pause_after_failures must be at least 1."));
        }
        if self.bounce_window < 1 {
            return Err(SettingsError::new("invalid_bounce_window", "bounce_window must be at least 1."));
        }
        if self.reply_check_max_age_mins > 1440 {
            return Err(SettingsError::new(
                "invalid_reply_check_max_age",
                "reply_check_max_age_mins must be 0 to 1440 (0 turns the check off).",
            ));
        }
        if let Some(lu) = &self.list_unsubscribe {
            if lu.mailto.as_deref().map_or(true, str::is_empty) && lu.url.as_deref().map_or(true, str::is_empty) {
                return Err(SettingsError::new(
                    "invalid_list_unsubscribe",
                    "list_unsubscribe needs a mailto, a url, or both. Use null to turn it off.",
                ));
            }
            if let Some(url) = &lu.url {
                if !url.starts_with("https://") && !url.starts_with("http://") {
                    return Err(SettingsError::new("invalid_list_unsubscribe", "list_unsubscribe.url must start with https://."));
                }
            }
        }
        if self.signature_html.chars().count() > 20_000 {
            return Err(SettingsError::new("invalid_signature", "signature_html must be at most 20000 characters."));
        }
        Ok(warnings)
    }

    /// Load from `jsonb`. Missing fields get defaults. Unknown fields are ignored.
    pub fn from_stored(value: Value) -> Self {
        serde_json::from_value(value).unwrap_or_default()
    }

    /// PATCH: only the fields in `patch` change. Merge, then validate the full result.
    pub fn merged(&self, patch: &Value) -> Result<Self, SettingsError> {
        let Value::Object(patch) = patch else {
            return Err(SettingsError::new("invalid_settings", "The body must be a JSON object."));
        };
        let Value::Object(mut current) = serde_json::to_value(self).unwrap_or_default() else {
            unreachable!("settings always serialize to an object");
        };
        for (key, value) in patch {
            if !current.contains_key(key) {
                return Err(SettingsError::new("unknown_setting", format!("'{key}' is not a sending setting.")));
            }
            current.insert(key.clone(), value.clone());
        }
        let merged: SendingSettings = serde_json::from_value(Value::Object(current))
            .map_err(|e| SettingsError::new("invalid_settings", e.to_string()))?;
        Ok(merged)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn defaults_are_valid() {
        assert!(SendingSettings::default().validate().unwrap().is_empty());
    }

    #[test]
    fn old_rows_get_defaults() {
        let s = SendingSettings::from_stored(json!({ "daily_limit": 12 }));
        assert_eq!(s.daily_limit, 12);
        assert_eq!(s.base_delay_secs, 300);
    }

    fn code(patch: Value) -> &'static str {
        SendingSettings::default().merged(&patch).and_then(|s| s.validate()).unwrap_err().code
    }

    #[test]
    fn validation_codes() {
        assert_eq!(code(json!({ "timezone": "Dhaka" })), "invalid_timezone");
        assert_eq!(code(json!({ "send_days": [] })), "invalid_send_days");
        assert_eq!(code(json!({ "send_days": [0] })), "invalid_send_days");
        assert_eq!(code(json!({ "send_days": [1, 1] })), "invalid_send_days");
        assert_eq!(code(json!({ "window_start": "18:00" })), "invalid_window");
        assert_eq!(code(json!({ "window_start": "9am" })), "invalid_window");
        assert_eq!(code(json!({ "jitter_min_secs": 700 })), "invalid_jitter");
        assert_eq!(code(json!({ "base_delay_secs": 59 })), "invalid_base_delay");
        assert_eq!(code(json!({ "daily_limit": 0 })), "invalid_daily_limit");
        assert_eq!(code(json!({ "daily_limit": 501 })), "invalid_daily_limit");
        assert_eq!(code(json!({ "daily_limit_jitter_pct": 51 })), "invalid_daily_limit_jitter");
        assert_eq!(code(json!({ "pause_bounce_rate_pct": 0 })), "invalid_bounce_rate");
        assert_eq!(code(json!({ "max_attempts": 5 })), "invalid_retry_backoff");
        assert_eq!(code(json!({ "nope": 1 })), "unknown_setting");
        assert_eq!(code(json!({ "daily_limit": "x" })), "invalid_settings");
    }

    #[test]
    fn high_daily_limit_is_a_warning() {
        let s = SendingSettings { daily_limit: 80, ..Default::default() };
        assert_eq!(s.validate().unwrap().len(), 1);
    }

    #[test]
    fn patch_merges() {
        let s = SendingSettings::default().merged(&json!({ "timezone": "Asia/Dhaka", "list_unsubscribe": { "mailto": "u@x.com", "url": null } })).unwrap();
        assert_eq!(s.timezone, "Asia/Dhaka");
        assert_eq!(s.daily_limit, 30);
        assert!(s.list_unsubscribe.is_some());
        s.validate().unwrap();
    }
}
