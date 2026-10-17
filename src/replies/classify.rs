//! `classify(&ParsedEmail, &ReplySettings) -> Classification`. Pure: no DB, no network.
//! First match wins. All text checks ignore case.
use super::dsn;
use super::model::{BounceInfo, Classification, Label};
use super::parse::ParsedEmail;
use super::quote;
use super::settings::ReplySettings;

/// The status code groups of a bounce.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum BounceClass {
    Hard,
    Soft,
    Block,
}

/// RFC 3463 status to a bounce class. `5.7.x` = blocked by policy. `5.2.2` = mailbox full.
/// `4.x.x` = temporary. Any other `5.x.x` = permanent.
pub fn bounce_class(status: &str) -> Option<BounceClass> {
    let mut parts = status.split('.');
    let (class, subject, detail) = (parts.next()?, parts.next()?, parts.next()?);
    match (class, subject, detail) {
        ("5", "7", _) => Some(BounceClass::Block),
        ("5", "2", "2") => Some(BounceClass::Soft),
        ("4", _, _) => Some(BounceClass::Soft),
        ("5", _, _) => Some(BounceClass::Hard),
        _ => None,
    }
}

fn contains_any(haystack: &str, patterns: &[String]) -> bool {
    let h = haystack.to_lowercase();
    patterns.iter().map(|p| p.trim().to_lowercase()).filter(|p| !p.is_empty()).any(|p| h.contains(&p))
}

/// `keyword` appears in `text` as whole words: the characters around it are not letters or digits.
pub fn contains_whole_words(text: &str, keyword: &str) -> bool {
    let text = text.to_lowercase().replace(['’', '‘'], "'");
    let kw = keyword.trim().to_lowercase().replace(['’', '‘'], "'");
    if kw.is_empty() {
        return false;
    }
    let is_word = |c: char| c.is_alphanumeric() || c == '_';
    text.match_indices(&kw).any(|(i, m)| {
        let before = text[..i].chars().next_back();
        let after = text[i + m.len()..].chars().next();
        !before.is_some_and(is_word) && !after.is_some_and(is_word)
    })
}

fn is_bounce_like(p: &ParsedEmail, s: &ReplySettings) -> bool {
    if p.delivery_status.is_some() || p.report_type.as_deref() == Some("delivery-status") {
        return true;
    }
    let local_part = p.from_email.as_deref().unwrap_or_default();
    if local_part.starts_with("mailer-daemon@") || local_part.starts_with("postmaster@") {
        return true;
    }
    p.subject.as_deref().is_some_and(|subj| contains_any(subj, &s.bounce_subject_patterns))
}

fn is_auto_reply(p: &ParsedEmail, s: &ReplySettings) -> Option<String> {
    if let Some(v) = p.headers.get("auto-submitted") {
        if !v.trim().eq_ignore_ascii_case("no") {
            return Some(format!("Auto-Submitted: {}", v.trim()));
        }
    }
    if p.headers.contains_key("x-autoreply") || p.headers.contains_key("x-autorespond") {
        return Some("X-Autoreply header".into());
    }
    if let Some(v) = p.headers.get("precedence") {
        let v = v.trim().to_lowercase();
        if matches!(v.as_str(), "auto_reply" | "auto-reply" | "bulk" | "junk") {
            return Some(format!("Precedence: {v}"));
        }
    }
    if p.subject.as_deref().is_some_and(|subj| contains_any(subj, &s.auto_reply_subject_patterns)) {
        return Some("auto-reply subject".into());
    }
    None
}

