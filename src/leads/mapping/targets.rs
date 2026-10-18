//! The list of targets a column can go to. The AI picks from this list only. Edit the descriptions here.
use crate::leads::fields::FieldDefs;

/// (key, description). The keys are the built-in lead columns an import can fill.
pub const BUILTINS: &[(&str, &str)] = &[
    ("email", "the email address of the person (exactly one column)"),
    ("first_name", "the first name of the person"),
    ("last_name", "the last name (family name) of the person"),
    ("company_name", "the name of the company the person works at"),
    ("job_title", "the job title or role of the person"),
    ("phone", "the phone number of the person"),
    ("website", "the website of the company"),
    ("linkedin_url", "the LinkedIn profile address of the person"),
    ("location", "the city, region or country of the person"),
    ("timezone", "an IANA time zone name like Asia/Dhaka"),
    ("notes", "free notes about the person"),
    ("industry", "the industry or sector of the company"),
    ("personalization_note", "a fact about the person or company that an email can use"),
    ("trigger_event", "a recent event about the person or company (news, launch, hire, funding, post)"),
    ("trigger_verified", "yes or no: was the trigger event checked by a person"),
    ("tags", "labels for the person (many columns may go here)"),
];

pub fn is_builtin(key: &str) -> bool {
    BUILTINS.iter().any(|(k, _)| *k == key)
}

#[derive(Clone, Debug)]
pub struct TargetInfo {
    pub key: String,
    pub description: String,
}

/// Built-in columns, then every custom field.
pub fn list(defs: &FieldDefs) -> Vec<TargetInfo> {
    let mut out: Vec<TargetInfo> = BUILTINS.iter().map(|(k, d)| TargetInfo { key: (*k).into(), description: (*d).into() }).collect();
    let mut custom: Vec<_> = defs.by_key.values().collect();
    custom.sort_by(|a, b| a.key.cmp(&b.key));
    for f in custom {
        out.push(TargetInfo { key: f.key.clone(), description: format!("custom field \"{}\" (kind {})", f.label, f.kind.as_str()) });
    }
    out
}
