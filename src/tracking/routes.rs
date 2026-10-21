//! The tracking API (scope `tracking`): domains, the settings of a campaign, the preview, and the open numbers.
use super::assets;
use super::domains::{self, DomainView};
use super::html;
use super::render;
use super::settings::{self, Mode, SettingsBody, SettingsView, TrackerType};
use super::stats::{self, CampaignOpens, OpenRow, OpenSummary, RecentLead};
use super::style::{ManualStyle, TextStyle};
use crate::auth::api_keys::Scope;
use crate::auth::middleware::require_scope;
use crate::error::{ApiError, ApiJson, ApiPath, ApiQuery, ApiResult};
use crate::state::AppState;
use axum::extract::State;
use base64::{Engine, engine::general_purpose::STANDARD as B64};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

pub fn router(state: AppState) -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list_domains))
        .routes(routes!(update_domain))
        .routes(routes!(check_domain))
        .routes(routes!(get_settings, put_settings))
        .routes(routes!(preview))
        .routes(routes!(refresh_image))
        .routes(routes!(campaign_opens))
        .routes(routes!(lead_opens))
        .route_layer(axum::middleware::from_fn_with_state((state, Scope::Tracking), require_scope))
}

fn domain_not_found() -> ApiError {
    ApiError::not_found("not_found", "No tracking domain with this id.")
}

async fn campaign_exists(st: &AppState, id: Uuid) -> ApiResult<()> {
    let found: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM campaigns WHERE id = $1)").bind(id).fetch_one(&st.pool).await?;
    if found { Ok(()) } else { Err(ApiError::not_found("campaign_not_found", "No campaign with this id.")) }
}

// ---------- domains ----------

#[derive(Serialize, ToSchema)]
pub struct DomainsReply {
    pub domains: Vec<DomainView>,
}

/// Every sending domain and the state of its tracking domain.
#[utoipa::path(get, path = "/v1/tracking/domains", tag = "tracking", responses((status = 200, body = DomainsReply)),
    security(("api_key" = ["tracking"]), ("session" = [])),
    description = "Scope: `tracking`. One row for each domain of the connected accounts (`nazmul@acme.com` gives `acme.com`). The rows are made by the server. \
        `record` is the CNAME to add in the DNS provider. Status: `not_connected` (no CNAME), `checking` (CNAME found, certificate in progress), `connected`, `error`.")]
async fn list_domains(State(st): State<AppState>) -> ApiResult<ApiJson<DomainsReply>> {
    domains::sync(&st).await?;
    let rows = domains::list(&st).await?;
    Ok(ApiJson(DomainsReply { domains: rows.iter().map(|r| domains::view(&st, r)).collect() }))
}

#[derive(Deserialize, ToSchema)]
pub struct UpdateDomain {
    /// One name: `media`, `img`, `assets`, `files` ... Not `track`, `tracking`, `click`, `open`, `pixel`.
    pub subdomain: String,
}

/// Change the subdomain name. The domain starts over: a new CNAME and a new certificate.
#[utoipa::path(put, path = "/v1/tracking/domains/{id}", tag = "tracking", params(("id" = Uuid, Path)), request_body = UpdateDomain,
    responses((status = 200, body = DomainView), (status = 404, description = "No such domain"), (status = 422, description = "`invalid_subdomain`")),
    security(("api_key" = ["tracking"]), ("session" = [])),
    description = "Scope: `tracking`. Blocked names: `track`, `tracking`, `click`, `open`, `pixel` (and names with `track`, `click` or `pixel` in them), and names that mail uses (`mail`, `www`, `smtp`, ...).")]
async fn update_domain(State(st): State<AppState>, ApiPath(id): ApiPath<Uuid>, ApiJson(body): ApiJson<UpdateDomain>) -> ApiResult<ApiJson<DomainView>> {
    let row = domains::set_subdomain(&st, id, &body.subdomain).await?;
    Ok(ApiJson(domains::view(&st, &row)))
}

