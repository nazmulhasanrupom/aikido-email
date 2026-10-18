//! The checks the code runs on every mail of the default prompt. The model's own SELF CHECK is not trusted.
//! Also the fixed server rules (`rules`) and the parser of the model answer.
use crate::generator::parse::json_part;
use regex::Regex;
use serde_json::Value;
use std::sync::OnceLock;

pub const PRIMARY_MAX_WORDS: usize = 80;
pub const FOLLOWUP_MAX_WORDS: usize = 60;
pub const SUBJECT_MIN_WORDS: usize = 2;
pub const SUBJECT_MAX_WORDS: usize = 5;

/// The same list as `# BANNED PHRASES` in the Core block (a test keeps them equal). Lower case.
pub const BANNED_PHRASES: [&str; 26] = [
    "i hope this email finds you well",
    "hope you're doing well",
    "i wanted to reach out",
    "i came across your",
    "quick question",
    "touch base",
    "circle back",
    "game changer",
    "game-changing",
    "synergy",
    "cutting-edge",
    "state-of-the-art",
    "revolutionary",
    "best-in-class",
    "world-class",
    "unlock",
    "supercharge",
    "leverage",
    "limited time",
    "act now",
    "once in a lifetime",
    "guaranteed",
    "100% free",
    "no obligation",
    "just checking in",
    "bumping this",
];

pub fn max_words(step: i32) -> usize {
    if step == 1 { PRIMARY_MAX_WORDS } else { FOLLOWUP_MAX_WORDS }
}

/// Steps 2 and 3 are replies in the same thread: no subject. Step 1 and step 4 or more have one.
pub fn subject_allowed(step: i32) -> bool {
    step == 1 || step >= 4
}

/// The fixed block at the end of the system prompt.
pub fn rules(step: i32) -> String {
    let subject = if subject_allowed(step) {
        format!("\"subject\" is {SUBJECT_MIN_WORDS} to {SUBJECT_MAX_WORDS} words, all lowercase.")
    } else {
        "This mail is a reply in the same thread: set \"subject\" to an empty string.".to_string()
    };
    let links = match step {
        1 | 2 => "Write no link, no email address and no phone number.".to_string(),
        3 => "Write a link only if SENDER proof_or_none contains it. Write no email address and no phone number.".to_string(),
        _ => "Write no link, no email address and no phone number.".to_string(),
    };
    format!(
        "Rules for your answer (fixed, they come from the server):\n\
         - Answer with one JSON object only, with the keys of # OUTPUT.\n\
         - {subject}\n\
         - \"body\" is plain text, under {} words. No markdown, no HTML.\n\
         - {links}\n\
         - Write no placeholder such as [Name] or {{{{x}}}}. Write the real words.\n\
         - Do not write a signature or an opt-out line: they are added for you. End the body with the sender first name only.\n\
         <optimized_request>{{\"step\":{step}}}</optimized_request>",
        max_words(step)
    )
}

// ---------- the answer ----------

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ModelOut {
    /// As the model wrote it, trimmed.
    pub status: String,
    pub subject: String,
    pub body: String,
    pub angle_used: String,
    pub used_trigger: bool,
    pub claims_check: Vec<String>,
    pub hold_reason: String,
}

impl ModelOut {
    pub fn is_hold(&self) -> bool {
        self.status.eq_ignore_ascii_case("HOLD")
    }
    pub fn is_ok(&self) -> bool {
        self.status.eq_ignore_ascii_case("OK")
    }
}

