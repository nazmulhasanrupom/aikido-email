//! Checks on every output of the model. Pure functions: no DB, no network.
//!
//! 1. every slot is there and not empty        2. word count within `max_words`
//! 3. no link unless the slot allows it        4. no email address, no phone number
//! 5. no leftover placeholder                  6. no "AI talk" (a list you can edit)
//! 7. plain text only (no markdown)            8. a small spam word check (warn or fail)
//! 9. language: a light check, warning only
//!
//! Honest limit: the guard cannot prove that a fact is true. The prompt says "only use facts from the context".
//! Review mode is the real check for that.
use super::slots::AiSlot;
use regex::Regex;
use std::collections::BTreeMap;
use std::sync::OnceLock;

pub struct GuardOptions<'a> {
    pub slots: &'a [AiSlot],
    pub banned_phrases: &'a [String],
    pub spam_words: &'a [String],
    pub spam_fail: bool,
    pub language: Option<&'a str>,
}

#[derive(Debug, Default, PartialEq)]
pub struct GuardReport {
    /// The text must not be used. The reason goes back to the model for one more try.
    pub errors: Vec<String>,
    /// The text can be used, but look at it.
    pub warnings: Vec<String>,
}

impl GuardReport {
    pub fn ok(&self) -> bool {
        self.errors.is_empty()
    }
    pub fn reason(&self) -> String {
        self.errors.join(" ")
    }
}

fn re(cell: &'static OnceLock<Regex>, pattern: &str) -> &'static Regex {
    cell.get_or_init(|| Regex::new(pattern).expect("the pattern is valid"))
}

fn url_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    re(
        &R,
        r"(?i)(?:https?://|www\.)\S+|\b[a-z0-9][a-z0-9-]*(?:\.[a-z0-9-]+)*\.(?:com|net|org|io|co|ai|app|dev|biz|info|me|us|uk|de|fr|es|it|nl|ca|au|in|ru|br|xyz|tech|cloud|so|sh|gg|ly)\b",
    )
}

fn email_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    re(&R, r"[A-Za-z0-9._%+\-]+@[A-Za-z0-9.\-]+\.[A-Za-z]{2,}")
}

fn phone_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    re(&R, r"\+?\(?\d[\d\s().\-]{5,}\d")
}

fn placeholder_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    re(&R, r"(?i)\{\{|\}\}|\[[^\]\n]{1,40}\]|<[^>\n]{1,40}>|\{[A-Za-z_ ]{1,30}\}|\bxxx+\b|lorem ipsum")
}

fn markdown_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    re(&R, r"(?m)\*\*|`|^\s*#{1,6}\s|^\s*[-*\u{2022}]\s|^\s*\d+[.)]\s|\[[^\]]+\]\([^)]+\)")
}

fn has_phone(text: &str) -> bool {
    phone_re().find_iter(text).any(|m| {
        let s = m.as_str();
        let digits = s.chars().filter(char::is_ascii_digit).count();
        digits >= 9 || (digits >= 7 && (s.starts_with('+') || s.contains('(')))
    })
}

/// Check the values of all slots of a step.
pub fn check(values: &BTreeMap<String, String>, opts: &GuardOptions) -> GuardReport {
    let mut report = GuardReport::default();
    let banned = super::settings::GeneratorSettings::lowered(opts.banned_phrases);
    let spam = super::settings::GeneratorSettings::lowered(opts.spam_words);
    for slot in opts.slots {
        let key = &slot.key;
        let Some(text) = values.get(key).map(|t| t.trim()).filter(|t| !t.is_empty()) else {
            report.errors.push(format!("Slot '{key}' is empty or missing."));
            continue;
        };
        let words = text.split_whitespace().count() as u32;
        if words > slot.max_words {
            report.errors.push(format!("Slot '{key}' has {words} words. The limit is {}.", slot.max_words));
        }
        if !slot.allow_links && url_re().is_match(text) {
            report.errors.push(format!("Slot '{key}' has a link or a web address. Write none."));
        }
        if email_re().is_match(text) {
            report.errors.push(format!("Slot '{key}' has an email address. Write none."));
        }
        if has_phone(text) {
            report.errors.push(format!("Slot '{key}' has a phone number. Write none."));
        }
        if placeholder_re().is_match(text) {
            report.errors.push(format!("Slot '{key}' has a placeholder like [Name] or {{{{x}}}}. Write the real words."));
        }
        let lower = text.to_lowercase();
        if let Some(p) = banned.iter().find(|p| lower.contains(p.as_str())) {
            // The phrase itself is a setting, not the model's text, so it is safe to show.
            report.errors.push(format!("Slot '{key}' uses a banned phrase: \"{p}\"."));
        }
        if markdown_re().is_match(text) {
            report.errors.push(format!("Slot '{key}' has markdown (bold, list, heading, code or link syntax). Write plain text."));
        }
        if let Some(w) = spam.iter().find(|w| lower.contains(w.as_str())) {
            let msg = format!("Slot '{key}' has a spam word: \"{w}\".");
            if opts.spam_fail { report.errors.push(msg) } else { report.warnings.push(msg) }
        }
        if let Some(lang) = opts.language {
            if let Some(w) = language_warning(text, lang) {
                report.warnings.push(format!("Slot '{key}': {w}"));
            }
        }
    }
    report
}