/// Check the DNS now.
#[utoipa::path(post, path = "/v1/tracking/domains/{id}/check", tag = "tracking", params(("id" = Uuid, Path)),
    responses((status = 200, body = DomainView), (status = 404, description = "No such domain")),
    security(("api_key" = ["tracking"]), ("session" = [])),
    description = "Scope: `tracking`. Looks at the DNS at once and starts the 72 hour window of the 5 minute check again. When the CNAME is found, the status turns `checking` and \
        the certificate is asked for in the background; read the status again after a short while.")]
async fn check_domain(State(st): State<AppState>, ApiPath(id): ApiPath<Uuid>) -> ApiResult<ApiJson<DomainView>> {
    let row = domains::check_now(&st, id).await.map_err(|e| if e.status == axum::http::StatusCode::NOT_FOUND { domain_not_found() } else { e })?;
    Ok(ApiJson(domains::view(&st, &row)))
}

// ---------- settings ----------

/// The tracking settings of a campaign, with the sending domains of its accounts.
#[utoipa::path(get, path = "/v1/campaigns/{id}/tracking", tag = "tracking", params(("id" = Uuid, Path)),
    responses((status = 200, body = SettingsView), (status = 404, description = "No such campaign")),
    security(("api_key" = ["tracking"]), ("session" = [])),
    description = "Scope: `tracking`. Off by default. `trackers_unlocked` is true when every sending domain of the campaign is `connected`.")]
async fn get_settings(State(st): State<AppState>, ApiPath(id): ApiPath<Uuid>) -> ApiResult<ApiJson<SettingsView>> {
    campaign_exists(&st, id).await?;
    Ok(ApiJson(settings::view(&st, id).await?))
}

/// Save the tracking settings of a campaign.
#[utoipa::path(put, path = "/v1/campaigns/{id}/tracking", tag = "tracking", params(("id" = Uuid, Path)), request_body = SettingsBody,
    responses((status = 200, body = SettingsView), (status = 404, description = "No such campaign"),
        (status = 422, description = "`tracking_domains_not_ready`, `no_sending_domains`, `text_required`, `invalid_manual_style`, `invalid_image_url`, `image_download_failed`")),
    security(("api_key" = ["tracking"]), ("session" = [])),
    description = "Scope: `tracking`. Replaces all settings. Pick one tracker per campaign: `text_image` (Type 1) or `signature_image` (Type 2). A tracker can be picked only \
        when every sending domain of the campaign is `connected`. All emails of the campaign get the tracker (first emails, follow-ups and manual emails), if they have an HTML part.")]
async fn put_settings(State(st): State<AppState>, ApiPath(id): ApiPath<Uuid>, ApiJson(body): ApiJson<SettingsBody>) -> ApiResult<ApiJson<SettingsView>> {
    campaign_exists(&st, id).await?;
    Ok(ApiJson(settings::save(&st, id, &body).await?))
}

// ---------- preview ----------

#[derive(Deserialize, ToSchema, Default)]
pub struct PreviewBody {
    /// What to preview. Every field that is left out is read from the saved settings.
    pub tracker_type: Option<TrackerType>,
    pub text_value: Option<String>,
    pub style_mode: Option<Mode>,
    pub manual_style: Option<ManualStyle>,
    pub image_mode: Option<Mode>,
    pub image_source_url: Option<String>,
}

#[derive(Serialize, ToSchema, Default)]
pub struct StyleView {
    pub font: String,
    pub size: f32,
    pub color: String,
    pub bold: bool,
    pub italic: bool,
    pub letter_spacing: f32,
    pub line_height: Option<f32>,
}

impl From<&TextStyle> for StyleView {
    fn from(s: &TextStyle) -> Self {
        Self { font: s.font.clone(), size: s.size, color: s.color.clone(), bold: s.bold, italic: s.italic, letter_spacing: s.letter_spacing, line_height: s.line_height }
    }
}

