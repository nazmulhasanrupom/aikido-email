//! The Optimised mode. One call writes 4 emails: the primary email and 3 follow-ups.
//! The system prompt and the user prompt are plain text with `{{variables}}`. Both are editable per campaign.
//! The server adds only the output format. A missing value never fails the prompt: it is left out.
use super::sender::SenderProfile;
use crate::generator::parse::json_part;
use crate::leads::model::Lead;
use regex::Regex;
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::OnceLock;

pub const DEFAULT_SYSTEM: &str = include_str!("default_system.md");
pub const DEFAULT_USER: &str = include_str!("default_user.md");
/// The primary email and three follow-ups.
pub const EMAILS: usize = 4;
const MAX_VALUE_CHARS: usize = 1000;

/// The names the prompts can use, for the Variable menu.
pub const SENDER_VARS: [&str; 9] = [
    "sender_first_name", "ideal_customer", "not_a_fit", "problem_solved", "offer", "outcome", "proof_or_none", "low_friction_offer", "allow_low_personalization",
];
pub const LEAD_VARS: [&str; 12] = [
    "first_name", "last_name", "company_name", "job_title", "industry", "website", "personalization_note", "trigger_event", "trigger_verified", "location", "linkedin_url", "timezone",
];

fn var_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\{\{\s*([A-Za-z_][A-Za-z0-9_]*)\s*(?:\|([^}]*))?\}\}").expect("regex"))
}

fn cap(s: &str) -> String {
    s.trim().chars().take(MAX_VALUE_CHARS).collect()
}

/// Every value the prompts can use: the sender profile and the lead. Private data (email, phone, notes, tags) is not here.
pub fn vars(lead: &Lead, profile: Option<&SenderProfile>, fallback_name: &str) -> BTreeMap<String, String> {
    let mut v: BTreeMap<String, String> = BTreeMap::new();
    let mut put = |k: &str, val: &str| {
        v.insert(k.to_string(), cap(val));
    };
    let name = profile.map(|p| p.sender_first_name.trim()).filter(|n| !n.is_empty()).map(str::to_string).unwrap_or_else(|| fallback_name.split_whitespace().next().unwrap_or("").to_string());
    put("sender_first_name", &name);
    put("ideal_customer", profile.map(|p| p.ideal_customer.as_str()).unwrap_or(""));
    put("not_a_fit", profile.map(|p| p.not_a_fit.as_str()).unwrap_or(""));
    put("problem_solved", profile.map(|p| p.problem_solved.as_str()).unwrap_or(""));
    put("offer", profile.map(|p| p.offer.as_str()).unwrap_or(""));
    put("outcome", profile.map(|p| p.outcome.as_str()).unwrap_or(""));
    put("proof_or_none", profile.map(|p| p.proof_or_none.as_str()).filter(|p| !p.trim().is_empty()).unwrap_or("none"));
    put("low_friction_offer", profile.map(|p| p.low_friction_offer.as_str()).unwrap_or(""));
    put("allow_low_personalization", if profile.is_some_and(|p| p.allow_low_personalization) { "yes" } else { "no" });
    let text = |o: &Option<String>| o.clone().unwrap_or_default();
    for (k, val) in [
        ("first_name", text(&lead.first_name)),
        ("last_name", text(&lead.last_name)),
        ("company_name", text(&lead.company_name)),
        ("job_title", text(&lead.job_title)),
        ("industry", text(&lead.industry)),
        ("website", text(&lead.website)),
        ("personalization_note", text(&lead.personalization_note)),
        ("trigger_event", text(&lead.trigger_event)),
        ("location", text(&lead.location)),
        ("linkedin_url", text(&lead.linkedin_url)),
        ("timezone", text(&lead.timezone)),
    ] {
        put(k, &val);
    }
    put("trigger_verified", if lead.trigger_verified { "yes" } else { "no" });
    // short names
    for (alias, of) in [("company", "company_name"), ("title", "job_title"), ("trigger", "trigger_event")] {
        let val = v.get(of).cloned().unwrap_or_default();
        v.insert(alias.to_string(), val);
    }
    if let Some(custom) = lead.custom_fields.as_object() {
        for (k, val) in custom {
            let s = match val {
                Value::String(s) => cap(s),
                Value::Number(n) => n.to_string(),
                _ => String::new(),
            };
            if !s.is_empty() {
                v.entry(k.clone()).or_insert(s);
            }
        }
    }
    v
}

/// Fill `{{name}}` and `{{name|fallback}}`. A value that is missing or empty uses the fallback, or nothing.
/// A line whose variables are all empty is dropped, so the prompt has no "Note:" with nothing after it.
pub fn fill(template: &str, vars: &BTreeMap<String, String>) -> String {
    let mut out: Vec<String> = Vec::new();
    for line in template.lines() {
        let mut any = false;
        let mut filled = false;
        let text = var_re().replace_all(line, |c: &regex::Captures| {
            any = true;
            let val = vars.get(&c[1]).map(String::as_str).unwrap_or("").trim();
            let val = if val.is_empty() { c.get(2).map(|f| f.as_str().trim()).unwrap_or("") } else { val };
            if !val.is_empty() {
                filled = true;
            }
            val.to_string()
        });
        if any && !filled {
            continue;
        }
        out.push(text.into_owned());
    }
    out.join("\n")
}

