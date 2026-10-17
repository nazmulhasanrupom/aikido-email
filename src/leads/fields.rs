//! Custom field definitions (`lead_fields`) and the check of custom values.
use super::model::{FieldKind, LeadField, RESERVED_KEYS};
use serde_json::{Map, Value};
use sqlx::PgConnection;
use std::collections::HashMap;

pub const MAX_FIELDS: i64 = 50;
pub const MAX_VALUE_CHARS: usize = 1000;

/// All field definitions, by key.
#[derive(Clone, Debug, Default)]
pub struct FieldDefs {
    pub by_key: HashMap<String, LeadField>,
}

impl FieldDefs {
    pub async fn load(conn: &mut PgConnection) -> Result<Self, sqlx::Error> {
        let rows: Vec<LeadField> = sqlx::query_as("SELECT key, label, kind, fallback, created_at FROM lead_fields ORDER BY key").fetch_all(conn).await?;
        Ok(Self { by_key: rows.into_iter().map(|f| (f.key.clone(), f)).collect() })
    }
}

/// The names of the sequences that still need the field `key`, so it must not be deleted. A sequence counts when
/// it has an open enrollment (also when the sequence is archived: it can be resumed), or when its campaign is
/// `active` or `paused` (the feeder will enroll more leads), and one of its steps uses `{{key}}`.
/// Live variables: a step reads the field when it is sent.
pub async fn sequences_using(conn: &mut PgConnection, key: &str) -> Result<Vec<String>, sqlx::Error> {
    let rows: Vec<(String, i32, String, Option<String>, Option<String>, String)> = sqlx::query_as(
        "SELECT s.name, st.step_number, st.subject, st.body_text, st.body_html, st.ai_slots::text
           FROM sequences s JOIN sequence_steps st ON st.sequence_id = s.id
          WHERE EXISTS (SELECT 1 FROM enrollments e WHERE e.sequence_id = s.id AND e.status IN ('active', 'waiting_send', 'paused'))
             OR EXISTS (SELECT 1 FROM campaigns c WHERE c.sequence_id = s.id AND c.status IN ('active', 'paused'))
          ORDER BY s.name, st.step_number",
    )
    .fetch_all(conn)
    .await?;
    let mut names: Vec<String> = Vec::new();
    for (name, _, subject, text, html, ai_slots) in rows {
        let Ok(t) = crate::followups::template::StepTemplates::parse(&subject, text.as_deref(), html.as_deref()) else { continue };
        if t.used().iter().any(|n| n == key) && !names.contains(&name) {
            names.push(name);
            continue;
        }
        // The instruction and the fallback of an AI slot may use `{{key}}` too (they are text for the AI, not templates,
        // but the author means the field). The JSON is searched as text.
        let slots: Vec<crate::generator::slots::AiSlot> = serde_json::from_str(&ai_slots).unwrap_or_default();
        let in_slots = slots.iter().any(|s| {
            [Some(s.instruction.as_str()), s.fallback.as_deref()].into_iter().flatten().any(|text| {
                crate::followups::template::StepTemplates::parse(text, None, None).is_ok_and(|t| t.used().iter().any(|n| n == key))
            })
        });
        if in_slots && !names.contains(&name) {
            names.push(name);
        }
    }
    Ok(names)
}

/// 1 to 40 characters, lower snake case, starts with a letter, not the name of a built-in field.
pub fn check_key(key: &str) -> Result<(), String> {
    let mut chars = key.chars();
    let shape_ok = !key.is_empty()
        && key.len() <= 40
        && chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
    if !shape_ok {
        return Err(format!("'{key}' is not a valid key. Use 1 to 40 characters: lower case letters, digits and _, starting with a letter."));
    }
    if RESERVED_KEYS.contains(&key) {
        return Err(format!("'{key}' is the name of a built-in field."));
    }
    Ok(())
}

/// Check one value against the kind. Returns the trimmed value.
pub fn check_value(kind: FieldKind, raw: &str) -> Result<String, String> {
    let v = raw.trim();
    if v.chars().count() > MAX_VALUE_CHARS {
        return Err(format!("a value is longer than {MAX_VALUE_CHARS} characters"));
    }
    match kind {
        FieldKind::Text => {}
        FieldKind::Number => {
            if v.parse::<f64>().map(|n| !n.is_finite()).unwrap_or(true) {
                return Err(format!("'{v}' is not a number"));
            }
        }
        FieldKind::Url => {
            let lower = v.to_lowercase();
            if !(lower.starts_with("http://") || lower.starts_with("https://")) || v.contains(char::is_whitespace) {
                return Err(format!("'{v}' is not a web address (http:// or https://)"));
            }
        }
        FieldKind::Date => {
            if chrono::NaiveDate::parse_from_str(v, "%Y-%m-%d").is_err() {
                return Err(format!("'{v}' is not a date (YYYY-MM-DD)"));
            }
        }
    }
    Ok(v.to_string())
}

