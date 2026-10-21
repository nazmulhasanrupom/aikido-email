//! The "insert tracker" step of the send pipeline. It runs on the final HTML, after the placeholders, the AI text,
//! the footer, the signature and the unsubscribe link are in, and before the MIME is built. That is why a name that
//! comes from `{{sender_name}}` or from the account signature is always found.
//!
//! It never stops a send. If anything is missing (the tracking domain is not connected, the text or the image is not
//! in the email, a download fails), the email goes out without a tracker and a warning is saved on the campaign.
use super::assets;
use super::domains::{self, Status};
use super::html::{self, Doc};
use super::render;
use super::settings::{self, Mode, Row, TrackerType};
use super::style::{TextStyle, read_style};
use crate::accounts::model::EmailAccount;
use crate::sending::queue::Job;
use crate::state::AppState;
use rand::Rng;
use rand::distributions::Alphanumeric;
use uuid::Uuid;

/// A random token: 22 letters and digits (about 131 bits). No lead data in it.
pub fn new_token() -> String {
    rand::thread_rng().sample_iter(&Alphanumeric).take(22).map(char::from).collect()
}

/// Type 1, the part that needs no database: where the text is, and the style to draw it in.
pub fn prepare_text(html: &str, text: &str, manual: Option<&TextStyle>) -> Option<(html::TextMatch, TextStyle)> {
    let doc = Doc::parse(html);
    let m = html::find_last_text(&doc, text)?;
    let style = match manual {
        Some(s) => s.clone(),
        None => read_style(&doc, html::first_node(&m)),
    };
    Some((m, style))
}

/// The new HTML with a tracker in it, or `None` when the email goes out as it is. `html` is the final HTML of a job
/// that has an HTML body.
pub async fn apply(st: &AppState, job: &Job, account: &EmailAccount, html: &str) -> Option<String> {
    let campaign_id = job.campaign_id?;
    let row = match settings::load(&st.pool, campaign_id).await {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(error = %e, %campaign_id, "could not read the tracking settings: sending without a tracker");
            return None;
        }
    };
    if !row.enabled {
        return None;
    }
    let tracker = row.tracker()?;
    match try_apply(st, job, account, html, &row, tracker, campaign_id).await {
        Ok(new_html) => {
            if row.warning.is_some() {
                let _ = sqlx::query("UPDATE tracking_settings SET warning = NULL, warning_at = NULL WHERE campaign_id = $1").bind(campaign_id).execute(&st.pool).await;
            }
            Some(new_html)
        }
        Err(reason) => {
            tracing::warn!(job_id = %job.id, %campaign_id, %reason, "email sent without a tracker");
            let msg = format!("An email went out without a tracker: {reason}");
            let _ = sqlx::query(
                "UPDATE tracking_settings SET warning = $2, warning_at = $3 WHERE campaign_id = $1
                    AND (warning IS DISTINCT FROM $2 OR warning_at IS NULL OR warning_at < $3 - interval '1 hour')",
            )
            .bind(campaign_id)
            .bind(msg)
            .bind(st.now())
            .execute(&st.pool)
            .await;
            None
        }
    }
}

