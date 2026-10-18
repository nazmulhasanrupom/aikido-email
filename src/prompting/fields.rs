//! The field registry: every key the default prompt uses, where its value comes from, and whether it is "Optimized".
//! "Optimized" is a fact of the code, not a column in the database.
use serde::Serialize;
use utoipa::ToSchema;

#[derive(Serialize, ToSchema, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum Group {
    Sender,
    Lead,
    System,
}

#[derive(Serialize, ToSchema, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum FieldType {
    Text,
    Bool,
}

#[derive(Serialize, ToSchema, Clone, Debug)]
pub struct FieldInfo {
    /// The key in the prompt.
    pub key: &'static str,
    /// The label in the UI.
    pub label: &'static str,
    /// The database column (leads and the sender profile). Null for system fields.
    pub column: Option<&'static str>,
    pub group: Group,
    #[serde(rename = "type")]
    pub ty: FieldType,
    /// A lead field that must have a value, or the mail is held (`missing_required`).
    pub required: bool,
    /// What the prompt shows when there is no value.
    pub empty: &'static str,
    /// Used by the default mail prompt. The lead table shows a badge.
    pub optimized: bool,
}

const fn f(key: &'static str, label: &'static str, column: Option<&'static str>, group: Group, ty: FieldType, required: bool, empty: &'static str, optimized: bool) -> FieldInfo {
    FieldInfo { key, label, column, group, ty, required, empty, optimized }
}

use FieldType::{Bool, Text};
use Group::{Lead, Sender, System};

pub const FIELDS: [FieldInfo; 19] = [
    f("sender_first_name", "Name", Some("sender_first_name"), Sender, Text, false, "", false),
    f("ideal_customer", "Ideal customer", Some("ideal_customer"), Sender, Text, false, "", false),
    f("not_a_fit", "Not a fit", Some("not_a_fit"), Sender, Text, false, "", false),
    f("problem_solved", "Problem solved", Some("problem_solved"), Sender, Text, false, "", false),
    f("offer", "Offer", Some("offer"), Sender, Text, false, "", false),
    f("outcome", "Outcome", Some("outcome"), Sender, Text, false, "", false),
    f("proof_or_none", "Proof", Some("proof_or_none"), Sender, Text, false, "none", false),
    f("low_friction_offer", "Low friction offer", Some("low_friction_offer"), Sender, Text, false, "", false),
    f("allow_low_personalization", "Allow low personalization", Some("allow_low_personalization"), Sender, Bool, false, "false", false),
    f("first_name", "First name", Some("first_name"), Lead, Text, true, "", true),
    f("company", "Company", Some("company_name"), Lead, Text, true, "", true),
    f("title", "Job title", Some("job_title"), Lead, Text, false, "", true),
    f("industry", "Industry", Some("industry"), Lead, Text, false, "", true),
    f("website", "Website", Some("website"), Lead, Text, false, "", true),
    f("personalization_note", "Personalization note", Some("personalization_note"), Lead, Text, false, "", true),
    f("trigger", "Trigger", Some("trigger_event"), Lead, Text, false, "", true),
    f("trigger_verified", "Trigger verified", Some("trigger_verified"), Lead, Bool, false, "false", true),
    f("sequence_step", "Sequence step", None, System, Text, false, "", false),
    f("previous_angles", "Previous angles", None, System, Text, false, "", false),
];

pub fn all() -> &'static [FieldInfo] {
    &FIELDS
}

pub fn group(g: Group) -> impl Iterator<Item = &'static FieldInfo> {
    FIELDS.iter().filter(move |f| f.group == g)
}

/// The lead columns the default prompt uses, in the order of the prompt.
pub fn optimized_lead_columns() -> Vec<&'static str> {
    group(Lead).filter(|f| f.optimized).filter_map(|f| f.column).collect()
}

/// Is this lead column used by the default prompt? (`company_name`, `job_title`, `trigger_event`, ...)
pub fn is_optimized_column(column: &str) -> bool {
    group(Lead).any(|f| f.optimized && f.column == Some(column))
}

/// The prompt key of a lead column: `company_name` gives `company`.
pub fn prompt_key_of_column(column: &str) -> Option<&'static str> {
    group(Lead).find(|f| f.column == Some(column)).map(|f| f.key)
}

pub fn get(key: &str) -> Option<&'static FieldInfo> {
    FIELDS.iter().find(|f| f.key == key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eight_lead_fields_are_optimized_and_two_are_required() {
        assert_eq!(group(Lead).filter(|f| f.optimized).count(), 8);
        let req: Vec<_> = group(Lead).filter(|f| f.required).map(|f| f.key).collect();
        assert_eq!(req, ["first_name", "company"]);
        assert_eq!(group(Sender).count(), 9);
    }

    #[test]
    fn prompt_keys_map_to_the_real_columns() {
        assert_eq!(prompt_key_of_column("company_name"), Some("company"));
        assert_eq!(prompt_key_of_column("job_title"), Some("title"));
        assert_eq!(prompt_key_of_column("trigger_event"), Some("trigger"));
        assert!(is_optimized_column("website"));
        assert!(!is_optimized_column("phone"));
    }
}
