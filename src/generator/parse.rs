//! Reads the JSON the model answers: `{ "slots": { "opener": "text", ... } }`. Every slot key of the step must be
//! there, and nothing else. The error text goes back to the model once (a repair try), so it never holds the answer.
use serde_json::Value;
use std::collections::BTreeMap;

/// The `{...}` part of an answer: the model may wrap it in a code fence or add a sentence around it.
pub fn json_part(text: &str) -> Option<&str> {
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    (end > start).then(|| &text[start..=end])
}

pub fn parse_slots(text: &str, keys: &[&str]) -> Result<BTreeMap<String, String>, String> {
    let json = json_part(text).ok_or("The answer has no JSON object.")?;
    let v: Value = serde_json::from_str(json).map_err(|_| "The answer is not valid JSON.".to_string())?;
    let slots = v.get("slots").and_then(|s| s.as_object()).ok_or("The JSON has no \"slots\" object.")?;
    let mut out = BTreeMap::new();
    for key in keys {
        match slots.get(*key) {
            Some(Value::String(s)) => {
                out.insert((*key).to_string(), s.trim().to_string());
            }
            Some(_) => return Err(format!("The slot '{key}' must be a text.")),
            None => return Err(format!("The slot '{key}' is missing.")),
        }
    }
    if let Some(extra) = slots.keys().find(|k| !keys.contains(&k.as_str())) {
        return Err(format!("The slot '{extra}' is not one of the slots of this step."));
    }
    if v.as_object().is_some_and(|o| o.len() > 1) {
        return Err("The JSON must hold only \"slots\".".into());
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_fenced_and_chatty_answers_parse() {
        let want = |m: &BTreeMap<String, String>| m["a"].clone();
        assert_eq!(want(&parse_slots(r#"{"slots":{"a":" hi "}}"#, &["a"]).unwrap()), "hi");
        assert_eq!(want(&parse_slots("```json\n{\"slots\":{\"a\":\"x\"}}\n```", &["a"]).unwrap()), "x");
        assert_eq!(want(&parse_slots("Sure! {\"slots\":{\"a\":\"y\"}} Done.", &["a"]).unwrap()), "y");
    }

    #[test]
    fn bad_answers_say_why_without_echoing_the_text() {
        let e = |t: &str, k: &[&str]| parse_slots(t, k).unwrap_err();
        assert!(e("no json here", &["a"]).contains("no JSON"));
        assert!(e("{not json}", &["a"]).contains("not valid JSON"));
        assert!(e(r#"{"a":"x"}"#, &["a"]).contains("slots"));
        assert!(e(r#"{"slots":{"b":"x"}}"#, &["a"]).contains("'a' is missing"));
        assert!(e(r#"{"slots":{"a":"x","b":"SECRET-TEXT"}}"#, &["a"]).contains("'b' is not one"));
        assert!(!e(r#"{"slots":{"a":"x","b":"SECRET-TEXT"}}"#, &["a"]).contains("SECRET-TEXT"));
        assert!(e(r#"{"slots":{"a":5}}"#, &["a"]).contains("must be a text"));
        assert!(e(r#"{"slots":{"a":"x"},"note":"hi"}"#, &["a"]).contains("only"));
    }
}
