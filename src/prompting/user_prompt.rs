//! The user prompt of the default mail prompt. The template is fixed and the server fills it.
//! The JSON is built with `serde_json`, never by joining strings, so quotes and line breaks stay valid.
use super::sender::{self, SenderProfile};
use crate::leads::model::Lead;
use serde_json::{Map, Value, json};

/// One text value of the prompt is cut at this many characters.
pub const MAX_VALUE_CHARS: usize = 1000;
/// The `extra` object is cut at this many characters.
pub const MAX_EXTRA_CHARS: usize = 3000;

fn cap(s: &str) -> String {
    s.trim().chars().take(MAX_VALUE_CHARS).collect()
}

fn text(v: &Option<String>) -> String {
    v.as_deref().map(cap).unwrap_or_default()
}

/// The required lead fields that are empty, by prompt key. The mail is held with `missing_required` and no model call.
pub fn missing_required(lead: &Lead) -> Vec<&'static str> {
    let mut out = Vec::new();
    if lead.first_name.as_deref().is_none_or(|v| v.trim().is_empty()) {
        out.push("first_name");
    }
    if lead.company_name.as_deref().is_none_or(|v| v.trim().is_empty()) {
        out.push("company");
    }
    out
}

/// Other non empty lead fields and the custom fields. Private and system data is never here:
/// email, phone, id, notes, tags, states, source, timestamps, suppression and Reoon data.
fn extra_of(lead: &Lead) -> Map<String, Value> {
    let mut all: Vec<(String, String)> = Vec::new();
    for (k, v) in [("last_name", &lead.last_name), ("linkedin_url", &lead.linkedin_url), ("location", &lead.location), ("timezone", &lead.timezone)] {
        if let Some(v) = v.as_deref().map(cap).filter(|v| !v.is_empty()) {
            all.push((k.to_string(), v));
        }
    }
    if let Some(custom) = lead.custom_fields.as_object() {
        let mut keys: Vec<&String> = custom.keys().collect();
        keys.sort();
        for k in keys {
            let v = match &custom[k] {
                Value::String(s) => cap(s),
                Value::Number(n) => n.to_string(),
                Value::Bool(b) => b.to_string(),
                _ => String::new(),
            };
            if !v.is_empty() {
                all.push((k.clone(), v));
            }
        }
    }
    let mut out = Map::new();
    let mut used = 0usize;
    for (k, v) in all {
        let size = k.chars().count() + v.chars().count() + 6;
        if used + size > MAX_EXTRA_CHARS {
            break;
        }
        used += size;
        out.insert(k, Value::String(v));
    }
    out
}

/// The LEAD block: the 8 prompt keys, `sequence_step`, `previous_angles` and `extra`.
pub fn lead_json(lead: &Lead, step: i32, previous_angles: &[String]) -> Value {
    json!({
        "first_name": text(&lead.first_name),
        "company": text(&lead.company_name),
        "title": text(&lead.job_title),
        "industry": text(&lead.industry),
        "website": text(&lead.website),
        "personalization_note": text(&lead.personalization_note),
        "trigger": text(&lead.trigger_event),
        "trigger_verified": lead.trigger_verified,
        "sequence_step": step,
        "previous_angles": previous_angles,
        "extra": Value::Object(extra_of(lead)),
    })
}

pub fn sender_json(profile: Option<&SenderProfile>, fallback_name: &str) -> Value {
    sender::prompt_json(profile, fallback_name)
}

/// The whole user prompt. `failures` is the list of failed checks when this is the second try.
pub fn build(sender: &Value, lead: &Value, step: i32, failures: &[String]) -> String {
    let pretty = |v: &Value| serde_json::to_string_pretty(v).unwrap_or_else(|_| "{}".into());
    let mut out = format!("SENDER\n{}\n\nLEAD\n{}\n\nWrite the mail for sequence_step {step}. Answer with the JSON object only.", pretty(sender), pretty(lead));
    if !failures.is_empty() {
        out.push_str("\n\nYour last answer failed these checks. Write the mail again and fix them:\n");
        for f in failures {
            out.push_str("- ");
            out.push_str(f);
            out.push('\n');
        }
    }
    out
}

