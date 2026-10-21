//! The unsubscribe link of every email, and the account signature.
//!
//! Templates may hold `{{unsubscribe}}` (a link) and `{{unsubscribe_url}}` (the address only). Render leaves
//! them in the text as they are (see `followups::template::build_vars`). Just before a send, the worker
//! adds the signature of the account and puts the real link of that one email in their place.
//!
//! The link is `<site url>/v1/unsubscribe/<token>`. The site url is `AIKIDO_PUBLIC_URL`, else the address the
//! app was last opened at (`AppState::site_url`). The token is the send job id and an HMAC of it, so a
//! token cannot be made up or changed to another email. A test email has no job: its link opens a test page.
//! Only before anyone has used the app is the link a `mailto:` to the sending account.
use super::senders::OutgoingMessage;
use super::settings::{ListUnsubscribe, SendingSettings};
use crate::crypto::Crypto;
use crate::followups::{LeadStop, LeadStopReason};
use crate::sending::queue::log_event;
use crate::state::AppState;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Response};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD as B64URL};
use chrono::{DateTime, Utc};
use hmac::{Hmac, KeyInit, Mac};
use serde_json::json;
use sha2::Sha256;
use subtle::ConstantTimeEq;
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

pub const TOKEN: &str = "{{unsubscribe}}";
pub const TOKEN_URL: &str = "{{unsubscribe_url}}";

fn mac(crypto: &Crypto, job_id: Uuid) -> [u8; 16] {
    let key = crypto.derive_key("aikido-unsubscribe-v1");
    let mut mac = <Hmac<Sha256> as KeyInit>::new_from_slice(&*key).expect("HMAC takes any key length");
    mac.update(job_id.as_bytes());
    let full = mac.finalize().into_bytes();
    let mut out = [0u8; 16];
    out.copy_from_slice(&full[..16]);
    out
}

/// The token of one send job: 32 hex digits, a dot, and the signature.
pub fn token(crypto: &Crypto, job_id: Uuid) -> String {
    format!("{}.{}", job_id.simple(), B64URL.encode(mac(crypto, job_id)))
}

/// The job id in a token, or `None` when the token was not made by us.
pub fn verify(crypto: &Crypto, token: &str) -> Option<Uuid> {
    let (id, sig) = token.split_once('.')?;
    let id = Uuid::try_parse(id).ok()?;
    let sig = B64URL.decode(sig).ok()?;
    bool::from(sig.ct_eq(&mac(crypto, id))).then_some(id)
}

/// The web address of the link, when the server has a public URL.
pub fn web_link(public_url: Option<&str>, crypto: &Crypto, job_id: Uuid) -> Option<String> {
    public_url.map(|base| format!("{base}/v1/unsubscribe/{}", token(crypto, job_id)))
}

fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&#39;")
}

/// A plain text body as HTML: escaped, line breaks kept.
pub fn text_to_html(text: &str) -> String {
    format!("<div style=\"white-space:pre-wrap\">{}</div>", escape_html(text))
}

pub fn html_to_text(html: &str) -> String {
    html2text::from_read(html.as_bytes(), 100).map(|t| t.trim_end().to_string()).unwrap_or_else(|_| html.to_string())
}

/// Put the link in place of the two tokens. In HTML, `{{unsubscribe}}` inside an attribute
/// (`href="{{unsubscribe}}"`) is the address; elsewhere it is a whole `<a>` link.
pub fn fill_html(html: &str, link: &str) -> String {
    let url = escape_html(link);
    let html = html.replace(TOKEN_URL, &url);
    let mut out = String::with_capacity(html.len());
    let mut rest = html.as_str();
    while let Some(at) = rest.find(TOKEN) {
        let before = &rest[..at];
        out.push_str(before);
        let in_attr = before.trim_end().ends_with(['"', '\'', '=']);
        if in_attr {
            out.push_str(&url);
        } else {
            out.push_str(&format!("<a href=\"{url}\">Unsubscribe</a>"));
        }
        rest = &rest[at + TOKEN.len()..];
    }
    out.push_str(rest);
    out
}