#[derive(Serialize, ToSchema, Default)]
pub struct PreviewReply {
    pub tracker_type: Option<TrackerType>,
    /// The image as a `data:` URL, to show in the page. `null` when there is nothing to show.
    pub image: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    /// Type 1: the style the image was drawn in.
    pub style: Option<StyleView>,
    /// Type 1: `manual`, `email` (read from the first email of the sequence) or `default` (Arial, 14px, #222222).
    pub style_source: Option<String>,
    /// Type 1: the text is in the first email of the sequence. It can be `false` when the text comes from a placeholder such as `{{sender_name}}`; it is found when the email is sent.
    pub text_found: Option<bool>,
    /// Type 2: the address of the image that will be tracked.
    pub source_url: Option<String>,
    /// Type 2: an image was found in the first email of the sequence.
    pub found: Option<bool>,
    pub message: Option<String>,
}

async fn first_step_html(st: &AppState, campaign_id: Uuid) -> Result<Option<String>, sqlx::Error> {
    let html: Option<Option<String>> = sqlx::query_scalar(
        "SELECT ss.body_html FROM campaigns c JOIN sequence_steps ss ON ss.sequence_id = c.sequence_id AND ss.step_number = 1 WHERE c.id = $1",
    )
    .bind(campaign_id)
    .fetch_optional(&st.pool)
    .await?;
    Ok(html.flatten().filter(|h| !h.trim().is_empty()))
}

fn data_url(content_type: &str, bytes: &[u8]) -> String {
    format!("data:{content_type};base64,{}", B64.encode(bytes))
}

/// The settings with the body on top.
struct Merged {
    tracker: Option<TrackerType>,
    text: Option<String>,
    style_mode: Mode,
    manual: Option<ManualStyle>,
    image_mode: Mode,
    image_url: Option<String>,
}

async fn merged(st: &AppState, id: Uuid, b: &PreviewBody) -> Result<Merged, sqlx::Error> {
    let row = settings::load(&st.pool, id).await?;
    Ok(Merged {
        tracker: b.tracker_type.or(row.tracker()),
        text: b.text_value.clone().or_else(|| row.text_value.clone()),
        style_mode: b.style_mode.unwrap_or_else(|| row.style_mode()),
        manual: b.manual_style.clone().or_else(|| row.manual()),
        image_mode: b.image_mode.unwrap_or_else(|| row.image_mode()),
        image_url: b.image_source_url.clone().or_else(|| row.image_source_url.clone()),
    })
}

async fn text_preview(st: &AppState, id: Uuid, m: &Merged) -> ApiResult<PreviewReply> {
    let text: String = m.text.as_deref().unwrap_or("").split_whitespace().collect::<Vec<_>>().join(" ");
    if text.is_empty() {
        return Err(ApiError::unprocessable("text_required", "Type the text to replace, for example the name in your signature."));
    }
    let email = first_step_html(st, id).await?;
    let found_in_email = email.as_deref().and_then(|h| super::send::prepare_text(h, &text, None));
    let (style, source) = match (m.style_mode, &m.manual) {
        (Mode::Manual, Some(ms)) => {
            ms.validate().map_err(|e| ApiError::unprocessable("invalid_manual_style", e))?;
            (ms.to_style(), "manual")
        }
        (Mode::Manual, None) => return Err(ApiError::unprocessable("manual_style_required", "Set the font, size and color for Manual style.")),
        (Mode::Auto, _) => match &found_in_email {
            Some((_, s)) => (s.clone(), "email"),
            None => (TextStyle::default(), "default"),
        },
    };
    let r = render::render_text(&text, &style).map_err(|e| ApiError::unprocessable("invalid_text", e))?;
    let message = match (&found_in_email, m.style_mode) {
        (None, Mode::Auto) => Some("The text is not in the first email yet, so the safe default style is shown (Arial, 14px, #222222). The real style is read when each email is sent.".to_string()),
        _ => None,
    };
    Ok(PreviewReply {
        tracker_type: Some(TrackerType::TextImage),
        image: Some(data_url("image/png", &r.png)),
        width: Some(r.width),
        height: Some(r.height),
        style: Some(StyleView::from(&style)),
        style_source: Some(source.into()),
        text_found: Some(found_in_email.is_some()),
        message,
        ..Default::default()
    })
}