/// A lead for tests of this module and of the generation flow.
#[cfg(test)]
pub fn test_lead() -> Lead {
    use crate::leads::model::{ContactState, LeadSource, Verification};
    use chrono::Utc;
    use uuid::Uuid;
    Lead {
            id: Uuid::nil(),
            email: "secret@example.com".into(),
            email_domain: "example.com".into(),
            first_name: Some("Ana".into()),
            last_name: Some("Silva".into()),
            company_name: Some("Acme \"Co\"".into()),
            job_title: Some("CTO".into()),
            phone: Some("+123456".into()),
            website: Some("https://acme.test".into()),
            linkedin_url: None,
            location: None,
            timezone: None,
            custom_fields: json!({ "team_size": "40", "empty": "" }),
            tags: vec!["vip".into()],
            contact_state: ContactState::New,
            interest: None,
            verification: Verification::SyntaxOk,
            source: LeadSource::Manual,
            source_ref: None,
            notes: Some("private note".into()),
            last_contacted_at: None,
            last_replied_at: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            industry: Some("Logistics\nand more".into()),
            personalization_note: None,
            trigger_event: Some("Opened a new depot".into()),
            trigger_verified: true,
            verification_status: None,
            verification_score: None,
            verification_checked_at: None,
        }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lead() -> Lead {
        test_lead()
    }

    #[test]
    fn the_lead_block_uses_prompt_keys_and_real_types() {
        let v = lead_json(&lead(), 2, &["a1".to_string()]);
        assert_eq!(v["company"], "Acme \"Co\"");
        assert_eq!(v["title"], "CTO");
        assert_eq!(v["trigger"], "Opened a new depot");
        assert_eq!(v["trigger_verified"], true);
        assert_eq!(v["personalization_note"], "");
        assert_eq!(v["sequence_step"], 2);
        assert_eq!(v["previous_angles"][0], "a1");
        assert_eq!(v["extra"]["team_size"], "40");
        assert_eq!(v["extra"]["last_name"], "Silva");
        assert!(v["extra"].get("empty").is_none());
    }

    #[test]
    fn private_fields_never_reach_the_prompt() {
        let prompt = build(&sender_json(None, "Dana"), &lead_json(&lead(), 1, &[]), 1, &[]);
        for secret in ["secret@example.com", "example.com", "+123456", "private note", "vip", "syntax_ok", "notes", "email"] {
            assert!(!prompt.contains(secret), "found {secret}");
        }
    }

    #[test]
    fn quotes_and_line_breaks_stay_valid_json() {
        let prompt = build(&sender_json(None, "Dana"), &lead_json(&lead(), 1, &[]), 1, &[]);
        let lead_part = prompt.split("LEAD\n").nth(1).unwrap().split("\n\nWrite").next().unwrap();
        let v: Value = serde_json::from_str(lead_part).unwrap();
        assert_eq!(v["industry"], "Logistics\nand more");
    }

    #[test]
    fn long_values_and_extra_are_capped() {
        let mut l = lead();
        l.personalization_note = Some("x".repeat(5000));
        for i in 0..20 {
            l.custom_fields.as_object_mut().unwrap().insert(format!("f{i:02}"), Value::String("y".repeat(900)));
        }
        let v = lead_json(&l, 1, &[]);
        assert_eq!(v["personalization_note"].as_str().unwrap().chars().count(), MAX_VALUE_CHARS);
        assert!(serde_json::to_string(&v["extra"]).unwrap().chars().count() <= MAX_EXTRA_CHARS + 200);
    }

    #[test]
    fn required_fields_are_checked() {
        let mut l = lead();
        assert!(missing_required(&l).is_empty());
        l.first_name = Some("  ".into());
        l.company_name = None;
        assert_eq!(missing_required(&l), ["first_name", "company"]);
    }

    #[test]
    fn the_second_try_lists_the_failures() {
        let p = build(&sender_json(None, ""), &lead_json(&lead(), 1, &[]), 1, &["too_long".to_string()]);
        assert!(p.contains("- too_long"));
    }
}
