//! Raw MIME to `ParsedEmail`. Uses `mail-parser`. No network, no DB.
use chrono::{DateTime, Utc};
use mail_parser::{MessageParser, MimeHeaders, PartType};
use regex::Regex;
use std::collections::BTreeMap;
use std::sync::LazyLock;

static ANGLE_ID: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"<[^<>\s]+>").unwrap());

/// Header names to values. Names are lowercase. Folded lines are joined.
/// A header that appears twice keeps both values, joined with `, `.
pub type HeaderMap = BTreeMap<String, String>;

#[derive(Clone, Debug, Default)]
pub struct ParsedEmail {
    pub from_email: Option<String>,
    pub from_name: Option<String>,
    pub to: Vec<String>,
    pub cc: Vec<String>,
    pub subject: Option<String>,
    pub date: Option<DateTime<Utc>>,
    /// With angle brackets: `<id@host>`.
    pub message_id: Option<String>,
    pub in_reply_to: Option<String>,
    pub references: Vec<String>,
    pub headers: HeaderMap,
    /// The text body. HTML-only mail is converted to text.
    pub text: String,
    /// `report-type` of a `multipart/report`.
    pub report_type: Option<String>,
    /// Text of the `message/delivery-status` part.
    pub delivery_status: Option<String>,
    /// Text of the `message/feedback-report` part.
    pub feedback_report: Option<String>,
    /// `Message-ID` of the attached original (`message/rfc822`).
    pub original_message_id: Option<String>,
    /// Headers of the attached original (`message/rfc822` or `text/rfc822-headers`).
    pub original_headers: Option<String>,
}

#[derive(Debug, PartialEq)]
pub struct ParseError(pub String);

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// All `<id>` tokens of a header value. A value with no angle brackets counts as one id.
pub fn message_ids(value: &str) -> Vec<String> {
    let ids: Vec<String> = ANGLE_ID.find_iter(value).map(|m| m.as_str().to_string()).collect();
    if ids.is_empty() {
        let v = value.trim();
        if v.is_empty() || v.contains(char::is_whitespace) {
            return vec![];
        }
        return vec![format!("<{v}>")];
    }
    ids
}