async fn signature_preview(st: &AppState, id: Uuid, m: &Merged, refresh: bool) -> ApiResult<PreviewReply> {
    let mut reply = PreviewReply { tracker_type: Some(TrackerType::SignatureImage), ..Default::default() };
    let Some(email) = first_step_html(st, id).await? else {
        reply.found = Some(false);
        reply.message = Some("The first email has no HTML part, so there is no image to track.".into());
        return Ok(reply);
    };
    let found = match m.image_mode {
        Mode::Manual => {
            let url = m.image_url.as_deref().unwrap_or("").trim();
            if url.is_empty() {
                return Err(ApiError::unprocessable("image_url_required", "Paste the address of the image for Manual mode."));
            }
            html::image_with_src(&email, url)
        }
        Mode::Auto => html::first_image_outside_links(&email),
    };
    let Some(found) = found else {
        reply.found = Some(false);
        reply.message = Some(match m.image_mode {
            Mode::Manual => "No image with this address is in the first email.".into(),
            Mode::Auto => "The first email has no image outside a link.".into(),
        });
        return Ok(reply);
    };
    reply.found = Some(true);
    reply.source_url = Some(found.src.clone());
    match assets::signature_asset(st, &found.src, refresh).await {
        Ok(asset) => {
            if let Some((bytes, ct)) = assets::file(&st.pool, asset.id).await? {
                reply.image = Some(data_url(&ct, &bytes));
            }
            reply.width = asset.width.map(|w| w.max(0) as u32);
            reply.height = asset.height.map(|h| h.max(0) as u32);
        }
        Err(e) => reply.message = Some(format!("The image could not be downloaded: {e}")),
    }
    Ok(reply)
}

/// Make a preview image.
#[utoipa::path(post, path = "/v1/campaigns/{id}/tracking/preview", tag = "tracking", params(("id" = Uuid, Path)), request_body = PreviewBody,
    responses((status = 200, body = PreviewReply), (status = 404, description = "No such campaign"), (status = 422, description = "`text_required`, `invalid_manual_style`, ...")),
    security(("api_key" = ["tracking"]), ("session" = [])),
    description = "Scope: `tracking`. Type 1: draws the text in the style that would be used. In Auto mode the style is read from the first email of the sequence. \
        Type 2: finds the image in the first email, stores a copy, and returns it. Nothing is saved except the stored image copy.")]
async fn preview(State(st): State<AppState>, ApiPath(id): ApiPath<Uuid>, body: axum::body::Bytes) -> ApiResult<ApiJson<PreviewReply>> {
    campaign_exists(&st, id).await?;
    let b: PreviewBody = crate::error::parse_optional_json(&body)?;
    let m = merged(&st, id, &b).await?;
    let reply = match m.tracker {
        Some(TrackerType::SignatureImage) => signature_preview(&st, id, &m, false).await?,
        Some(TrackerType::TextImage) => text_preview(&st, id, &m).await?,
        None => return Err(ApiError::unprocessable("tracker_required", "Pick a tracker type first: text_image or signature_image.")),
    };
    Ok(ApiJson(reply))
}

/// Download the Type 2 image again.
#[utoipa::path(post, path = "/v1/campaigns/{id}/tracking/refresh-image", tag = "tracking", params(("id" = Uuid, Path)),
    responses((status = 200, body = PreviewReply), (status = 404, description = "No such campaign"), (status = 422, description = "`tracker_required`: the campaign does not use Type 2")),
    security(("api_key" = ["tracking"]), ("session" = [])),
    description = "Scope: `tracking`. The server keeps one copy of the image and sends it back from the tracking domain, so a change at the source is not seen by itself. \
        This downloads it again. The copy keeps its id, so emails that are already sent show the new image too.")]
