//! One lead from the outside world (API body, CSV row), checked and cleaned.
use super::email;
use super::fields::{FieldDefs, MAX_VALUE_CHARS, clean_custom, clean_tags};
use serde::Deserialize;
use serde_json::{Map, Value};
use utoipa::ToSchema;

pub const MAX_NOTES_CHARS: usize = 5000;

/// A lead as a caller sends it. Only `email` is required.
#[derive(Deserialize, ToSchema, Default, Clone, Debug)]
pub struct RawLead {
    /// Trimmed and lower-cased when saved.
    pub email: String,
    pub first_name: Option<String>,
    pub last_name: Option<String>,
    pub company_name: Option<String>,
    pub job_title: Option<String>,
    pub phone: Option<String>,
    pub website: Option<String>,
    pub linkedin_url: Option<String>,
    pub location: Option<String>,
    /// IANA name, like `Asia/Dhaka`.
    pub timezone: Option<String>,
    pub industry: Option<String>,
    pub personalization_note: Option<String>,
    pub trigger_event: Option<String>,
    /// `true` when the trigger event was checked by a person. Empty means `false`.
    pub trigger_verified: Option<bool>,
    /// The CSV cell of `trigger_verified` (`yes/no`, `true/false`, `1/0`, `y/n`). A bad value is a row error.
    #[serde(skip)]
    pub trigger_verified_text: Option<String>,
    /// Keys must exist in `/v1/leads/fields`. Up to 1000 characters per value.
    #[schema(value_type = Option<Object>)]
    pub custom_fields: Option<Map<String, Value>>,
    pub tags: Option<Vec<String>>,
    pub notes: Option<String>,
}

/// A lead that passed every check. `upsert_batch` takes only these.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LeadInput {
    pub email: String,
    pub domain: String,
    pub first_name: Option<String>,
    pub last_name: Option<String>,
    pub company_name: Option<String>,
    pub job_title: Option<String>,
    pub phone: Option<String>,
    pub website: Option<String>,
    pub linkedin_url: Option<String>,
    pub location: Option<String>,
    pub timezone: Option<String>,
    pub industry: Option<String>,
    pub personalization_note: Option<String>,
    pub trigger_event: Option<String>,
    /// `None`: not given, so an existing value is kept.
    pub trigger_verified: Option<bool>,
    pub custom_fields: Map<String, Value>,
    pub tags: Vec<String>,
    pub notes: Option<String>,
}

/// Trim. Empty means "no value". Longer than `max` characters is an error.
pub fn clean_text(name: &str, v: Option<&str>, max: usize) -> Result<Option<String>, String> {
    let Some(v) = v.map(str::trim).filter(|v| !v.is_empty()) else { return Ok(None) };
    if v.chars().count() > max {
        return Err(format!("{name} is longer than {max} characters"));
    }
    Ok(Some(v.to_string()))
}

pub fn clean_timezone(v: Option<&str>) -> Result<Option<String>, String> {
    let Some(v) = clean_text("timezone", v, 64)? else { return Ok(None) };
    v.parse::<chrono_tz::Tz>().map_err(|_| format!("'{v}' is not an IANA time zone (like Asia/Dhaka)"))?;
    Ok(Some(v))
}

/// `yes/no`, `true/false`, `1/0`, `y/n` in any case. Anything else is an error.
pub fn parse_flag(name: &str, v: &str) -> Result<bool, String> {
    match v.trim().to_lowercase().as_str() {
        "yes" | "true" | "1" | "y" => Ok(true),
        "no" | "false" | "0" | "n" => Ok(false),
        other => Err(format!("{name} must be yes or no (got '{other}')")),
    }
}

impl RawLead {
    pub fn check(&self, defs: &FieldDefs) -> Result<LeadInput, String> {
        let email = email::normalize(&self.email).map_err(str::to_string)?;
        let t = |name: &str, v: &Option<String>| clean_text(name, v.as_deref(), MAX_VALUE_CHARS);
        Ok(LeadInput {
            domain: email::domain_of(&email),
            email,
            first_name: t("first_name", &self.first_name)?,
            last_name: t("last_name", &self.last_name)?,
            company_name: t("company_name", &self.company_name)?,
            job_title: t("job_title", &self.job_title)?,
            phone: t("phone", &self.phone)?,
            website: t("website", &self.website)?,
            linkedin_url: t("linkedin_url", &self.linkedin_url)?,
            location: t("location", &self.location)?,
            timezone: clean_timezone(self.timezone.as_deref())?,
            industry: t("industry", &self.industry)?,
            personalization_note: t("personalization_note", &self.personalization_note)?,
            trigger_event: t("trigger_event", &self.trigger_event)?,
            trigger_verified: match self.trigger_verified_text.as_deref().map(str::trim).filter(|v| !v.is_empty()) {
                Some(v) => Some(parse_flag("trigger_verified", v)?),
                None => self.trigger_verified,
            },
            custom_fields: match &self.custom_fields {
                Some(m) => clean_custom(defs, m, false)?,
                None => Map::new(),
            },
            tags: clean_tags(self.tags.as_deref().unwrap_or(&[]))?,
            notes: clean_text("notes", self.notes.as_deref(), MAX_NOTES_CHARS)?,
        })
    }
}
