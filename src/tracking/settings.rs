//! The tracking settings of one campaign (`tracking_settings`), and the checks of what a user may save.
use super::domains::{self, DomainView, Row as DomainRow, Status};
use super::render;
use super::style::ManualStyle;
use crate::error::{ApiError, ApiResult};
use crate::state::AppState;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::FromRow;
use utoipa::ToSchema;
use uuid::Uuid;

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum TrackerType {
    /// A word in the email (usually the name in the signature) becomes an image of the same text.
    TextImage,
    /// The first image that is not in a link is served from the tracking domain.
    SignatureImage,
}

impl TrackerType {
    pub fn as_str(self) -> &'static str {
        match self {
            TrackerType::TextImage => "text_image",
            TrackerType::SignatureImage => "signature_image",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "text_image" => Some(TrackerType::TextImage),
            "signature_image" => Some(TrackerType::SignatureImage),
            _ => None,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    /// The server finds the style (Type 1) or the image (Type 2) in the email.
    #[default]
    Auto,
    /// The user gives the style or the image address.
    Manual,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Auto => "auto",
            Mode::Manual => "manual",
        }
    }
    pub fn parse(s: &str) -> Self {
        if s == "manual" { Mode::Manual } else { Mode::Auto }
    }
}

#[derive(FromRow, Debug, Clone)]
pub struct Row {
    pub campaign_id: Uuid,
    pub enabled: bool,
    pub tracker_type: Option<String>,
    pub text_value: Option<String>,
    pub style_mode: String,
    pub manual_style: Option<Value>,
    pub image_mode: String,
    pub image_source_url: Option<String>,
    pub warning: Option<String>,
    pub warning_at: Option<DateTime<Utc>>,
}

pub const COLS: &str = "campaign_id, enabled, tracker_type, text_value, style_mode, manual_style, image_mode, image_source_url, warning, warning_at";

impl Row {
    pub fn tracker(&self) -> Option<TrackerType> {
        self.tracker_type.as_deref().and_then(TrackerType::parse)
    }
    pub fn style_mode(&self) -> Mode {
        Mode::parse(&self.style_mode)
    }
    pub fn image_mode(&self) -> Mode {
        Mode::parse(&self.image_mode)
    }
    pub fn manual(&self) -> Option<ManualStyle> {
        serde_json::from_value(self.manual_style.clone()?).ok()
    }
    /// A campaign with no row has tracking off.
    pub fn off(campaign_id: Uuid) -> Self {
        Self { campaign_id, enabled: false, tracker_type: None, text_value: None, style_mode: "auto".into(), manual_style: None, image_mode: "auto".into(), image_source_url: None, warning: None, warning_at: None }
    }
}

pub async fn load(pool: &sqlx::PgPool, campaign_id: Uuid) -> Result<Row, sqlx::Error> {
    Ok(sqlx::query_as(&format!("SELECT {COLS} FROM tracking_settings WHERE campaign_id = $1")).bind(campaign_id).fetch_optional(pool).await?.unwrap_or_else(|| Row::off(campaign_id)))
}

/// What the user sends to save. Every field has a default, so `{}` turns tracking off and resets the rest.
#[derive(Deserialize, ToSchema, Default, Clone, Debug)]
pub struct SettingsBody {
    #[serde(default)]
    pub enabled: bool,
    /// `text_image` or `signature_image`. `null` until the user picks one. A tracker can be picked only when
    /// every sending domain of the campaign is `connected`.
    #[serde(default)]
    pub tracker_type: Option<TrackerType>,
    /// Type 1: the text to replace, for example `Nazmul Hasan`. 1 to 200 characters.
    #[serde(default)]
    pub text_value: Option<String>,
    /// Type 1: `auto` (default) reads the style around the text; `manual` uses `manual_style`.
    #[serde(default)]
    pub style_mode: Option<Mode>,
    #[serde(default)]
    pub manual_style: Option<ManualStyle>,
    /// Type 2: `auto` (default) takes the first image that is not in a link; `manual` takes `image_source_url`.
    #[serde(default)]
    pub image_mode: Option<Mode>,
    /// Type 2, manual: the `src` of the image in the email.
    #[serde(default)]
    pub image_source_url: Option<String>,
}

