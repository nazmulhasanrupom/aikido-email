//! Read bounce reports (DSN, RFC 3464) and feedback reports (ARF, RFC 5965). Pure functions.
use super::model::BounceInfo;
use super::parse::ParsedEmail;
use regex::Regex;
use std::sync::LazyLock;

static STATUS_CODE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\b[45]\.\d{1,3}\.\d{1,3}\b").unwrap());
static VALID_STATUS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[245]\.\d{1,3}\.\d{1,3}$").unwrap());
static EMAIL: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[A-Za-z0-9._%+\-']+@[A-Za-z0-9\-]+(?:\.[A-Za-z0-9\-]+)+").unwrap());
static MESSAGE_ID_LINE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?im)^message-id:\s*(<[^>\s]+>)").unwrap());

const MAX_DIAGNOSTIC: usize = 500;

/// One recipient block of a `message/delivery-status` part.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DsnRecipient {
    pub final_recipient: Option<String>,
    pub action: Option<String>,
    pub status: Option<String>,
    pub diagnostic: Option<String>,
}

/// Header fields of one block. Folded lines are joined.
fn fields(block: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for line in block.replace("\r\n", "\n").lines() {
        if line.starts_with([' ', '\t']) {
            if let Some(last) = out.last_mut() {
                last.1.push(' ');
                last.1.push_str(line.trim());
            }
        } else if let Some((name, value)) = line.split_once(':') {
            out.push((name.trim().to_lowercase(), value.trim().to_string()));
        }
    }
    out
}

/// The part after `rfc822;` (or any `type;` prefix).
fn after_type(value: &str) -> String {
    value.split_once(';').map(|(_, v)| v).unwrap_or(value).trim().trim_matches(['<', '>']).to_string()
}

fn one_line(text: &str) -> String {
    let mut s: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if s.len() > MAX_DIAGNOSTIC {
        let mut end = MAX_DIAGNOSTIC;
        while !s.is_char_boundary(end) {
            end -= 1;
        }
        s.truncate(end);
    }
    s
}

/// The recipients in the text of a `message/delivery-status` part. The first block is about the
/// whole message (Reporting-MTA). Each next block is about one recipient.
pub fn parse_delivery_status(text: &str) -> Vec<DsnRecipient> {
    let text = text.replace("\r\n", "\n");
    let mut out = Vec::new();
    for block in text.split("\n\n").filter(|b| !b.trim().is_empty()) {
        let f = fields(block);
        let get = |name: &str| f.iter().find(|(n, _)| n == name).map(|(_, v)| v.clone());
        let final_recipient = get("final-recipient").or_else(|| get("original-recipient"));
        let (action, status) = (get("action"), get("status"));
        if final_recipient.is_none() && action.is_none() && status.is_none() {
            continue; // the per-message block
        }
        out.push(DsnRecipient {
            final_recipient: final_recipient.map(|v| after_type(&v).to_lowercase()).filter(|v| !v.is_empty()),
            action: action.map(|v| v.to_lowercase()),
            status: status.map(|v| v.trim().to_string()).filter(|v| VALID_STATUS.is_match(v)),
            diagnostic: get("diagnostic-code").map(|v| one_line(&after_type(&v))).filter(|v| !v.is_empty()),
        });
    }
    out
}

/// The status code from the body of an old-style bounce (no standard report).
pub fn status_in_text(text: &str) -> Option<String> {
    STATUS_CODE.find(text).map(|m| m.as_str().to_string())
}

/// Email addresses in the text, in order, without repeats.
pub fn addresses_in_text(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for m in EMAIL.find_iter(text) {
        let a = m.as_str().trim_matches(['.', '\'']).to_lowercase();
        if !out.contains(&a) {
            out.push(a);
        }
    }
    out
}

fn message_id_in(text: &str) -> Option<String> {
    MESSAGE_ID_LINE.captures(text).map(|c| c[1].to_string())
}