fn classify_bounce(p: &ParsedEmail) -> Classification {
    let (info, _candidates) = dsn::extract_bounce(p);
    let (label, reason) = match (info.action.as_deref(), info.status.as_deref()) {
        (Some("delayed"), _) => (Label::Delayed, "Action: delayed".to_string()),
        (Some("delivered" | "relayed" | "expanded"), _) => (Label::Delayed, "delivery notice, not a failure".to_string()),
        (_, Some(status)) => match bounce_class(status) {
            Some(BounceClass::Block) => (Label::BounceBlock, format!("status {status}: blocked by the receiving server")),
            Some(BounceClass::Soft) => (Label::BounceSoft, format!("status {status}: temporary problem")),
            Some(BounceClass::Hard) => (Label::BounceHard, format!("status {status}: address does not exist or refuses mail")),
            None => (Label::BounceSoft, "unparsed".to_string()),
        },
        (_, None) => (Label::BounceSoft, "unparsed".to_string()),
    };
    Classification { label, reason, bounce: Some(info) }
}

pub fn classify(p: &ParsedEmail, s: &ReplySettings) -> Classification {
    // 1. Spam complaint.
    if p.feedback_report.is_some() || p.report_type.as_deref() == Some("feedback-report") {
        return Classification {
            label: Label::Complaint,
            reason: "feedback report".into(),
            bounce: Some(dsn::extract_complaint(p)),
        };
    }
    // 2. Bounce report, or a bounce-like sender or subject.
    if is_bounce_like(p, s) {
        return classify_bounce(p);
    }
    // 3. Auto-reply.
    if let Some(why) = is_auto_reply(p, s) {
        return Classification { label: Label::AutoReply, reason: why, bounce: None };
    }
    // 4. Unsubscribe: a keyword in the new text, and a short text.
    let new_text = quote::strip_quoted(&p.text);
    let words = new_text.split_whitespace().count();
    if words <= s.unsubscribe_max_words as usize {
        if let Some(k) = s.unsubscribe_keywords.iter().find(|k| contains_whole_words(&new_text, k)) {
            return Classification { label: Label::Unsubscribe, reason: format!("keyword '{}'", k.trim()), bounce: None };
        }
    }
    // 5. Anything else.
    Classification { label: Label::Reply, reason: "linked mail, no other rule matched".into(), bounce: None::<BounceInfo> }
}

/// Parse and classify raw MIME. A parse error is not fatal: the email gets the label `unknown`,
/// is stored without actions, and the batch goes on.
pub fn classify_raw(raw: &[u8], s: &ReplySettings) -> (Option<ParsedEmail>, Classification) {
    match super::parse::parse(raw) {
        Ok(p) => {
            let c = classify(&p, s);
            (Some(p), c)
        }
        Err(e) => (None, Classification { label: Label::Unknown, reason: format!("parse error: {e}"), bounce: None }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_classes() {
        assert_eq!(bounce_class("5.1.1"), Some(BounceClass::Hard));
        assert_eq!(bounce_class("5.1.10"), Some(BounceClass::Hard));
        assert_eq!(bounce_class("5.4.4"), Some(BounceClass::Hard));
        assert_eq!(bounce_class("5.2.2"), Some(BounceClass::Soft));
        assert_eq!(bounce_class("5.2.1"), Some(BounceClass::Hard));
        assert_eq!(bounce_class("5.7.1"), Some(BounceClass::Block));
        assert_eq!(bounce_class("5.7.510"), Some(BounceClass::Block));
        assert_eq!(bounce_class("4.4.7"), Some(BounceClass::Soft));
        assert_eq!(bounce_class("4.2.2"), Some(BounceClass::Soft));
        assert_eq!(bounce_class("2.0.0"), None);
        assert_eq!(bounce_class("junk"), None);
    }

    #[test]
    fn whole_word_matching() {
        assert!(contains_whole_words("Please UNSUBSCRIBE me", "unsubscribe"));
        assert!(contains_whole_words("opt-out now", "opt-out"));
        assert!(contains_whole_words("please don’t contact me", "don't contact"));
        assert!(contains_whole_words("remove me.", "remove me"));
        assert!(!contains_whole_words("we resubscribe often", "subscribe"));
        assert!(!contains_whole_words("unsubscribed", "unsubscribe"));
        assert!(!contains_whole_words("anything", ""));
    }
}