pub fn fill_text(text: &str, link: &str) -> String {
    text.replace(TOKEN_URL, link).replace(TOKEN, link)
}

/// Add the signature, then fill the unsubscribe tokens. `link` is the link of this one email.
/// A signature and a text-only email give an HTML part too, so the signature shows as HTML.
pub fn finish(msg: &mut OutgoingMessage, settings: &SendingSettings, link: &str) {
    let signature = settings.signature_html.trim();
    if !signature.is_empty() {
        let name = msg.from_name.clone().unwrap_or_default();
        let signature = crate::campaigns::emails::fill_sender(signature, &name, &msg.from_email, true);
        let sig_text = html_to_text(&signature);
        if msg.body_html.is_none() {
            msg.body_html = msg.body_text.as_deref().map(text_to_html);
        }
        if let Some(h) = msg.body_html.as_mut() {
            h.push_str("<br><br>");
            h.push_str(&signature);
        }
        if let Some(t) = msg.body_text.as_mut() {
            t.push_str("\n\n");
            t.push_str(&sig_text);
        }
    }
    if let Some(h) = msg.body_html.as_mut() {
        *h = fill_html(h, link);
    }
    if let Some(t) = msg.body_text.as_mut() {
        *t = fill_text(t, link);
    }
}

/// The `List-Unsubscribe` header: the one set in the settings, or else our own link.
pub fn header(settings: &SendingSettings, web: Option<&str>, https: bool) -> Option<ListUnsubscribe> {
    if settings.list_unsubscribe.is_some() {
        return settings.list_unsubscribe.clone();
    }
    // Mail providers only do one-click unsubscribe over https.
    web.filter(|_| https).map(|u| ListUnsubscribe { mailto: None, url: Some(u.to_string()) })
}

/// The link of a test email: a page that unsubscribes no one.
pub fn test_link(site_url: Option<&str>) -> Option<String> {
    site_url.map(|base| format!("{base}/v1/unsubscribe/{TEST_TOKEN}"))
}

const TEST_TOKEN: &str = "test";

/// The `mailto:` link to use when the server has no public URL.
pub fn mailto_link(account_email: &str) -> String {
    format!("mailto:{account_email}?subject=Unsubscribe")
}

// ---------- the page ----------

pub fn public_router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new().routes(routes!(show, confirm))
}

fn page(title: &str, body: &str) -> String {
    format!(
        "<!doctype html><html><head><meta charset=utf-8><meta name=viewport content=\"width=device-width,initial-scale=1\">\
         <title>{t}</title><style>body{{font-family:system-ui,sans-serif;max-width:28rem;margin:4rem auto;padding:0 1rem;color:#222}}\
         button{{font:inherit;padding:.6rem 1.2rem;border-radius:6px;border:0;background:#222;color:#fff;cursor:pointer}}</style></head>\
         <body><h1>{t}</h1>{body}</body></html>",
        t = escape_html(title)
    )
}

fn test_page() -> Response {
    Html(page("Unsubscribe link works", "<p>This is a test email. In a real email, this link unsubscribes the receiver.</p>")).into_response()
}

fn bad_link() -> Response {
    (StatusCode::NOT_FOUND, Html(page("Link not valid", "<p>This unsubscribe link is not valid.</p>"))).into_response()
}

async fn job_email(st: &AppState, token: &str) -> Result<Option<(Uuid, Uuid, String)>, sqlx::Error> {
    let Some(job_id) = verify(&st.crypto, token) else { return Ok(None) };
    sqlx::query_as("SELECT id, account_id, to_email::text FROM send_jobs WHERE id = $1").bind(job_id).fetch_optional(&st.pool).await
}

/// The page behind the link. A script on the page sends the POST at once, so a click is enough.
/// Mail scanners open links but do not run scripts, so they do not unsubscribe anyone.
/// With scripts off, the person clicks the button.
#[utoipa::path(get, path = "/v1/unsubscribe/{token}", tag = "sending",
    params(("token" = String, Path)),
    responses((status = 200, description = "A page with an Unsubscribe button.", content_type = "text/html")),
    description = "No key needed. The signed token names one sent email.")]
