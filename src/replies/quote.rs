//! Remove old quoted text from a reply. Pure function.
//! Cut the text at the first line that starts a quote and keep what is above it.
use regex::Regex;
use std::sync::LazyLock;

/// Max size of the saved reply text.
pub const MAX_REPLY_TEXT_BYTES: usize = 10 * 1024;

/// "On <date> <name> wrote:" and its translations. The line start and the line end are checked
/// on the joined text, because mail clients wrap this line over 2 or 3 lines.
static ATTRIBUTION_START: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^\s*(on|am|le|el|il|em)\s").unwrap());
static ATTRIBUTION_END: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)(wrote|a écrit|escribió|ha scritto|escreveu)\s*:\s*$|schrieb\b[^:]{0,200}:\s*$").unwrap());
static ORIGINAL_MESSAGE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^\s*-{2,}\s*(original message|ursprüngliche nachricht|message d'origine|mensaje original)\s*-{2,}\s*$").unwrap());
static UNDERSCORES: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\s*_{20,}\s*$").unwrap());
static OUTLOOK_FROM: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^\s*\**\s*(from|von|de)\s*:\**\s").unwrap());
static OUTLOOK_SENT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^\s*\**\s*(sent|gesendet|envoyé|enviado)\s*:").unwrap());
static OUTLOOK_SUBJECT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^\s*\**\s*(subject|betreff|objet|asunto)\s*:").unwrap());

/// The longest "wrote:" line we join (in lines).
const MAX_WRAPPED_LINES: usize = 3;

fn is_attribution(lines: &[&str], i: usize) -> bool {
    if !ATTRIBUTION_START.is_match(lines[i]) {
        return false;
    }
    let mut joined = String::new();
    for line in lines.iter().skip(i).take(MAX_WRAPPED_LINES) {
        if !joined.is_empty() {
            joined.push(' ');
        }
        joined.push_str(line.trim());
        if ATTRIBUTION_END.is_match(&joined) {
            return true;
        }
        // A blank line ends a paragraph: the attribution cannot continue past it.
        if line.trim().is_empty() {
            return false;
        }
    }
    false
}

fn is_outlook_header(lines: &[&str], i: usize) -> bool {
    if !OUTLOOK_FROM.is_match(lines[i]) {
        return false;
    }
    let next = &lines[i + 1..lines.len().min(i + 5)];
    next.iter().any(|l| OUTLOOK_SENT.is_match(l)) && next.iter().any(|l| OUTLOOK_SUBJECT.is_match(l))
}

fn starts_quote(lines: &[&str], i: usize) -> bool {
    let line = lines[i];
    line.trim_start().starts_with('>')
        || ORIGINAL_MESSAGE.is_match(line)
        || UNDERSCORES.is_match(line)
        || is_attribution(lines, i)
        || is_outlook_header(lines, i)
}

/// The new text of a reply, without the old quoted text. Trimmed, at most 10 KB.
pub fn strip_quoted(text: &str) -> String {
    let text = text.replace("\r\n", "\n").replace('\r', "\n");
    let lines: Vec<&str> = text.split('\n').collect();
    let cut = (0..lines.len()).find(|&i| starts_quote(&lines, i)).unwrap_or(lines.len());
    clip(lines[..cut].join("\n").trim())
}

fn clip(text: &str) -> String {
    if text.len() <= MAX_REPLY_TEXT_BYTES {
        return text.to_string();
    }
    let mut end = MAX_REPLY_TEXT_BYTES;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_text_without_a_quote() {
        assert_eq!(strip_quoted("Hello\nthere\n"), "Hello\nthere");
    }

    #[test]
    fn cuts_at_a_quote_marker() {
        assert_eq!(strip_quoted("Yes.\n\n> old text\n> more"), "Yes.");
        assert_eq!(strip_quoted("Yes.\n   > indented old text"), "Yes.");
    }

    #[test]
    fn cuts_at_on_wrote() {
        assert_eq!(strip_quoted("Sounds good.\n\nOn Mon, Oct 19, 2026 at 10:00 AM Sam <s@x.com> wrote:\n> hi"), "Sounds good.");
    }

    #[test]
    fn cuts_a_wrapped_on_wrote_line() {
        let t = "Thanks.\n\nOn Mon, Oct 19, 2026 at 10:00 AM Sam Sender\n<sender@acme.example> wrote:\n\nold text without markers";
        assert_eq!(strip_quoted(t), "Thanks.");
        let t3 = "Thanks.\nOn Mon, Oct 19, 2026 at 10:00 AM\nSam Sender\n<sender@acme.example> wrote:\nold";
        assert_eq!(strip_quoted(t3), "Thanks.");
    }

    #[test]
    fn cuts_translations() {
        assert_eq!(strip_quoted("Ja.\nAm 19.10.2026 um 10:00 schrieb Sam <s@x.com>:\nalt"), "Ja.");
        assert_eq!(strip_quoted("Oui.\nLe 19 oct. 2026 à 10:00, Sam <s@x.com> a écrit :\nvieux"), "Oui.");
        assert_eq!(strip_quoted("Si.\nEl lun, 19 oct 2026 a las 10:00, Sam <s@x.com> escribió:\nviejo"), "Si.");
    }

    #[test]
    fn a_normal_sentence_that_starts_with_on_is_kept() {
        let t = "On Monday we can talk. On Tuesday I am away.\nThanks";
        assert_eq!(strip_quoted(t), t);
    }

    #[test]
    fn cuts_at_original_message_and_underscores() {
        assert_eq!(strip_quoted("Ok.\n-----Original Message-----\nFrom: x"), "Ok.");
        assert_eq!(strip_quoted("Ok.\n________________________________\nFrom: x"), "Ok.");
    }

    #[test]
    fn cuts_at_an_outlook_header_block() {
        let t = "Send details.\n\nFrom: Sam <s@x.com>\nSent: Monday, October 19, 2026 10:00 AM\nTo: Jane\nSubject: Hello\n\nbody";
        assert_eq!(strip_quoted(t), "Send details.");
        // A lone "From:" line is not a header block.
        let t = "Send details.\nFrom: Berlin office\nThanks";
        assert_eq!(strip_quoted(t), t);
    }

    #[test]
    fn handles_windows_line_endings_and_the_size_cap() {
        assert_eq!(strip_quoted("Hi\r\n> q\r\n"), "Hi");
        let long = "é".repeat(10_000);
        let out = strip_quoted(&long);
        assert!(out.len() <= MAX_REPLY_TEXT_BYTES);
        assert!(out.chars().all(|c| c == 'é'));
    }

    #[test]
    fn empty_when_the_whole_text_is_quoted() {
        assert_eq!(strip_quoted("> only a quote"), "");
    }
}
