//! Classifier tests. No DB, no network: saved `.eml` files only.
use aikido_email::replies::classify::classify;
use aikido_email::replies::model::{Classification, Label};
use aikido_email::replies::parse::{ParsedEmail, parse};
use aikido_email::replies::quote::strip_quoted;
use aikido_email::replies::settings::ReplySettings;

fn load(name: &str) -> Vec<u8> {
    std::fs::read(format!("{}/tests/fixtures/inbound/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap_or_else(|e| panic!("{name}: {e}"))
}

fn parsed(name: &str) -> ParsedEmail {
    parse(&load(name)).unwrap_or_else(|e| panic!("{name}: {e}"))
}

fn run(name: &str) -> Classification {
    classify(&parsed(name), &ReplySettings::default())
}

#[test]
fn every_fixture_gets_its_label() {
    let expected = [
        ("gmail_bounce_user_unknown.eml", Label::BounceHard),
        ("gmail_bounce_domain_not_found.eml", Label::BounceHard),
        ("outlook_bounce_5_1_10.eml", Label::BounceHard),
        ("outlook_bounce_5_7_1.eml", Label::BounceBlock),
        ("postfix_bounce.eml", Label::BounceHard),
        ("exim_bounce_no_report.eml", Label::BounceHard),
        ("delayed_notice.eml", Label::Delayed),
        ("mailbox_full_5_2_2.eml", Label::BounceSoft),
        ("soft_bounce_4_4_7.eml", Label::BounceSoft),
        ("dsn_without_status.eml", Label::BounceSoft),
        ("gmail_vacation.eml", Label::AutoReply),
        ("outlook_automatic_reply.eml", Label::AutoReply),
        ("german_abwesenheit.eml", Label::AutoReply),
        ("precedence_bulk.eml", Label::AutoReply),
        ("auto_submitted_no.eml", Label::Reply),
        ("human_plain.eml", Label::Reply),
        ("human_html_only.eml", Label::Reply),
        ("human_gmail_quote.eml", Label::Reply),
        ("gmail_quote_wrapped.eml", Label::Reply),
        ("human_outlook_quote.eml", Label::Reply),
        ("reply_all_colleague.eml", Label::Reply),
        ("reply_new_thread.eml", Label::Reply),
        ("unsubscribe_short.eml", Label::Unsubscribe),
        ("unsubscribe_keyword_in_quote_only.eml", Label::Reply),
        ("long_reply_with_remove.eml", Label::Reply),
        ("spam_complaint.eml", Label::Complaint),
    ];
    for (file, label) in expected {
        let c = run(file);
        assert_eq!(c.label, label, "{file}: {c:?}");
    }
}

#[test]
fn status_codes_decide_the_bounce_label() {
    for (file, status, label) in [
        ("mailbox_full_5_2_2.eml", "5.2.2", Label::BounceSoft),
        ("outlook_bounce_5_7_1.eml", "5.7.1", Label::BounceBlock),
        ("gmail_bounce_user_unknown.eml", "5.1.1", Label::BounceHard),
        ("soft_bounce_4_4_7.eml", "4.4.7", Label::BounceSoft),
        ("outlook_bounce_5_1_10.eml", "5.1.10", Label::BounceHard),
    ] {
        let c = run(file);
        assert_eq!(c.label, label, "{file}");
        assert_eq!(c.bounce.unwrap().status.as_deref(), Some(status), "{file}");
    }
}

#[test]
fn a_bounce_with_no_status_code_is_soft_never_hard() {
    let c = run("dsn_without_status.eml");
    assert_eq!(c.label, Label::BounceSoft);
    assert_eq!(c.reason, "unparsed");
    assert_eq!(c.bounce.unwrap().status, None);
}

#[test]
fn old_servers_without_a_report_use_the_code_in_the_body() {
    let c = run("exim_bounce_no_report.eml");
    assert_eq!(c.label, Label::BounceHard);
    let b = c.bounce.unwrap();
    assert_eq!(b.status.as_deref(), Some("5.1.1"));
    // No Final-Recipient: the matcher picks the recipient from the body addresses.
    assert_eq!(b.recipient, None);
    let (_, candidates) = aikido_email::replies::dsn::extract_bounce(&parsed("exim_bounce_no_report.eml"));
    assert_eq!(candidates.first().map(String::as_str), Some("jane@prospect.example"));
}

#[test]
fn the_bounce_report_gives_recipient_diagnostic_and_original_id() {
    let b = run("gmail_bounce_user_unknown.eml").bounce.unwrap();
    assert_eq!(b.recipient.as_deref(), Some("nobody@prospect.example"));
    assert_eq!(b.original_message_id.as_deref(), Some("<sent-0001@acme-outreach.example>"));
    assert!(b.diagnostic.unwrap().contains("does not exist"));
    // Postfix puts only the headers of the original (text/rfc822-headers).
    let b = run("postfix_bounce.eml").bounce.unwrap();
    assert_eq!(b.recipient.as_deref(), Some("ghost@prospect.example"));
    assert_eq!(b.original_message_id.as_deref(), Some("<sent-0001@acme-outreach.example>"));
}

#[test]
fn delayed_notice_is_logged_only() {
    let c = run("delayed_notice.eml");
    assert_eq!(c.label, Label::Delayed);
    assert_eq!(c.bounce.unwrap().action.as_deref(), Some("delayed"));
}

#[test]
fn complaint_names_the_recipient() {
    let c = run("spam_complaint.eml");
    let b = c.bounce.unwrap();
    assert_eq!(b.recipient.as_deref(), Some("jane@prospect.example"));
    assert_eq!(b.original_message_id.as_deref(), Some("<sent-0001@acme-outreach.example>"));
}

#[test]
fn auto_submitted_no_is_not_an_auto_reply() {
    assert_eq!(run("auto_submitted_no.eml").label, Label::Reply);
    // Any other value is.
    let mut p = parsed("auto_submitted_no.eml");
    p.headers.insert("auto-submitted".into(), "auto-generated".into());
    assert_eq!(classify(&p, &ReplySettings::default()).label, Label::AutoReply);
}

#[test]
fn an_unsubscribe_keyword_only_in_the_quote_gives_reply() {
    let p = parsed("unsubscribe_keyword_in_quote_only.eml");
    assert!(p.text.contains("stop emailing"), "the keyword is in the raw text");
    assert!(!strip_quoted(&p.text).contains("stop emailing"));
    assert_eq!(classify(&p, &ReplySettings::default()).label, Label::Reply);
}

#[test]
fn a_long_reply_that_says_remove_is_a_reply() {
    let p = parsed("long_reply_with_remove.eml");
    assert!(p.text.split_whitespace().count() > 30);
    assert_eq!(classify(&p, &ReplySettings::default()).label, Label::Reply);
    // The same text with a bigger word limit and the keyword "remove" is an unsubscribe: the limit is what protects it.
    let s = ReplySettings { unsubscribe_keywords: vec!["remove".into()], unsubscribe_max_words: 200, ..Default::default() };
    assert_eq!(classify(&p, &s).label, Label::Unsubscribe);
    let s = ReplySettings { unsubscribe_keywords: vec!["remove".into()], ..Default::default() };
    assert_eq!(classify(&p, &s).label, Label::Reply);
}

#[test]
fn a_gmail_wrapped_on_wrote_line_is_cut_correctly() {
    let p = parsed("gmail_quote_wrapped.eml");
    assert_eq!(strip_quoted(&p.text), "Please remove the other person from this thread. Thanks.");
    let p = parsed("human_gmail_quote.eml");
    assert_eq!(strip_quoted(&p.text), "Sounds good, let's talk Thursday.");
    let p = parsed("human_outlook_quote.eml");
    assert_eq!(strip_quoted(&p.text), "Yes, send me the details please.");
}

#[test]
fn html_only_mail_is_converted_to_text() {
    let p = parsed("human_html_only.eml");
    assert!(p.text.contains("Yes, I am interested"), "{:?}", p.text);
    assert!(!p.text.contains("<div>"));
}

#[test]
fn lists_come_from_the_settings_not_from_code() {
    let p = parsed("unsubscribe_short.eml");
    // Take the keyword away and it is a plain reply.
    let s = ReplySettings { unsubscribe_keywords: vec!["nothing".into()], ..Default::default() };
    assert_eq!(classify(&p, &s).label, Label::Reply);
    // A new subject pattern makes a human reply an auto-reply.
    let p = parsed("human_plain.eml");
    let s = ReplySettings { auto_reply_subject_patterns: vec!["re: quick question".into()], ..Default::default() };
    assert_eq!(classify(&p, &s).label, Label::AutoReply);
    // A new bounce subject pattern.
    let s = ReplySettings { bounce_subject_patterns: vec!["quick question".into()], ..Default::default() };
    assert_eq!(classify(&p, &s).label, Label::BounceSoft);
}

#[test]
fn headers_and_ids_are_read() {
    let p = parsed("human_plain.eml");
    assert_eq!(p.in_reply_to.as_deref(), Some("<sent-0001@acme-outreach.example>"));
    assert_eq!(p.references, ["<sent-0001@acme-outreach.example>"]);
    assert_eq!(p.from_email.as_deref(), Some("jane@prospect.example"));
    let p = parsed("reply_new_thread.eml");
    assert_eq!(p.in_reply_to, None);
    let p = parsed("reply_all_colleague.eml");
    assert_eq!(p.from_email.as_deref(), Some("tom@prospect.example"));
    assert_eq!(p.cc, ["jane@prospect.example"]);
}

#[test]
fn garbage_does_not_panic() {
    for raw in [&b""[..], b"\x00\x01\x02", b"Subject: x\n", b"From: \n\n\n", &[0xff; 64][..]] {
        let _ = parse(raw).map(|p| classify(&p, &ReplySettings::default()));
    }
}