/// Names only the Optimised prompts know: the sender fields and short names of lead fields.
pub fn is_known(name: &str) -> bool {
    SENDER_VARS.contains(&name) || LEAD_VARS.contains(&name) || ["company", "title", "trigger"].contains(&name)
}

/// The names a prompt uses.
pub fn names_in(template: &str) -> Vec<String> {
    var_re().captures_iter(template).map(|c| c[1].to_string()).collect()
}

/// The fixed end of the system prompt: the format of the answer.
pub fn rules() -> String {
    "\n\nAnswer with one JSON object only, in this format:\n\
     {\"primary\": {\"subject\": \"...\", \"body\": \"...\"}, \"followup_1\": {\"body\": \"...\"}, \"followup_2\": {\"body\": \"...\"}, \"followup_3\": {\"body\": \"...\"}}\n\
     The follow-ups go in the same thread, so they have no subject. Write plain text: no markdown, no HTML, no placeholders in brackets.\n\
     <optimized_request>{\"emails\":4}</optimized_request>"
        .to_string()
}

#[derive(Clone, Debug, PartialEq)]
pub struct Mail {
    pub subject: String,
    pub body: String,
}

fn clean(s: &str) -> String {
    s.trim().replace(" \u{2014} ", ", ").replace(" \u{2013} ", ", ").replace(['\u{2014}', '\u{2013}'], ",")
}

/// Read the answer: the primary email and three follow-ups. `Err` is a short reason for one repair try.
pub fn parse_output(text: &str) -> Result<Vec<Mail>, String> {
    let json = json_part(text).ok_or("The answer has no JSON object.")?;
    let v: Value = serde_json::from_str(json).map_err(|_| "The answer is not valid JSON.".to_string())?;
    let mut out = Vec::new();
    for (i, key) in ["primary", "followup_1", "followup_2", "followup_3"].iter().enumerate() {
        let item = v.get(*key).ok_or_else(|| format!("The JSON has no \"{key}\"."))?;
        let body = item.get("body").and_then(Value::as_str).map(clean).filter(|b| !b.is_empty()).ok_or_else(|| format!("\"{key}\" has no \"body\" text."))?;
        let subject = if i == 0 {
            item.get("subject").and_then(Value::as_str).map(clean).filter(|s| !s.is_empty() && !s.contains('\n')).ok_or_else(|| "\"primary\" needs a one line \"subject\".".to_string())?
        } else {
            String::new()
        };
        out.push(Mail { subject, body });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    #[test]
    fn fill_uses_values_fallbacks_and_drops_empty_lines() {
        let v = map(&[("first_name", "Ana"), ("note", ""), ("company", "Acme")]);
        let out = fill("Hi {{first_name}} at {{company}}\nNote: {{note}}\nTitle: {{title|there}}\nPlain line\nA {{ nope }} B {{first_name}}", &v);
        assert_eq!(out, "Hi Ana at Acme\nTitle: there\nPlain line\nA  B Ana");
    }

    #[test]
    fn the_default_prompts_use_only_known_names() {
        let known: Vec<&str> = SENDER_VARS.iter().chain(LEAD_VARS.iter()).copied().collect();
        for n in names_in(DEFAULT_USER).iter().chain(names_in(DEFAULT_SYSTEM).iter()) {
            assert!(known.contains(&n.as_str()), "{n}");
        }
    }

    #[test]
    fn vars_have_no_private_data_and_cut_long_values() {
        let mut lead = crate::prompting::fields::test_lead();
        lead.personalization_note = Some("x".repeat(5000));
        let v = vars(&lead, None, "Dana Lee");
        assert_eq!(v["sender_first_name"], "Dana");
        assert_eq!(v["company"], v["company_name"]);
        assert_eq!(v["trigger_verified"], "yes");
        assert_eq!(v["personalization_note"].chars().count(), 1000);
        assert_eq!(v["team_size"], "40");
        let all = format!("{v:?}");
        for secret in ["secret@example.com", "+123456", "private note", "vip"] {
            assert!(!all.contains(secret), "{secret}");
        }
    }

    #[test]
    fn output_is_read_and_a_bad_shape_is_an_error() {
        let ok = r#"```json
{"primary":{"subject":"new depot","body":"Hi Ana — hello"},"followup_1":{"body":"a"},"followup_2":{"body":"b"},"followup_3":{"body":"c"}}
```"#;
        let m = parse_output(ok).unwrap();
        assert_eq!(m.len(), 4);
        assert_eq!(m[0].subject, "new depot");
        assert_eq!(m[0].body, "Hi Ana, hello");
        assert!(m[1..].iter().all(|x| x.subject.is_empty()));
        assert!(parse_output("nope").is_err());
        assert!(parse_output(r#"{"primary":{"subject":"a","body":"b"}}"#).unwrap_err().contains("followup_1"));
        assert!(parse_output(r#"{"primary":{"subject":"","body":"b"},"followup_1":{"body":"a"},"followup_2":{"body":"b"},"followup_3":{"body":"c"}}"#).is_err());
        assert!(parse_output(r#"{"primary":{"subject":"s","body":"b"},"followup_1":{"body":""},"followup_2":{"body":"b"},"followup_3":{"body":"c"}}"#).is_err());
    }
}