// ---------- language (a light check) ----------

enum Script {
    Latin,
    Cyrillic,
    Arabic,
    Hebrew,
    Greek,
    Cjk,
    Hangul,
    Devanagari,
    Thai,
}

fn script_of(lang: &str) -> Option<Script> {
    Some(match lang {
        "en" | "es" | "fr" | "de" | "pt" | "it" | "nl" | "sv" | "da" | "no" | "nb" | "fi" | "pl" | "cs" | "tr" | "ro" | "hu" | "id" | "ms" => Script::Latin,
        "ru" | "uk" | "bg" | "sr" => Script::Cyrillic,
        "ar" | "fa" | "ur" => Script::Arabic,
        "he" => Script::Hebrew,
        "el" => Script::Greek,
        "zh" | "ja" => Script::Cjk,
        "ko" => Script::Hangul,
        "hi" => Script::Devanagari,
        "th" => Script::Thai,
        _ => return None,
    })
}

fn in_script(c: char, s: &Script) -> bool {
    let u = c as u32;
    match s {
        Script::Latin => c.is_ascii_alphabetic() || (0xC0..=0x24F).contains(&u),
        Script::Cyrillic => (0x400..=0x4FF).contains(&u),
        Script::Arabic => (0x600..=0x6FF).contains(&u),
        Script::Hebrew => (0x590..=0x5FF).contains(&u),
        Script::Greek => (0x370..=0x3FF).contains(&u),
        Script::Cjk => (0x4E00..=0x9FFF).contains(&u) || (0x3040..=0x30FF).contains(&u),
        Script::Hangul => (0xAC00..=0xD7AF).contains(&u),
        Script::Devanagari => (0x900..=0x97F).contains(&u),
        Script::Thai => (0xE00..=0xE7F).contains(&u),
    }
}

fn stop_words(lang: &str) -> Option<&'static [&'static str]> {
    Some(match lang {
        "en" => &["the", "and", "to", "of", "in", "for", "is", "on", "with", "your", "that", "this", "at", "as", "are", "you", "we", "it"],
        "es" => &["el", "la", "los", "las", "de", "y", "en", "que", "un", "una", "para", "con", "es", "por", "su", "del"],
        "fr" => &["le", "la", "les", "de", "des", "et", "en", "un", "une", "pour", "que", "est", "dans", "vous", "nous", "sur"],
        "de" => &["der", "die", "das", "und", "in", "zu", "den", "mit", "von", "ist", "für", "auf", "ein", "eine", "sie", "wir"],
        "pt" => &["o", "a", "os", "as", "de", "e", "em", "que", "um", "uma", "para", "com", "é", "por", "seu", "da"],
        "it" => &["il", "la", "le", "di", "e", "in", "che", "un", "una", "per", "con", "è", "del", "della", "sono", "nel"],
        "nl" => &["de", "het", "een", "en", "van", "in", "is", "dat", "op", "te", "voor", "met", "zijn", "je", "we", "uw"],
        _ => return None,
    })
}