async fn refresh_image(State(st): State<AppState>, ApiPath(id): ApiPath<Uuid>) -> ApiResult<ApiJson<PreviewReply>> {
    campaign_exists(&st, id).await?;
    let m = merged(&st, id, &PreviewBody::default()).await?;
    if m.tracker != Some(TrackerType::SignatureImage) {
        return Err(ApiError::unprocessable("tracker_required", "This campaign does not use the Signature Image tracker."));
    }
    Ok(ApiJson(signature_preview(&st, id, &m, true).await?))
}

// ---------- numbers ----------

#[derive(Deserialize)]
pub struct OpensQuery {
    /// How many recent leads to list (1 to 100, default 20).
    pub limit: Option<i64>,
}

#[derive(Serialize, ToSchema)]
pub struct CampaignOpensReply {
    #[serde(flatten)]
    pub summary: CampaignOpens,
    /// The leads that opened last, newest first.
    pub recent: Vec<RecentLead>,
}

/// Open numbers of a campaign.
#[utoipa::path(get, path = "/v1/campaigns/{id}/opens", tag = "tracking", params(("id" = Uuid, Path), ("limit" = Option<i64>, Query, description = "Recent leads to list, 1 to 100 (default 20)")),
    responses((status = 200, body = CampaignOpensReply), (status = 404, description = "No such campaign")),
    security(("api_key" = ["tracking"]), ("session" = [])),
    description = "Scope: `tracking`. `opened`: leads with a real open. `maybe_opened`: leads whose opens all came from Apple Mail Privacy Protection, the Google image proxy or a scanner. \
        `repeat_opens`: real opens after the first. `open_rate` is `opened` divided by `tracked_leads`.")]
async fn campaign_opens(State(st): State<AppState>, ApiPath(id): ApiPath<Uuid>, ApiQuery(q): ApiQuery<OpensQuery>) -> ApiResult<ApiJson<CampaignOpensReply>> {
    campaign_exists(&st, id).await?;
    let summary = stats::campaign_summary(&st.pool, id).await?;
    let recent = stats::recent_leads(&st.pool, id, q.limit.unwrap_or(20).clamp(1, 100)).await?;
    Ok(ApiJson(CampaignOpensReply { summary, recent }))
}

#[derive(Serialize, ToSchema)]
pub struct LeadOpensReply {
    pub summary: OpenSummary,
    /// The last 200 opens, newest first.
    pub opens: Vec<OpenRow>,
}

/// The opens of one lead.
#[utoipa::path(get, path = "/v1/leads/{id}/opens", tag = "tracking", params(("id" = Uuid, Path)),
    responses((status = 200, body = LeadOpensReply), (status = 404, description = "No such lead")),
    security(("api_key" = ["tracking"]), ("session" = [])),
    description = "Scope: `tracking`. Each open has the time, the step, and whether it came from a proxy or a scanner (`is_proxy`, `proxy_kind`: `apple`, `google`, `scanner`).")]
async fn lead_opens(State(st): State<AppState>, ApiPath(id): ApiPath<Uuid>) -> ApiResult<ApiJson<LeadOpensReply>> {
    let mut conn = st.pool.acquire().await?;
    let exists: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM leads WHERE id = $1)").bind(id).fetch_one(&mut *conn).await?;
    if !exists {
        return Err(ApiError::not_found("lead_not_found", "No lead with this id."));
    }
    let summary = stats::lead_summary(&mut conn, id).await?;
    drop(conn);
    let opens = stats::lead_opens(&st.pool, id, 200).await?;
    Ok(ApiJson(LeadOpensReply { summary, opens }))
}

