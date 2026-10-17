//! The hash of a step. It covers the text and the slots. When it changes, the text that was written for the
//! old step is `stale`.
use super::slots::AiSlot;
use sha2::{Digest, Sha256};

pub fn step_hash(subject: &str, body_text: Option<&str>, body_html: Option<&str>, slots: &[AiSlot]) -> String {
    let mut h = Sha256::new();
    for part in [subject, body_text.unwrap_or(""), body_html.unwrap_or("")] {
        h.update((part.len() as u64).to_le_bytes());
        h.update(part.as_bytes());
    }
    // `AiSlot` serializes with a fixed field order, so the bytes are the same every time.
    h.update(serde_json::to_vec(slots).unwrap_or_default());
    let digest = h.finalize();
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slot(instruction: &str) -> AiSlot {
        AiSlot { key: "a".into(), instruction: instruction.into(), max_words: 40, allow_links: false, fallback: None }
    }

    #[test]
    fn same_step_same_hash_and_any_change_moves_it() {
        let base = step_hash("S", Some("body {{ai:a}}"), None, &[slot("x")]);
        assert_eq!(base, step_hash("S", Some("body {{ai:a}}"), None, &[slot("x")]));
        assert_ne!(base, step_hash("S2", Some("body {{ai:a}}"), None, &[slot("x")]));
        assert_ne!(base, step_hash("S", Some("body {{ai:a}} "), None, &[slot("x")]));
        assert_ne!(base, step_hash("S", Some("body {{ai:a}}"), Some("<p>"), &[slot("x")]));
        assert_ne!(base, step_hash("S", Some("body {{ai:a}}"), None, &[slot("y")]));
        // the border between two texts counts
        assert_ne!(step_hash("ab", Some("c"), None, &[]), step_hash("a", Some("bc"), None, &[]));
    }
}