fn unfold(v: &str) -> String {
    v.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn header_map(msg: &mail_parser::Message<'_>) -> HeaderMap {
    let mut map = HeaderMap::new();
    for (name, value) in msg.headers_raw() {
        let (name, value) = (name.to_lowercase(), unfold(value));
        match map.get_mut(&name) {
            Some(old) => {
                old.push_str(", ");
                old.push_str(&value);
            }
            None => {
                map.insert(name, value);
            }
        }
    }
    map
}

/// Headers of a header-only block (what IMAP `BODY.PEEK[HEADER]` returns).
pub fn parse_header_block(raw: &[u8]) -> HeaderMap {
    MessageParser::default().parse_headers(raw).map(|m| header_map(&m)).unwrap_or_default()
}

fn addresses(a: Option<&mail_parser::Address<'_>>) -> Vec<String> {
    a.map(|a| a.iter().filter_map(|x| x.address.as_deref()).map(|s| s.trim().to_lowercase()).collect()).unwrap_or_default()
}

fn html_to_text(html: &str) -> String {
    html2text::from_read(html.as_bytes(), 100).unwrap_or_else(|_| html.to_string())
}

/// Parse one raw email. An error means the bytes are not an email we can read.
pub fn parse(raw: &[u8]) -> Result<ParsedEmail, ParseError> {
    let msg = MessageParser::default().parse(raw).ok_or_else(|| ParseError("not a valid email".into()))?;
    let mut headers = header_map(&msg);
    // The parser is forgiving: junk bytes can look like a header. Keep only real header names.
    headers.retain(|name, _| !name.is_empty() && name.bytes().all(|b| b.is_ascii_graphic()));
    if headers.is_empty() {
        return Err(ParseError("the email has no headers".into()));
    }

    let first_from = msg.from().and_then(|a| a.first());
    if first_from.and_then(|a| a.address.as_deref()).is_none() {
        return Err(ParseError("the email has no From address".into()));
    }
    let mut p = ParsedEmail {
        from_email: first_from.and_then(|a| a.address.as_deref()).map(|s| s.trim().to_lowercase()),
        from_name: first_from.and_then(|a| a.name.as_deref()).map(|s| s.trim().to_string()).filter(|s| !s.is_empty()),
        to: addresses(msg.to()),
        cc: addresses(msg.cc()),
        subject: msg.subject().map(|s| s.trim().to_string()),
        date: msg.date().and_then(|d| DateTime::from_timestamp(d.to_timestamp(), 0)),
        message_id: headers.get("message-id").and_then(|v| message_ids(v).into_iter().next()),
        in_reply_to: headers.get("in-reply-to").and_then(|v| message_ids(v).into_iter().next()),
        references: headers.get("references").map(|v| message_ids(v)).unwrap_or_default(),
        report_type: msg
            .content_type()
            .filter(|ct| ct.ctype().eq_ignore_ascii_case("multipart") && ct.subtype().is_some_and(|s| s.eq_ignore_ascii_case("report")))
            .and_then(|ct| ct.attribute("report-type"))
            .map(|s| s.to_lowercase()),
        headers,
        ..Default::default()
    };

    // The body text: the first text/plain part, else the first HTML part as text.
    let (mut plain, mut html) = (None::<String>, None::<String>);
    for &id in &msg.text_body {
        match msg.parts.get(id as usize).map(|part| &part.body) {
            Some(PartType::Text(t)) if plain.is_none() => plain = Some(t.to_string()),
            Some(PartType::Html(h)) if html.is_none() => html = Some(h.to_string()),
            _ => {}
        }
    }
    p.text = plain.or_else(|| html.map(|h| html_to_text(&h))).unwrap_or_default();

    // The parts of a report.
    for part in &msg.parts {
        let Some(ct) = part.content_type() else { continue };
        let kind = format!("{}/{}", ct.ctype().to_lowercase(), ct.subtype().unwrap_or_default().to_lowercase());
        match kind.as_str() {
            "message/delivery-status" => p.delivery_status = part.text_contents().map(str::to_string),
            "message/feedback-report" => p.feedback_report = part.text_contents().map(str::to_string),
            "message/rfc822" | "text/rfc822-headers" => {
                if let PartType::Message(inner) = &part.body {
                    p.original_message_id = inner.message_id().map(|id| format!("<{id}>"));
                    let block: Vec<String> = inner.headers_raw().map(|(n, v)| format!("{n}: {}", unfold(v))).collect();
                    p.original_headers = Some(block.join("\n"));
                } else if let Some(text) = part.text_contents() {
                    let block = text.replace("\r\n", "\n");
                    let head = block.split("\n\n").next().unwrap_or_default();
                    p.original_message_id = head
                        .lines()
                        .find(|l| l.to_lowercase().starts_with("message-id:"))
                        .and_then(|l| message_ids(l.split_once(':').map(|x| x.1).unwrap_or_default()).into_iter().next());
                    p.original_headers = Some(head.to_string());
                }
            }
            _ => {}
        }
    }
    Ok(p)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_message_ids() {
        assert_eq!(message_ids("<a@x> <b@y>\n <c@z>"), ["<a@x>", "<b@y>", "<c@z>"]);
        assert_eq!(message_ids("a@x"), ["<a@x>"]);
        assert!(message_ids("two words").is_empty());
        assert!(message_ids("").is_empty());
    }

    #[test]
    fn parses_a_simple_email() {
        let raw = b"From: Jane Prospect <Jane@Prospect.example>\r\nTo: sam@acme.example\r\nSubject: Re: Hello\r\n\
            Message-ID: <abc@prospect.example>\r\nIn-Reply-To: <sent-1@acme.example>\r\nReferences: <old@x> <sent-1@acme.example>\r\n\
            Date: Mon, 19 Oct 2026 14:12:00 +0000\r\nContent-Type: text/plain\r\n\r\nHello there\r\n";
        let p = parse(raw).unwrap();
        assert_eq!(p.from_email.as_deref(), Some("jane@prospect.example"));
        assert_eq!(p.from_name.as_deref(), Some("Jane Prospect"));
        assert_eq!(p.to, ["sam@acme.example"]);
        assert_eq!(p.message_id.as_deref(), Some("<abc@prospect.example>"));
        assert_eq!(p.in_reply_to.as_deref(), Some("<sent-1@acme.example>"));
        assert_eq!(p.references, ["<old@x>", "<sent-1@acme.example>"]);
        assert_eq!(p.text.trim(), "Hello there");
        assert_eq!(p.date.unwrap().to_rfc3339(), "2026-10-19T14:12:00+00:00");
        assert_eq!(p.headers["subject"], "Re: Hello");
    }

    #[test]
    fn garbage_is_an_error() {
        assert!(parse(b"").is_err());
        assert!(parse(&[0xff, 0xfe, 0x00, 0x01]).is_err());
        assert!(parse(b"Subject: no sender\n\nbody").is_err());
    }

    #[test]
    fn header_block_works() {
        let h = parse_header_block(b"From: a@b.com\r\nSubject: Hi\r\n there\r\nAuto-Submitted: auto-replied\r\n\r\n");
        assert_eq!(h["subject"], "Hi there");
        assert_eq!(h["auto-submitted"], "auto-replied");
    }
}