/// A warning if the text does not look like the language. Needs at least 8 words to judge by stop words.
pub fn language_warning(text: &str, language: &str) -> Option<String> {
    let lang = language.split(['-', '_']).next().unwrap_or(language).to_lowercase();
    let script = script_of(&lang)?;
    let letters: Vec<char> = text.chars().filter(|c| c.is_alphabetic()).collect();
    if letters.is_empty() {
        return None;
    }
    let inside = letters.iter().filter(|c| in_script(**c, &script)).count();
    if (inside as f64) / (letters.len() as f64) < 0.8 {
        return Some(format!("the text does not look like it is written in '{lang}' (wrong script)."));
    }
    let words: Vec<String> = text.split(|c: char| !c.is_alphabetic()).filter(|w| !w.is_empty()).map(str::to_lowercase).collect();
    if let Some(list) = stop_words(&lang) {
        if words.len() >= 8 && !words.iter().any(|w| list.contains(&w.as_str())) {
            return Some(format!("the text does not look like it is written in '{lang}'."));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slot(max_words: u32, allow_links: bool) -> AiSlot {
        AiSlot { key: "s".into(), instruction: "x".into(), max_words, allow_links, fallback: None }
    }

    fn run(text: &str) -> GuardReport {
        run_with(text, &slot(40, false), "en", false)
    }

    fn run_with(text: &str, s: &AiSlot, lang: &str, spam_fail: bool) -> GuardReport {
        let values: BTreeMap<String, String> = [("s".to_string(), text.to_string())].into();
        let banned = super::super::settings::default_banned_phrases();
        let spam = super::super::settings::default_spam_words();
        let slots = [s.clone()];
        check(&values, &GuardOptions { slots: &slots, banned_phrases: &banned, spam_words: &spam, spam_fail, language: if lang.is_empty() { None } else { Some(lang) } })
    }

    fn fails(text: &str, needle: &str) {
        let r = run(text);
        assert!(r.errors.iter().any(|e| e.contains(needle)), "{text:?} should fail with {needle:?}, got {r:?}");
    }

    #[test]
    fn a_good_text_passes() {
        let r = run("Congrats on the Series B, Acme looks like it is growing fast in the logistics space.");
        assert!(r.ok(), "{r:?}");
        assert!(r.warnings.is_empty());
    }

    #[test]
    fn rule_1_a_slot_must_be_there_and_not_empty() {
        fails("", "empty or missing");
        fails("   ", "empty or missing");
        let values = BTreeMap::new();
        let slots = [slot(40, false)];
        let r = check(&values, &GuardOptions { slots: &slots, banned_phrases: &[], spam_words: &[], spam_fail: false, language: None });
        assert!(!r.ok());
    }

    #[test]
    fn rule_2_word_limit() {
        let ok = vec!["word"; 40].join(" ");
        let long = vec!["word"; 41].join(" ");
        assert!(run(&ok).ok());
        fails(&long, "41 words");
        assert!(run_with("a b c", &slot(2, false), "", false).errors[0].contains("3 words"));
    }

    #[test]
    fn rule_3_links() {
        for t in ["see https://acme.com/x", "visit www.acme.com", "go to acme.io now", "Acme.COM is great", "http://x"] {
            fails(t, "link");
        }
        for t in ["We saw Node.js in your stack.", "Your U.S. team grows.", "e.g. this"] {
            assert!(run(t).ok(), "{t}");
        }
        assert!(run_with("see https://acme.com/x", &slot(40, true), "", false).ok());
    }

    #[test]
    fn rule_4_email_and_phone() {
        fails("write to ana@acme.com", "email address");
        fails("call +44 20 7946 0958", "phone");
        fails("call 555 123 4567", "phone");
        fails("call (555) 1234", "phone");
        for t in ["Revenue grew 2024-2025 by a lot.", "Since 2019 you opened 12 offices.", "Date 2026-10-17 was great."] {
            assert!(run(t).ok(), "{t}");
        }
    }

    #[test]
    fn rule_5_placeholders() {
        for t in ["Hi {{name}}", "Hi [Name]", "at <company>", "Hi XXX", "Hello {first_name}", "lorem ipsum dolor", "a }} b"] {
            fails(t, "placeholder");
        }
        assert!(run("A normal sentence with (brackets) and 3 < 5 ok.").ok());
    }

    #[test]
    fn rule_6_ai_talk_and_the_editable_list() {
        fails("As an AI, I think so", "banned phrase");
        fails("I hope this email finds you well.", "banned phrase");
        fails("I am a Language Model", "banned phrase");
        // the list is a setting
        let values: BTreeMap<String, String> = [("s".to_string(), "Synergy is key".to_string())].into();
        let slots = [slot(40, false)];
        let banned = vec!["synergy".to_string()];
        let r = check(&values, &GuardOptions { slots: &slots, banned_phrases: &banned, spam_words: &[], spam_fail: false, language: None });
        assert!(r.errors[0].contains("synergy"));
        let none = check(&values, &GuardOptions { slots: &slots, banned_phrases: &[], spam_words: &[], spam_fail: false, language: None });
        assert!(none.ok());
    }

    #[test]
    fn rule_7_plain_text_only() {
        for t in ["This is **bold**", "# Heading", "- a bullet", "* a bullet", "1. first", "2) second", "see [here](http://x)", "use `code`", "line\n- item"] {
            let r = run(t);
            assert!(r.errors.iter().any(|e| e.contains("markdown") || e.contains("link")), "{t:?}: {r:?}");
        }
        assert!(run("A well-known 3-step plan; nothing fancy - really.").ok());
    }

    #[test]
    fn rule_8_spam_words_warn_or_fail() {
        let w = run_with("This is a guaranteed result for you", &slot(40, false), "", false);
        assert!(w.ok());
        assert!(w.warnings[0].contains("guaranteed"));
        let f = run_with("This is a guaranteed result for you", &slot(40, false), "", true);
        assert!(!f.ok());
    }

    #[test]
    fn rule_9_language_is_a_warning_only() {
        let en = "Congrats on the launch of your new product and the team that built it for you.";
        assert!(run_with(en, &slot(40, false), "en", false).warnings.is_empty());
        let es_text = "Felicidades por el lanzamiento de su nuevo producto y por el equipo que lo hizo.";
        let r = run_with(es_text, &slot(40, false), "en", false);
        assert!(r.ok());
        assert!(!r.warnings.is_empty());
        assert!(run_with(es_text, &slot(40, false), "es", false).warnings.is_empty());
        let ru = run_with("Поздравляем с запуском нового продукта вашей компании", &slot(40, false), "en", false);
        assert!(ru.warnings[0].contains("wrong script"));
        assert!(run_with("Поздравляем с запуском", &slot(40, false), "ru", false).warnings.is_empty());
        // an unknown language, and a short text, give no warning
        assert!(language_warning("whatever text here", "xx").is_none());
        assert!(language_warning("Short text", "en").is_none());
    }
}