/// What a bounce report says. `candidates` are the addresses in the body of a report without
/// a `Final-Recipient`: the matcher picks the first one we really sent to.
pub fn extract_bounce(p: &ParsedEmail) -> (BounceInfo, Vec<String>) {
    let recipients = p.delivery_status.as_deref().map(parse_delivery_status).unwrap_or_default();
    // Prefer a failed or delayed recipient. A report may also list delivered ones.
    let chosen = recipients
        .iter()
        .find(|r| matches!(r.action.as_deref(), Some("failed" | "delayed")))
        .or_else(|| recipients.first())
        .cloned();
    let original_message_id = p
        .original_message_id
        .clone()
        .or_else(|| p.original_headers.as_deref().and_then(message_id_in));

    let mut info = BounceInfo { original_message_id, ..Default::default() };
    let mut candidates = Vec::new();
    if let Some(r) = chosen {
        info.recipient = r.final_recipient;
        info.status = r.status;
        info.diagnostic = r.diagnostic;
        info.action = r.action;
    }
    if info.recipient.is_none() {
        if let Some(h) = p.headers.get("x-failed-recipients") {
            info.recipient = addresses_in_text(h).into_iter().next();
        }
    }
    if info.status.is_none() {
        // Old servers: look for a code in the body.
        info.status = status_in_text(&p.text).or_else(|| p.delivery_status.as_deref().and_then(status_in_text));
    }
    if info.recipient.is_none() {
        let own = p.from_email.clone().unwrap_or_default();
        candidates = addresses_in_text(&p.text)
            .into_iter()
            .filter(|a| *a != own && !is_system_address(a))
            .collect();
    }
    (info, candidates)
}

/// Addresses of mail systems. They are never the recipient we sent to.
fn is_system_address(a: &str) -> bool {
    let local = a.split('@').next().unwrap_or_default();
    local.starts_with("mailer-daemon") || local.starts_with("postmaster") || local.starts_with("no-reply") || local.starts_with("noreply")
}

/// The recipient of a spam complaint, from the feedback report or the attached original.
pub fn extract_complaint(p: &ParsedEmail) -> BounceInfo {
    let mut info = BounceInfo { action: Some("complaint".into()), ..Default::default() };
    if let Some(report) = &p.feedback_report {
        let f = fields(report);
        let get = |name: &str| f.iter().find(|(n, _)| n == name).map(|(_, v)| v.clone());
        info.recipient = get("original-rcpt-to").and_then(|v| addresses_in_text(&v).into_iter().next());
    }
    if info.recipient.is_none() {
        if let Some(h) = &p.original_headers {
            let f = fields(h);
            info.recipient = f.iter().find(|(n, _)| n == "to").and_then(|(_, v)| addresses_in_text(v).into_iter().next());
        }
    }
    info.original_message_id = p.original_message_id.clone().or_else(|| p.original_headers.as_deref().and_then(message_id_in));
    info
}

#[cfg(test)]
mod tests {
    use super::*;

    const REPORT: &str = "Reporting-MTA: dns; googlemail.com\nArrival-Date: Mon, 19 Oct 2026 10:00:05 +0000\n\n\
        Final-Recipient: rfc822; Nobody@Prospect.example\nAction: failed\nStatus: 5.1.1\n\
        Diagnostic-Code: smtp; 550-5.1.1 The email account\n  does not exist.\n";

    #[test]
    fn reads_a_standard_report() {
        let r = parse_delivery_status(REPORT);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].final_recipient.as_deref(), Some("nobody@prospect.example"));
        assert_eq!(r[0].action.as_deref(), Some("failed"));
        assert_eq!(r[0].status.as_deref(), Some("5.1.1"));
        assert_eq!(r[0].diagnostic.as_deref(), Some("550-5.1.1 The email account does not exist."));
    }

    #[test]
    fn a_bad_status_is_dropped() {
        let r = parse_delivery_status("Final-Recipient: rfc822; a@b.com\nAction: failed\nStatus: oops\n");
        assert_eq!(r[0].status, None);
    }

    #[test]
    fn finds_codes_and_addresses_in_free_text() {
        assert_eq!(status_in_text("SMTP error: 550 5.1.1 Unknown user").as_deref(), Some("5.1.1"));
        assert_eq!(status_in_text("version 1.2.3 and 4.4.7 timeout").as_deref(), Some("4.4.7"));
        assert_eq!(status_in_text("no code here"), None);
        assert_eq!(addresses_in_text("to Jane@X.com, then <jane@x.com> and b@y.org."), ["jane@x.com", "b@y.org"]);
    }
}