async fn show(State(st): State<AppState>, axum::extract::Path(token): axum::extract::Path<String>) -> Response {
    if token == TEST_TOKEN {
        return test_page();
    }
    match job_email(&st, &token).await {
        Ok(Some((_, _, email))) => Html(page(
            "Unsubscribe",
            &format!(
                "<form id=f method=post><p>Unsubscribing <b>{}</b>...</p>\
                 <noscript><p>Click the button to stop all emails to this address.</p></noscript>\
                 <button type=submit>Unsubscribe</button></form><script>document.getElementById('f').submit()</script>",
                escape_html(&email)
            ),
        ))
        .into_response(),
        Ok(None) => bad_link(),
        Err(e) => {
            tracing::error!(error = %e, "unsubscribe page failed");
            (StatusCode::INTERNAL_SERVER_ERROR, Html(page("Error", "<p>Try again later.</p>"))).into_response()
        }
    }
}

/// The button of the page, and the one-click `List-Unsubscribe-Post` of mail apps.
#[utoipa::path(post, path = "/v1/unsubscribe/{token}", tag = "sending",
    params(("token" = String, Path)),
    responses((status = 200, description = "The address is unsubscribed.", content_type = "text/html")),
    description = "No key needed. Adds the address to the suppression list, cancels its queued emails and stops its sequences. A second call does nothing new.")]
async fn confirm(State(st): State<AppState>, axum::extract::Path(token): axum::extract::Path<String>) -> Response {
    if token == TEST_TOKEN {
        return test_page();
    }
    let found = match job_email(&st, &token).await {
        Ok(Some(f)) => f,
        Ok(None) => return bad_link(),
        Err(e) => {
            tracing::error!(error = %e, "unsubscribe failed");
            return (StatusCode::INTERNAL_SERVER_ERROR, Html(page("Error", "<p>Try again later.</p>"))).into_response();
        }
    };
    let (job_id, account_id, email) = found;
    match unsubscribe(&st, job_id, account_id, &email, st.now()).await {
        Ok(()) => Html(page(
            "Successfully unsubscribed",
            &format!("<p><b>{}</b> is unsubscribed. You get no more emails from us.</p>", escape_html(&email)),
        ))
        .into_response(),
        Err(e) => {
            tracing::error!(error = %e, "unsubscribe failed");
            (StatusCode::INTERNAL_SERVER_ERROR, Html(page("Error", "<p>Try again later.</p>"))).into_response()
        }
    }
}