async fn try_apply(st: &AppState, job: &Job, account: &EmailAccount, html: &str, row: &Row, tracker: TrackerType, campaign_id: Uuid) -> Result<String, String> {
    let sending_domain = domains::domain_of_email(&account.email).ok_or_else(|| format!("the sender address {} has no domain", account.email))?;
    let domain = domains::by_sending_domain(&st.pool, &sending_domain).await.map_err(|e| e.to_string())?.ok_or_else(|| format!("{sending_domain} has no tracking domain"))?;
    if domain.status != Status::Connected {
        return Err(format!("the tracking domain {} is not connected", domain.host()));
    }
    let host = domain.host();
    match tracker {
        TrackerType::TextImage => {
            let text: String = row.text_value.as_deref().unwrap_or("").trim().to_string();
            if text.is_empty() {
                return Err("no text is set for the text image".into());
            }
            let manual = match row.style_mode() {
                Mode::Manual => Some(row.manual().ok_or("the manual style is missing")?.to_style()),
                Mode::Auto => None,
            };
            let (m, style) = prepare_text(html, &text, manual.as_ref()).ok_or_else(|| format!("the text \"{text}\" is not in the email"))?;
            let asset = assets::text_asset(st, &text, &style).await?;
            let token = save_token(st, job, campaign_id, &asset.id).await?;
            let tag = render::img_tag(&format!("https://{host}/i/{token}.png"), &text, &assets::as_rendered(&asset));
            Ok(html::replace_match(html, &m, &tag))
        }
        TrackerType::SignatureImage => {
            let found = match row.image_mode() {
                Mode::Manual => html::image_with_src(html, row.image_source_url.as_deref().unwrap_or("")),
                Mode::Auto => html::first_image_outside_links(html),
            }
            .ok_or("no image to track is in the email")?;
            let asset = assets::signature_asset(st, &found.src, false).await.map_err(|e| format!("the image {} could not be stored ({e})", found.src))?;
            let token = save_token(st, job, campaign_id, &asset.id).await?;
            Ok(html::replace_src(html, &found, &format!("https://{host}/p/{token}")))
        }
    }
}

/// One token for each sent email. A retry of the same job keeps its token.
async fn save_token(st: &AppState, job: &Job, campaign_id: Uuid, asset_id: &Uuid) -> Result<String, String> {
    let token: String = sqlx::query_scalar(
        "INSERT INTO tracking_tokens (token, send_job_id, campaign_id, lead_id, asset_id, created_at) VALUES ($1, $2, $3, $4, $5, $6)
         ON CONFLICT (send_job_id) DO UPDATE SET asset_id = EXCLUDED.asset_id RETURNING token",
    )
    .bind(new_token())
    .bind(job.id)
    .bind(campaign_id)
    .bind(job.lead_id)
    .bind(asset_id)
    .bind(st.now())
    .fetch_one(&st.pool)
    .await
    .map_err(|e| e.to_string())?;
    sqlx::query("UPDATE send_jobs SET tracking_token = $2 WHERE id = $1").bind(job.id).bind(&token).execute(&st.pool).await.map_err(|e| e.to_string())?;
    Ok(token)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_are_random_and_22_characters() {
        let a = new_token();
        assert_eq!(a.len(), 22);
        assert!(super::super::server::valid_token(&a));
        assert_ne!(a, new_token());
    }

    #[test]
    fn auto_mode_reads_the_style_around_the_last_match() {
        let html = r#"<p>Hi Nazmul Hasan,</p><div style="font-family: Georgia; font-size: 16px; color: #333333">Best,<br><b>Nazmul Hasan</b></div>"#;
        let (m, style) = prepare_text(html, "Nazmul Hasan", None).unwrap();
        assert_eq!((style.font.as_str(), style.size, style.color.as_str(), style.bold), ("Georgia", 16.0, "#333333", true));
        let out = html::replace_match(html, &m, "<img>");
        assert!(out.starts_with("<p>Hi Nazmul Hasan,</p>"), "the first match stays: {out}");
        assert!(out.ends_with("<b><img></b></div>"), "{out}");
    }

    #[test]
    fn manual_mode_uses_the_saved_style_and_none_without_the_text() {
        let manual = TextStyle { font: "Verdana".into(), ..TextStyle::default() };
        let (_, style) = prepare_text("<p>Nazmul</p>", "Nazmul", Some(&manual)).unwrap();
        assert_eq!(style.font, "Verdana");
        assert!(prepare_text("<p>Someone else</p>", "Nazmul", None).is_none());
    }

    #[test]
    fn a_placeholder_filled_text_is_found_in_the_final_html() {
        // the final HTML has the real name, not {{sender_name}}
        let html = "<p>Thanks,</p><p>Nazmul Hasan</p><hr><div class=\"unsub\">Unsubscribe</div>";
        assert!(prepare_text(html, "Nazmul Hasan", None).is_some());
        assert!(prepare_text("<p>{{sender_name}}</p>", "Nazmul Hasan", None).is_none());
    }
}