#[derive(Serialize, ToSchema, Debug)]
pub struct SettingsView {
    pub campaign_id: Uuid,
    pub enabled: bool,
    pub tracker_type: Option<TrackerType>,
    pub text_value: Option<String>,
    pub style_mode: Mode,
    pub manual_style: Option<ManualStyle>,
    pub image_mode: Mode,
    pub image_source_url: Option<String>,
    /// Why emails went out without a tracker, if they did (a domain is not connected, the text was not found, ...).
    pub warning: Option<String>,
    pub warning_at: Option<DateTime<Utc>>,
    /// One row for each sending domain of the campaign (from the accounts in its pool).
    pub domains: Vec<DomainView>,
    /// Every sending domain is `connected` (and there is at least one). The tracker cards unlock then.
    pub trackers_unlocked: bool,
}

pub fn unlocked(domains: &[DomainRow]) -> bool {
    !domains.is_empty() && domains.iter().all(|d| d.status == Status::Connected)
}

pub async fn view(st: &AppState, campaign_id: Uuid) -> Result<SettingsView, sqlx::Error> {
    domains::sync(st).await?;
    let row = load(&st.pool, campaign_id).await?;
    let doms = domains::of_campaign(&st.pool, campaign_id).await?;
    Ok(SettingsView {
        campaign_id,
        enabled: row.enabled,
        tracker_type: row.tracker(),
        text_value: row.text_value.clone(),
        style_mode: row.style_mode(),
        manual_style: row.manual(),
        image_mode: row.image_mode(),
        image_source_url: row.image_source_url.clone(),
        warning: row.warning.clone(),
        warning_at: row.warning_at,
        trackers_unlocked: unlocked(&doms),
        domains: doms.iter().map(|d| domains::view(st, d)).collect(),
    })
}

fn bad(code: &str, msg: impl Into<String>) -> ApiError {
    ApiError::unprocessable(code, msg)
}

pub fn is_web_url(u: &str) -> bool {
    let u = u.trim();
    (u.starts_with("http://") || u.starts_with("https://")) && u.len() <= 2000 && !u.contains(char::is_whitespace) && reqwest::Url::parse(u).is_ok()
}

/// The checks that need no database.
pub fn validate(b: &SettingsBody) -> ApiResult<()> {
    if let Some(m) = &b.manual_style {
        m.validate().map_err(|e| bad("invalid_manual_style", e))?;
    }
    if let Some(t) = &b.text_value {
        let n = t.trim().chars().count();
        if n > render::MAX_TEXT_CHARS {
            return Err(bad("invalid_text", format!("The text can have at most {} characters.", render::MAX_TEXT_CHARS)));
        }
    }
    if let Some(u) = b.image_source_url.as_deref().filter(|u| !u.trim().is_empty()) {
        if !is_web_url(u) {
            return Err(bad("invalid_image_url", "The image address must start with http:// or https://."));
        }
    }
    if b.enabled {
        match b.tracker_type {
            Some(TrackerType::TextImage) => {
                if b.text_value.as_deref().is_none_or(|t| t.trim().is_empty()) {
                    return Err(bad("text_required", "Type the text to replace, for example the name in your signature."));
                }
                if b.style_mode == Some(Mode::Manual) && b.manual_style.is_none() {
                    return Err(bad("manual_style_required", "Set the font, size and color for Manual style."));
                }
            }
            Some(TrackerType::SignatureImage) => {
                if b.image_mode == Some(Mode::Manual) && b.image_source_url.as_deref().is_none_or(|u| u.trim().is_empty()) {
                    return Err(bad("image_url_required", "Paste the address of the image for Manual mode."));
                }
            }
            None => {}
        }
    }
    Ok(())
}