/// Turn a JSON value into the text we store. `null` and empty text mean "no value" (`None`).
pub fn value_text(v: &Value) -> Result<Option<String>, String> {
    match v {
        Value::Null => Ok(None),
        Value::String(s) => Ok(Some(s.trim().to_string()).filter(|s| !s.is_empty())),
        Value::Number(n) => Ok(Some(n.to_string())),
        Value::Bool(b) => Ok(Some(b.to_string())),
        _ => Err("a custom field value must be text, a number or true/false".into()),
    }
}

/// Check a `custom_fields` object from a caller. Keys must exist. Returns only keys that have a value.
/// With `keep_nulls`, a key with no value comes back as `Value::Null` (PATCH uses it to remove the key).
pub fn clean_custom(defs: &FieldDefs, input: &Map<String, Value>, keep_nulls: bool) -> Result<Map<String, Value>, String> {
    let mut out = Map::new();
    for (key, raw) in input {
        let Some(def) = defs.by_key.get(key) else {
            return Err(format!("unknown custom field '{key}'. Create it first in /v1/leads/fields."));
        };
        match value_text(raw).map_err(|e| format!("{key}: {e}"))? {
            Some(text) => {
                out.insert(key.clone(), Value::String(check_value(def.kind, &text).map_err(|e| format!("{key}: {e}"))?));
            }
            None if keep_nulls => {
                out.insert(key.clone(), Value::Null);
            }
            None => {}
        }
    }
    Ok(out)
}

/// Check and clean a list of tags: trim, lower case, 1 to 50 characters, no commas, no repeats, up to 20.
pub fn clean_tags(tags: &[String]) -> Result<Vec<String>, String> {
    let mut out: Vec<String> = Vec::new();
    for t in tags {
        let t = t.trim().to_lowercase();
        if t.is_empty() {
            continue;
        }
        if t.chars().count() > 50 || t.contains([',', '\n', '\r']) {
            return Err(format!("tag '{t}' must be 1 to 50 characters with no comma"));
        }
        if !out.contains(&t) {
            out.push(t);
        }
    }
    if out.len() > 20 {
        return Err("a lead holds up to 20 tags".into());
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn keys() {
        assert!(check_key("pain_point").is_ok());
        for bad in ["", "Pain", "1abc", "a-b", "email", "first_name", "tags", &"a".repeat(41)] {
            assert!(check_key(bad).is_err(), "{bad}");
        }
        assert!(check_key(&"a".repeat(40)).is_ok());
    }

    #[test]
    fn values() {
        assert_eq!(check_value(FieldKind::Number, " 12.5 ").unwrap(), "12.5");
        assert!(check_value(FieldKind::Number, "abc").is_err());
        assert!(check_value(FieldKind::Number, "NaN").is_err());
        assert!(check_value(FieldKind::Url, "https://a.io/x").is_ok());
        assert!(check_value(FieldKind::Url, "javascript:alert(1)").is_err());
        assert!(check_value(FieldKind::Date, "2026-10-17").is_ok());
        assert!(check_value(FieldKind::Date, "17/10/2026").is_err());
        assert!(check_value(FieldKind::Text, &"x".repeat(1001)).is_err());
        assert!(check_value(FieldKind::Text, &"x".repeat(1000)).is_ok());
    }

    #[test]
    fn custom_values() {
        let mut defs = FieldDefs::default();
        defs.by_key.insert(
            "mrr".into(),
            LeadField { key: "mrr".into(), label: "MRR".into(), kind: FieldKind::Number, fallback: None, created_at: chrono::Utc::now() },
        );
        let m = |v: Value| v.as_object().unwrap().clone();
        assert_eq!(clean_custom(&defs, &m(json!({ "mrr": 300 })), false).unwrap()["mrr"], json!("300"));
        assert!(clean_custom(&defs, &m(json!({ "mrr": "" })), false).unwrap().is_empty());
        assert_eq!(clean_custom(&defs, &m(json!({ "mrr": null })), true).unwrap()["mrr"], Value::Null);
        assert!(clean_custom(&defs, &m(json!({ "nope": "x" })), false).unwrap_err().contains("unknown custom field"));
        assert!(clean_custom(&defs, &m(json!({ "mrr": [1] })), false).is_err());
    }

    #[test]
    fn tags() {
        assert_eq!(clean_tags(&[" SaaS ".into(), "saas".into(), "".into(), "b2b".into()]).unwrap(), vec!["saas", "b2b"]);
        assert!(clean_tags(&["a,b".into()]).is_err());
        let many: Vec<String> = (0..21).map(|i| format!("t{i}")).collect();
        assert!(clean_tags(&many).is_err());
    }
}
