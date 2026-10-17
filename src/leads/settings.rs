//! `LeadSettings`: stored as `jsonb` in `lead_defaults`. `#[serde(default)]` means a new field needs no
//! migration. Validate on every write.
use super::model::DedupeMode;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use utoipa::ToSchema;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct LeadSettings {
    /// 1 to 1000000. A CSV with more rows is refused.
    pub import_max_rows: u32,
    /// 1 KB to 200 MB. A bigger CSV is refused.
    pub import_max_bytes: u64,
    /// Look up the MX record of each new domain.
    pub mx_check: bool,
    /// Used when an import does not say.
    pub default_dedupe_mode: DedupeMode,
    /// 5 to 3600. How often the campaign feeder runs.
    pub feeder_tick_secs: u32,
    /// 1 to 5000. The daily new-lead limit of a new campaign.
    pub default_daily_new_leads: u32,
    /// Free-mail domains to add to the built-in list. Used by the company stop.
    pub free_mail_domains_extra: Vec<String>,
    /// `false`: the import mapping uses the rules only (no AI call).
    pub ai_mapping_enabled: bool,
    /// The provider for the AI column mapping. `null`: the generator's slot provider (then its draft provider).
    pub ai_mapping_provider_id: Option<uuid::Uuid>,
    /// 1 to 10. How many sample values per column go to the mapper.
    pub mapping_sample_rows: u32,
    /// Mask emails and phone numbers before the samples go to the model.
    pub mapping_mask_pii: bool,
    /// 0 to 1. A column under this confidence goes in `needs_check`.
    pub mapping_min_confidence: f64,
    /// Reuse a saved mapping for the same headers.
    pub mapping_templates_enabled: bool,
}

impl Default for LeadSettings {
    fn default() -> Self {
        Self {
            import_max_rows: 100_000,
            import_max_bytes: 50 * 1024 * 1024,
            mx_check: true,
            default_dedupe_mode: DedupeMode::FillEmpty,
            feeder_tick_secs: 60,
            default_daily_new_leads: 50,
            free_mail_domains_extra: Vec::new(),
            ai_mapping_enabled: true,
            ai_mapping_provider_id: None,
            mapping_sample_rows: 5,
            mapping_mask_pii: true,
            mapping_min_confidence: 0.7,
            mapping_templates_enabled: true,
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

impl LeadSettings {
    pub fn validate(&mut self) -> Result<(), SettingsError> {
        let range = |name: &str, v: u64, min: u64, max: u64| {
            if (min..=max).contains(&v) {
                Ok(())
            } else {
                Err(SettingsError::new("invalid_setting", format!("{name} must be {min} to {max}.")))
            }
        };
        range("import_max_rows", self.import_max_rows.into(), 1, 1_000_000)?;
        range("import_max_bytes", self.import_max_bytes, 1024, 200 * 1024 * 1024)?;
        range("feeder_tick_secs", self.feeder_tick_secs.into(), 5, 3600)?;
        range("default_daily_new_leads", self.default_daily_new_leads.into(), 1, 5000)?;
        range("mapping_sample_rows", self.mapping_sample_rows.into(), 1, 10)?;
        if !(0.0..=1.0).contains(&self.mapping_min_confidence) {
            return Err(SettingsError::new("invalid_setting", "mapping_min_confidence must be 0 to 1."));
        }
        let mut domains = Vec::new();
        for d in &self.free_mail_domains_extra {
            let d = d.trim().to_lowercase();
            if d.is_empty() || d.contains(['@', ' ', '/']) || !d.contains('.') {
                return Err(SettingsError::new("invalid_setting", format!("'{d}' is not a valid domain in free_mail_domains_extra.")));
            }
            if !domains.contains(&d) {
                domains.push(d);
            }
        }
        if domains.len() > 500 {
            return Err(SettingsError::new("invalid_setting", "free_mail_domains_extra holds up to 500 domains."));
        }
        self.free_mail_domains_extra = domains;
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
                return Err(SettingsError::new("unknown_setting", format!("'{key}' is not a lead setting.")));
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

    fn err(patch: Value) -> SettingsError {
        let mut s = LeadSettings::default().merged(&patch).unwrap();
        s.validate().unwrap_err()
    }

    #[test]
    fn defaults_are_valid() {
        LeadSettings::default().validate().unwrap();
    }

    #[test]
    fn old_rows_get_defaults() {
        let s = LeadSettings::from_stored(json!({ "feeder_tick_secs": 30 }));
        assert_eq!(s.feeder_tick_secs, 30);
        assert_eq!(s.import_max_rows, 100_000);
        assert!(s.mx_check);
    }

    #[test]
    fn validation() {
        assert!(err(json!({ "import_max_rows": 0 })).message.contains("1 to 1000000"));
        assert!(err(json!({ "feeder_tick_secs": 4 })).message.contains("5 to 3600"));
        assert!(err(json!({ "default_daily_new_leads": 5001 })).message.contains("1 to 5000"));
        assert!(err(json!({ "free_mail_domains_extra": ["nodot"] })).message.contains("nodot"));
        assert!(err(json!({ "mapping_sample_rows": 11 })).message.contains("1 to 10"));
        assert!(err(json!({ "mapping_min_confidence": 1.5 })).message.contains("0 to 1"));
        assert_eq!(LeadSettings::default().merged(&json!({ "nope": 1 })).unwrap_err().code, "unknown_setting");
    }

    #[test]
    fn domains_are_cleaned() {
        let mut s = LeadSettings::default().merged(&json!({ "free_mail_domains_extra": [" Acme.IO ", "acme.io"] })).unwrap();
        s.validate().unwrap();
        assert_eq!(s.free_mail_domains_extra, vec!["acme.io".to_string()]);
    }
}