/// Save. Picking a tracker needs every sending domain of the campaign to be `connected`.
pub async fn save(st: &AppState, campaign_id: Uuid, b: &SettingsBody) -> ApiResult<SettingsView> {
    validate(b)?;
    if b.enabled && b.tracker_type.is_some() {
        domains::sync(st).await?;
        let doms = domains::of_campaign(&st.pool, campaign_id).await?;
        if doms.is_empty() {
            return Err(bad("no_sending_domains", "Add at least one sending account to the campaign first. Tracking needs the domain it sends from."));
        }
        let waiting: Vec<String> = doms.iter().filter(|d| d.status != Status::Connected).map(|d| d.host()).collect();
        if !waiting.is_empty() {
            return Err(bad("tracking_domains_not_ready", format!("Connect these tracking domains first: {}.", waiting.join(", "))));
        }
    }
    // a Type 2 image that cannot be downloaded now will not work at send time either
    if b.enabled && b.tracker_type == Some(TrackerType::SignatureImage) && b.image_mode == Some(Mode::Manual) {
        let url = b.image_source_url.as_deref().unwrap_or("").trim();
        super::assets::signature_asset(st, url, false).await.map_err(|e| bad("image_download_failed", format!("The image could not be downloaded: {e}")))?;
    }
    let manual = b.manual_style.as_ref().map(|m| serde_json::to_value(m).unwrap_or(Value::Null));
    sqlx::query(
        "INSERT INTO tracking_settings (campaign_id, enabled, tracker_type, text_value, style_mode, manual_style, image_mode, image_source_url, warning, warning_at, updated_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, NULL, NULL, $9)
         ON CONFLICT (campaign_id) DO UPDATE SET enabled = $2, tracker_type = $3, text_value = $4, style_mode = $5, manual_style = $6, image_mode = $7,
                image_source_url = $8, warning = NULL, warning_at = NULL, updated_at = $9",
    )
    .bind(campaign_id)
    .bind(b.enabled)
    .bind(b.tracker_type.map(TrackerType::as_str))
    .bind(b.text_value.as_deref().map(str::trim).filter(|t| !t.is_empty()))
    .bind(b.style_mode.unwrap_or_default().as_str())
    .bind(manual)
    .bind(b.image_mode.unwrap_or_default().as_str())
    .bind(b.image_source_url.as_deref().map(str::trim).filter(|u| !u.is_empty()))
    .bind(st.now())
    .execute(&st.pool)
    .await?;
    Ok(view(st, campaign_id).await?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(enabled: bool, t: &str) -> SettingsBody {
        SettingsBody { enabled, tracker_type: Some(TrackerType::TextImage), text_value: Some(t.into()), ..Default::default() }
    }

    #[test]
    fn type_1_needs_text_and_a_manual_style_when_manual() {
        assert!(validate(&text(true, "Nazmul Hasan")).is_ok());
        assert_eq!(validate(&text(true, "  ")).unwrap_err().code, "text_required");
        assert!(validate(&text(false, "")).is_ok(), "off: nothing is needed");
        let mut b = text(true, "Nazmul");
        b.style_mode = Some(Mode::Manual);
        assert_eq!(validate(&b).unwrap_err().code, "manual_style_required");
        b.manual_style = Some(ManualStyle { font: "Georgia".into(), size: 16.0, color: "#333333".into(), bold: false, italic: false });
        assert!(validate(&b).is_ok());
        b.manual_style.as_mut().unwrap().size = 300.0;
        assert_eq!(validate(&b).unwrap_err().code, "invalid_manual_style");
        assert_eq!(validate(&text(true, &"x".repeat(201))).unwrap_err().code, "invalid_text");
    }

    #[test]
    fn type_2_manual_needs_a_web_address() {
        let mut b = SettingsBody { enabled: true, tracker_type: Some(TrackerType::SignatureImage), ..Default::default() };
        assert!(validate(&b).is_ok(), "auto needs nothing");
        b.image_mode = Some(Mode::Manual);
        assert_eq!(validate(&b).unwrap_err().code, "image_url_required");
        b.image_source_url = Some("ftp://x.com/a.png".into());
        assert_eq!(validate(&b).unwrap_err().code, "invalid_image_url");
        b.image_source_url = Some("https://x.com/a.png".into());
        assert!(validate(&b).is_ok());
    }

    #[test]
    fn the_tracker_cards_unlock_only_when_every_domain_is_connected() {
        fn d(status: Status) -> DomainRow {
            DomainRow {
                id: Uuid::nil(),
                sending_domain: "a.com".into(),
                subdomain: "media".into(),
                cname_target: String::new(),
                status,
                cert_expires_at: None,
                last_checked_at: None,
                error_message: None,
                check_until: Utc::now(),
                cert_attempts: 0,
                next_cert_attempt_at: None,
                created_at: Utc::now(),
                has_cert: false,
            }
        }
        assert!(unlocked(&[d(Status::Connected), d(Status::Connected)]));
        assert!(!unlocked(&[d(Status::Connected), d(Status::Checking)]));
        assert!(!unlocked(&[d(Status::Error)]));
        assert!(!unlocked(&[]), "no sending domain: nothing to unlock");
    }
}