/// Read the answer. `Err` is a short reason for the repair try.
pub fn parse_out(text: &str) -> Result<ModelOut, String> {
    let json = json_part(text).ok_or("The answer has no JSON object.")?;
    let v: Value = serde_json::from_str(json).map_err(|_| "The answer is not valid JSON.".to_string())?;
    let s = |k: &str| v.get(k).and_then(Value::as_str).map(str::trim).unwrap_or_default().to_string();
    let status = v.get("status").and_then(Value::as_str).map(str::trim).ok_or("The JSON has no \"status\" text.")?.to_string();
    let claims_check = match v.get("claims_check") {
        Some(Value::Array(a)) => a.iter().map(|c| c.as_str().map(str::to_string).unwrap_or_else(|| c.to_string())).collect(),
        _ => Vec::new(),
    };
    Ok(ModelOut {
        status,
        subject: s("subject"),
        body: s("body"),
        angle_used: s("angle_used"),
        used_trigger: v.get("used_trigger").and_then(Value::as_bool).unwrap_or(false),
        claims_check,
        hold_reason: s("hold_reason"),
    })
}

// ---------- the checks ----------

pub struct CheckInput<'a> {
    pub step: i32,
    pub previous_angles: &'a [String],
    /// `proof_or_none` of the sender profile. Step 3 may link to a URL in it.
    pub proof: &'a str,
    /// More banned phrases (the generator settings). Case does not matter.
    pub extra_banned: &'a [String],
}

fn link_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?i)(?:https?://\S+|www\.\S+|\b[a-z0-9][a-z0-9-]*\.(?:com|net|org|io|co|ai|dev|app|me|us|uk|ca|de|biz|info|xyz|ly)\b\S*|\b[\w.+-]+@[\w-]+\.[\w.-]+)").expect("link regex")
    })
}

pub fn words(s: &str) -> usize {
    s.split_whitespace().count()
}

fn norm_angle(s: &str) -> String {
    s.trim().to_lowercase()
}

/// The links of a text, with the closing punctuation cut off.
pub fn links_of(text: &str) -> Vec<String> {
    link_re().find_iter(text).map(|m| m.as_str().trim_end_matches(['.', ',', ';', ':', ')', '!', '?', '"', '\'']).to_string()).collect()
}

/// The names of the checks, in the order the report shows them.
pub const CHECK_NAMES: [&str; 8] = ["status", "word_limit", "dashes", "banned_phrases", "subject", "links", "angle", "placeholders"];

#[derive(Clone, Debug, PartialEq, serde::Serialize, utoipa::ToSchema)]
pub struct CheckResult {
    pub name: &'static str,
    pub passed: bool,
    /// Why it failed. Empty when it passed.
    pub detail: String,
}

