//! The template values of a lead. Decision A (live variables): at send time the current lead fields win over
//! the snapshot saved in `enrollments.variables`.
use super::fields::FieldDefs;
use super::model::Lead;
use serde_json::{Map, Value};

/// The non-empty fields of a lead as template values: the text fields and the custom fields.
/// Private notes are not included.
pub fn lead_vars(lead: &Lead) -> Map<String, Value> {
    let mut out = Map::new();
    let text = [
        ("first_name", &lead.first_name),
        ("last_name", &lead.last_name),
        ("company_name", &lead.company_name),
        ("job_title", &lead.job_title),
        ("phone", &lead.phone),
        ("website", &lead.website),
        ("linkedin_url", &lead.linkedin_url),
        ("location", &lead.location),
        ("timezone", &lead.timezone),
    ];
    for (k, v) in text {
        if let Some(v) = v.as_deref().filter(|v| !v.trim().is_empty()) {
            out.insert(k.to_string(), Value::String(v.to_string()));
        }
    }
    if let Some(custom) = lead.custom_fields.as_object() {
        for (k, v) in custom {
            if v.as_str().is_some_and(|s| !s.trim().is_empty()) {
                out.insert(k.clone(), v.clone());
            }
        }
    }
    out
}

/// The values to render a step with, lowest priority first:
/// 1. the `fallback` of each custom field,
/// 2. the snapshot saved at enroll time,
/// 3. the current fields of the lead (when the lead still exists).
pub fn render_variables(snapshot: &Value, lead: Option<&Lead>, defs: &FieldDefs) -> Value {
    let mut out = Map::new();
    for def in defs.by_key.values() {
        if let Some(f) = def.fallback.as_deref().filter(|f| !f.trim().is_empty()) {
            out.insert(def.key.clone(), Value::String(f.to_string()));
        }
    }
    match snapshot {
        Value::Object(m) => {
            for (k, v) in m {
                if !v.is_null() {
                    out.insert(k.clone(), v.clone());
                }
            }
        }
        // a damaged snapshot is reported by `build_vars`
        other if !other.is_null() => return other.clone(),
        _ => {}
    }
    if let Some(lead) = lead {
        out.extend(lead_vars(lead));
    }
    Value::Object(out)
}

/// What to save in `enrollments.variables`: the lead's fields, with the caller's own values on top.
pub fn enroll_snapshot(explicit: &Value, lead: Option<&Lead>) -> Value {
    let mut out = lead.map(lead_vars).unwrap_or_default();
    if let Value::Object(m) = explicit {
        for (k, v) in m {
            out.insert(k.clone(), v.clone());
        }
        return Value::Object(out);
    }
    if explicit.is_null() { Value::Object(out) } else { explicit.clone() }
}