/// Suppress the address, cancel its queued emails and stop its sequences. One transaction.
pub async fn unsubscribe(st: &AppState, job_id: Uuid, account_id: Uuid, email: &str, now: DateTime<Utc>) -> Result<(), sqlx::Error> {
    let email = email.trim().to_lowercase();
    let mut tx = st.pool.begin().await?;
    let created = sqlx::query(
        "INSERT INTO suppressions (value, kind, reason, created_at)
         VALUES ($1::citext, 'email', 'unsubscribed', $2) ON CONFLICT DO NOTHING",
    )
    .bind(&email)
    .bind(now)
    .execute(&mut *tx)
    .await?
    .rows_affected()
        == 1;
    if !created {
        // Already on the list (a second click, or an earlier reply). Make a weak `replied` row strong.
        sqlx::query("UPDATE suppressions SET reason = 'unsubscribed' WHERE value = $1::citext AND kind = 'email' AND reason = 'replied'")
            .bind(&email)
            .execute(&mut *tx)
            .await?;
    }
    let cancelled: Vec<(Uuid, Uuid)> = sqlx::query_as(
        "UPDATE send_jobs SET status = 'cancelled', last_error = 'stopped: unsubscribed', updated_at = $2
         WHERE status = 'queued' AND enrollment_id IS NULL AND to_email = $1::citext
         RETURNING id, account_id",
    )
    .bind(&email)
    .bind(now)
    .fetch_all(&mut *tx)
    .await?;
    for (id, acc) in &cancelled {
        log_event(&mut tx, Some(*id), *acc, "cancelled", json!({ "reason": "unsubscribed", "via": "link" }), now).await?;
    }
    let emails = [email.clone()];
    crate::followups::on_lead_stopped(
        &mut tx,
        &LeadStop { reason: LeadStopReason::Unsubscribe, matched_job_id: Some(job_id), emails: &emails, domains: &[], inbound_message_id: None },
        now,
    )
    .await?;
    if created {
        log_event(&mut tx, Some(job_id), account_id, "unsubscribed", json!({ "email": email, "via": "link" }), now).await?;
    }
    tx.commit().await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn crypto() -> Crypto {
        Crypto::from_key_bytes(&[7u8; 32]).unwrap()
    }

    #[test]
    fn token_round_trip() {
        let c = crypto();
        let id = Uuid::now_v7();
        let t = token(&c, id);
        assert_eq!(verify(&c, &t), Some(id));
        let other = Uuid::now_v7();
        let forged = format!("{}.{}", other.simple(), t.split_once('.').unwrap().1);
        assert_eq!(verify(&c, &forged), None);
        assert_eq!(verify(&c, "nope"), None);
        assert_eq!(verify(&Crypto::from_key_bytes(&[8u8; 32]).unwrap(), &t), None);
    }

    #[test]
    fn test_link_is_not_a_token() {
        assert_eq!(test_link(Some("https://x.com")).as_deref(), Some("https://x.com/v1/unsubscribe/test"));
        assert_eq!(test_link(None), None);
        assert_eq!(verify(&crypto(), TEST_TOKEN), None);
    }

    #[test]
    fn html_link_or_address() {
        let l = "https://x.com/v1/unsubscribe/a.b";
        assert_eq!(fill_html("Bye {{unsubscribe}}", l), format!("Bye <a href=\"{l}\">Unsubscribe</a>"));
        assert_eq!(fill_html("<a href=\"{{unsubscribe}}\">Stop</a>", l), format!("<a href=\"{l}\">Stop</a>"));
        assert_eq!(fill_html("{{unsubscribe_url}}", l), l);
        assert_eq!(fill_text("Stop: {{unsubscribe}}", l), format!("Stop: {l}"));
    }

    fn msg(text: Option<&str>, html: Option<&str>) -> OutgoingMessage {
        OutgoingMessage {
            message_id: "<a@b>".into(),
            from_email: "me@x.com".into(),
            from_name: Some("Me".into()),
            to_email: "you@y.com".into(),
            to_name: None,
            subject: "Hi".into(),
            body_text: text.map(String::from),
            body_html: html.map(String::from),
            headers: Default::default(),
            in_reply_to: None,
            references: None,
            list_unsubscribe: None,
            thread_id: None,
        }
    }

    #[test]
    fn signature_adds_html_part() {
        let s = SendingSettings { signature_html: "<b>{{sender_name}}</b> <a href=\"{{unsubscribe}}\">opt out</a>".into(), ..Default::default() };
        let mut m = msg(Some("Hello <you>"), None);
        finish(&mut m, &s, "https://u");
        let html = m.body_html.unwrap();
        assert!(html.contains("Hello &lt;you&gt;"));
        assert!(html.ends_with("<b>Me</b> <a href=\"https://u\">opt out</a>"));
        let text = m.body_text.unwrap();
        assert!(text.starts_with("Hello <you>\n\n"));
        assert!(text.contains("Me"));
        assert!(!text.contains("{{"));
    }

    #[test]
    fn no_signature_keeps_text_only() {
        let mut m = msg(Some("Hi. {{unsubscribe}}"), None);
        finish(&mut m, &SendingSettings::default(), "https://u");
        assert_eq!(m.body_html, None);
        assert_eq!(m.body_text.as_deref(), Some("Hi. https://u"));
    }

    #[test]
    fn header_uses_own_link_on_https_only() {
        let s = SendingSettings::default();
        assert!(header(&s, Some("https://u"), true).is_some());
        assert!(header(&s, Some("http://u"), false).is_none());
        let custom = SendingSettings { list_unsubscribe: Some(ListUnsubscribe { mailto: Some("a@b".into()), url: None }), ..Default::default() };
        assert_eq!(header(&custom, Some("https://u"), true), custom.list_unsubscribe);
    }
}