/// Every check with its result. A HOLD answer passes (it has no text to check).
pub fn report(o: &ModelOut, i: &CheckInput) -> Vec<CheckResult> {
    let mut by: Vec<(&'static str, Vec<String>)> = CHECK_NAMES.iter().map(|n| (*n, Vec::new())).collect();
    let mut add = |name: &str, msg: String| {
        if let Some(slot) = by.iter_mut().find(|(n, _)| *n == name) {
            slot.1.push(msg);
        }
    };
    if !(o.is_ok() || o.is_hold()) {
        add("status", format!("status must be OK or HOLD (got '{}')", o.status));
    } else if o.is_ok() {
        let limit = max_words(i.step);
        let n = words(&o.body);
        if n == 0 {
            add("word_limit", "body is empty".into());
        } else if n >= limit {
            add("word_limit", format!("body has {n} words, it must be under {limit}"));
        }
        if [&o.subject, &o.body].iter().any(|t| t.contains('\u{2014}') || t.contains('\u{2013}')) {
            add("dashes", "contains an em dash or en dash, use a comma or a full stop".into());
        }
        let hay = format!("{}\n{}", o.subject, o.body).to_lowercase().replace('\u{2019}', "'");
        for p in BANNED_PHRASES.iter().copied().chain(i.extra_banned.iter().map(String::as_str)) {
            let p = p.trim().to_lowercase();
            if !p.is_empty() && hay.contains(&p) {
                add("banned_phrases", format!("banned phrase: '{p}'"));
            }
        }
        if subject_allowed(i.step) {
            let sw = words(&o.subject);
            if !(SUBJECT_MIN_WORDS..=SUBJECT_MAX_WORDS).contains(&sw) {
                add("subject", format!("subject has {sw} words, it must have {SUBJECT_MIN_WORDS} to {SUBJECT_MAX_WORDS}"));
            }
            if o.subject != o.subject.to_lowercase() {
                add("subject", "subject must be all lowercase".into());
            }
        } else if !o.subject.is_empty() {
            add("subject", "subject must be empty for this step (same thread)".into());
        }
        let found: Vec<String> = links_of(&o.body).into_iter().chain(links_of(&o.subject)).collect();
        if i.step == 3 {
            let proof = i.proof.to_lowercase();
            for l in &found {
                if !proof.contains(&l.to_lowercase()) {
                    add("links", format!("link not in Proof: {l}"));
                }
            }
        } else if !found.is_empty() {
            add("links", format!("links are not allowed in this step: {}", found.join(", ")));
        }
        if o.angle_used.trim().is_empty() {
            add("angle", "angle_used is empty".into());
        } else if i.previous_angles.iter().any(|a| norm_angle(a) == norm_angle(&o.angle_used)) {
            add("angle", "angle_used repeats an angle in previous_angles".into());
        }
        if o.body.contains("{{") || o.body.contains("}}") || o.body.contains('[') || o.body.contains(']') || o.subject.contains("{{") || o.subject.contains('[') {
            add("placeholders", "a placeholder is left in the text".into());
        }
    }
    by.into_iter().map(|(name, msgs)| CheckResult { name, passed: msgs.is_empty(), detail: msgs.join("; ") }).collect()
}

/// Every failed check, as a short line. Empty means the mail passed.
pub fn run(o: &ModelOut, i: &CheckInput) -> Vec<String> {
    let mut out = Vec::new();
    for r in report(o, i) {
        if !r.passed {
            out.extend(r.detail.split("; ").map(str::to_string));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(step: i32) -> ModelOut {
        ModelOut {
            status: "OK".into(),
            subject: if subject_allowed(step) { "new depot plans".into() } else { String::new() },
            body: "Hi Ana, saw the new depot. Routing is slow at that size. Worth a look?\nDana".into(),
            angle_used: format!("angle {step}"),
            ..Default::default()
        }
    }

    fn input<'a>(step: i32, prev: &'a [String], proof: &'a str) -> CheckInput<'a> {
        CheckInput { step, previous_angles: prev, proof, extra_banned: &[] }
    }

    #[test]
    fn a_good_mail_passes_in_every_step() {
        for s in 1..=6 {
            assert!(run(&ok(s), &input(s, &[], "none")).is_empty(), "step {s}: {:?}", run(&ok(s), &input(s, &[], "none")));
        }
    }

    #[test]
    fn status_must_be_ok_or_hold() {
        let mut o = ok(1);
        o.status = "MAYBE".into();
        assert_eq!(run(&o, &input(1, &[], "none")).len(), 1);
        o.status = "hold".into();
        assert!(run(&o, &input(1, &[], "none")).is_empty());
    }

    #[test]
    fn word_limits_are_80_and_60() {
        let mut o = ok(1);
        o.body = vec!["word"; 80].join(" ");
        assert!(run(&o, &input(1, &[], "none")).iter().any(|f| f.contains("80")));
        o.body = vec!["word"; 79].join(" ");
        assert!(run(&o, &input(1, &[], "none")).is_empty());
        let mut o = ok(2);
        o.body = vec!["word"; 60].join(" ");
        assert!(run(&o, &input(2, &[], "none")).iter().any(|f| f.contains("60")));
    }

    #[test]
    fn dashes_are_refused_in_subject_and_body() {
        let mut o = ok(1);
        o.body = "Hi \u{2014} there".into();
        assert!(run(&o, &input(1, &[], "none")).iter().any(|f| f.contains("dash")));
        let mut o = ok(1);
        o.subject = "new \u{2013} depot".into();
        assert!(run(&o, &input(1, &[], "none")).iter().any(|f| f.contains("dash")));
    }

    #[test]
    fn banned_phrases_are_found_in_any_case() {
        let mut o = ok(1);
        o.body = "We can LEVERAGE your data. I hope this email finds you well".into();
        let f = run(&o, &input(1, &[], "none"));
        assert!(f.iter().any(|x| x.contains("leverage")) && f.iter().any(|x| x.contains("finds you well")));
        let extra = vec!["depot".to_string()];
        let mut i = input(1, &[], "none");
        i.extra_banned = &extra;
        assert!(run(&ok(1), &i).iter().any(|x| x.contains("depot")));
    }

    #[test]
    fn subject_rules_follow_the_step() {
        for s in [2, 3] {
            let mut o = ok(s);
            o.subject = "hello there".into();
            assert!(run(&o, &input(s, &[], "none")).iter().any(|f| f.contains("empty")), "step {s}");
        }
        for s in [1, 4, 7] {
            let mut o = ok(s);
            o.subject = "Hello There".into();
            assert!(run(&o, &input(s, &[], "none")).iter().any(|f| f.contains("lowercase")), "step {s}");
            o.subject = "one".into();
            assert!(run(&o, &input(s, &[], "none")).iter().any(|f| f.contains("words")), "step {s}");
            o.subject = "one two three four five six".into();
            assert!(run(&o, &input(s, &[], "none")).iter().any(|f| f.contains("words")), "step {s}");
            o.subject = String::new();
            assert!(!run(&o, &input(s, &[], "none")).is_empty(), "step {s}");
        }
    }

    #[test]
    fn links_are_refused_except_a_proof_link_in_step_three() {
        for s in [1, 2, 4, 5] {
            let mut o = ok(s);
            o.body = "See https://acme.test/case now".into();
            assert!(run(&o, &input(s, &[], "https://acme.test/case")).iter().any(|f| f.contains("link")), "step {s}");
            o.body = "See acme.com now".into();
            assert!(run(&o, &input(s, &[], "none")).iter().any(|f| f.contains("link")), "step {s}");
        }
        let mut o = ok(3);
        o.body = "Case: https://acme.test/case.".into();
        assert!(run(&o, &input(3, &[], "Case study https://acme.test/case")).is_empty());
        assert!(run(&o, &input(3, &[], "none")).iter().any(|f| f.contains("Proof")));
        o.body = "Case: https://other.test/x".into();
        assert!(run(&o, &input(3, &[], "Case study https://acme.test/case")).iter().any(|f| f.contains("Proof")));
    }

    #[test]
    fn the_angle_must_be_new_and_not_empty() {
        let mut o = ok(2);
        o.angle_used = String::new();
        assert!(run(&o, &input(2, &[], "none")).iter().any(|f| f.contains("empty")));
        o.angle_used = "  Cost Angle ".into();
        let prev = vec!["cost angle".to_string()];
        assert!(run(&o, &input(2, &prev, "none")).iter().any(|f| f.contains("repeats")));
    }

    #[test]
    fn placeholders_are_found() {
        for body in ["Hi {{first_name}}", "Hi [Name]"] {
            let mut o = ok(1);
            o.body = body.into();
            assert!(run(&o, &input(1, &[], "none")).iter().any(|f| f.contains("placeholder")), "{body}");
        }
    }

    #[test]
    fn the_answer_is_parsed_and_bad_json_is_an_error() {
        let o = parse_out("```json\n{\"status\":\"OK\",\"subject\":\"a b\",\"body\":\"hi\",\"angle_used\":\"x\",\"used_trigger\":true,\"claims_check\":[\"c1\",{\"k\":1}],\"hold_reason\":\"\"}\n```").unwrap();
        assert!(o.is_ok() && o.used_trigger && o.claims_check.len() == 2);
        assert!(parse_out("not json").is_err());
        assert!(parse_out("{\"body\":\"x\"}").is_err());
    }

    #[test]
    fn rules_name_the_step() {
        assert!(rules(1).contains("2 to 5 words") && rules(1).contains("under 80"));
        assert!(rules(2).contains("empty string") && rules(2).contains("under 60"));
        assert!(rules(3).contains("proof_or_none"));
        assert!(rules(5).contains("<optimized_request>{\"step\":5}"));
    }
}
