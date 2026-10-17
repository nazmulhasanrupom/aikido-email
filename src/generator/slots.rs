//! The AI slots of a step: `{{ai:opener}}` in the text, and an entry in `sequence_steps.ai_slots`.
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

pub const DEFAULT_MAX_WORDS: u32 = 40;

fn default_max_words() -> u32 {
    DEFAULT_MAX_WORDS
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct AiSlot {
    /// Letters, digits and `_`, starts with a letter. Used as `{{ai:key}}`.
    pub key: String,
    /// What the AI must write here. 1 to 1000 characters.
    pub instruction: String,
    /// 1 to 200. Default 40.
    #[serde(default = "default_max_words")]
    pub max_words: u32,
    /// `true`: the text may hold a link. Default `false`.
    #[serde(default)]
    pub allow_links: bool,
    /// Used when no AI text is available. Up to 500 characters.
    #[serde(default)]
    pub fallback: Option<String>,
}

fn valid_key(key: &str) -> bool {
    let mut chars = key.chars();
    key.len() <= 40 && chars.next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_') && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Check a list of slots. Returns them cleaned (trimmed text, empty fallback removed).
pub fn validate(slots: &[AiSlot]) -> Result<Vec<AiSlot>, String> {
    if slots.len() > 10 {
        return Err("A step has at most 10 ai_slots.".into());
    }
    let mut out: Vec<AiSlot> = Vec::new();
    for s in slots {
        if !valid_key(&s.key) {
            return Err(format!("'{}' is not a valid slot key. Use 1 to 40 letters, digits and _, starting with a letter.", s.key));
        }
        if out.iter().any(|o| o.key == s.key) {
            return Err(format!("The slot key '{}' is used twice.", s.key));
        }
        let instruction = s.instruction.trim().to_string();
        if instruction.is_empty() || instruction.chars().count() > 1000 {
            return Err(format!("Slot '{}': instruction must be 1 to 1000 characters.", s.key));
        }
        if !(1..=200).contains(&s.max_words) {
            return Err(format!("Slot '{}': max_words must be 1 to 200.", s.key));
        }
        let fallback = s.fallback.as_deref().map(str::trim).filter(|f| !f.is_empty()).map(String::from);
        if fallback.as_ref().is_some_and(|f| f.chars().count() > 500) {
            return Err(format!("Slot '{}': fallback must be up to 500 characters.", s.key));
        }
        out.push(AiSlot { key: s.key.clone(), instruction, max_words: s.max_words, allow_links: s.allow_links, fallback });
    }
    Ok(out)
}

/// Read the slots saved in a step. A damaged value gives an empty list.
pub fn from_json(v: &serde_json::Value) -> Vec<AiSlot> {
    serde_json::from_value(v.clone()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slot(key: &str) -> AiSlot {
        AiSlot { key: key.into(), instruction: "Write one line.".into(), max_words: 30, allow_links: false, fallback: Some(" Hi ".into()) }
    }

    #[test]
    fn a_good_slot_is_cleaned() {
        let v = validate(&[slot("opener")]).unwrap();
        assert_eq!(v[0].fallback.as_deref(), Some("Hi"));
        assert_eq!(validate(&[AiSlot { fallback: Some("  ".into()), ..slot("a") }]).unwrap()[0].fallback, None);
    }

    #[test]
    fn bad_slots_are_refused() {
        assert!(validate(&[slot("1abc")]).is_err());
        assert!(validate(&[slot("a b")]).is_err());
        assert!(validate(&[slot("a"), slot("a")]).unwrap_err().contains("twice"));
        assert!(validate(&[AiSlot { instruction: " ".into(), ..slot("a") }]).is_err());
        assert!(validate(&[AiSlot { max_words: 0, ..slot("a") }]).is_err());
        assert!(validate(&[AiSlot { max_words: 201, ..slot("a") }]).is_err());
        assert!(validate(&[AiSlot { fallback: Some("x".repeat(501)), ..slot("a") }]).is_err());
        let many: Vec<AiSlot> = (0..11).map(|i| slot(&format!("k{i}"))).collect();
        assert!(validate(&many).is_err());
    }

    #[test]
    fn defaults_apply_when_reading_json() {
        let s: Vec<AiSlot> = serde_json::from_value(serde_json::json!([{ "key": "a", "instruction": "x" }])).unwrap();
        assert_eq!(s[0].max_words, 40);
        assert!(!s[0].allow_links);
    }
}
