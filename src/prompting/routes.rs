//! `/v1/personalization/*` (scope `personalization`) and `/v1/fields` (scope `leads`).
//! The PERSONALIZATION page: collect sources (step A, code only), let the AI draft a sender profile (step B), save it (step C).
use super::fields::{self, FieldInfo};
use super::sender::{self, SenderFields, SenderProfile};
use super::sender_builder::{self, Draft};
use super::sources::{self, Chunk, DetailRow, PersonalizationSource};
use crate::auth::api_keys::Scope;
use crate::auth::middleware::require_scope;
use crate::error::{ApiError, ApiJson, ApiPath, ApiResult};
use crate::state::AppState;
use axum::Extension;
use axum::extract::{Multipart, State};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use utoipa::ToSchema;
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

pub fn router(state: AppState) -> OpenApiRouter<AppState> {
    let personalization = OpenApiRouter::new()
        .routes(routes!(add_source, list_sources))
        .routes(routes!(add_cv))
        .routes(routes!(delete_source))
        .routes(routes!(delete_all))
        .routes(routes!(generate_profile))
        .routes(routes!(get_profile, save_profile))
        .routes(routes!(list_versions))
        .route_layer(axum::middleware::from_fn_with_state((state.clone(), Scope::Personalization), require_scope));
    let registry = OpenApiRouter::new().routes(routes!(get_fields)).route_layer(axum::middleware::from_fn_with_state((state.clone(), Scope::Leads), require_scope));
    let defaults = OpenApiRouter::new().routes(routes!(get_defaults)).route_layer(axum::middleware::from_fn_with_state((state, Scope::Campaigns), require_scope));
    personalization.merge(registry).merge(defaults)
}

#[derive(Serialize, ToSchema)]
pub struct PromptDefaults {
    /// The shipped instructions of the Optimised mode.
    pub system: String,
    /// The shipped user prompt of the Optimised mode.
    pub user: String,
    /// The `{{names}}` the prompts can use: the sender fields first, then the lead fields.
    pub variables: Vec<String>,
}

/// The shipped prompts of the Optimised mode.
#[utoipa::path(get, path = "/v1/prompts/defaults", tag = "campaigns",
    responses((status = 200, body = PromptDefaults)),
    security(("api_key" = ["campaigns"])),
    description = "Scope: `campaigns`. A campaign with no prompts of its own uses these. The prompts are plain text with `{{variables}}`; a variable with no value is left out.")]
async fn get_defaults() -> ApiJson<PromptDefaults> {
    use super::optimized::{DEFAULT_SYSTEM, DEFAULT_USER, LEAD_VARS, SENDER_VARS};
    ApiJson(PromptDefaults {
        system: DEFAULT_SYSTEM.to_string(),
        user: DEFAULT_USER.to_string(),
        variables: SENDER_VARS.iter().chain(LEAD_VARS.iter()).map(|s| s.to_string()).collect(),
    })
}

// ---------- the field registry ----------

/// The field registry: every key the default mail prompt uses.
#[utoipa::path(get, path = "/v1/fields", tag = "leads",
    responses((status = 200, body = Vec<FieldInfo>)),
    security(("api_key" = ["leads"])),
    description = "Scope: `leads`. Sender, lead and system fields. A lead field with `optimized: true` is used by the default prompt; `column` is the lead column it comes from \
        (`company` is `company_name`, `title` is `job_title`, `trigger` is `trigger_event`). `required` fields hold the mail when they are empty.")]
async fn get_fields() -> ApiJson<Vec<FieldInfo>> {
    ApiJson(fields::all().to_vec())
}

// ---------- sources ----------

#[derive(Deserialize, ToSchema)]
pub struct AddSource {
    /// `url`, `text`, `detail` or `guide`.
    pub kind: String,
    /// kind `url`: a portfolio address. Up to 3 sites. The page and up to 4 useful pages of the same site are read.
    pub url: Option<String>,
    /// kind `text`: up to 10000 characters.
    pub text: Option<String>,
    /// kind `detail`: up to 20 key and value rows.
    pub details: Option<Vec<DetailRow>>,
    /// kind `guide`: answers to the four guide questions.
    pub guide: Option<Guide>,
}

#[derive(Deserialize, ToSchema, Default)]
pub struct Guide {
    pub who_you_help: Option<String>,
    pub who_you_do_not_help: Option<String>,
    pub result_you_give: Option<String>,
    pub real_proof: Option<String>,
}

fn invalid(msg: impl Into<String>) -> ApiError {
    ApiError::unprocessable("invalid_source", msg)
}

/// Add input. It is cleaned and saved as tagged text. Nothing goes to the AI yet.
#[utoipa::path(post, path = "/v1/personalization/sources", tag = "personalization",
    request_body = AddSource,
    responses((status = 200, body = Vec<PersonalizationSource>), (status = 422, description = "invalid_source: the address, the text or a limit is not valid")),
    security(("api_key" = ["personalization"])),
    description = "Scope: `personalization`. Step A runs in code: a page is fetched safely (public addresses only, ports 80 and 443, 10 s, 2 MB), turned to text, cleaned and tagged. \
        A source with the same tag replaces the old one. Returns the sources that were saved.")]
async fn add_source(State(st): State<AppState>, ApiJson(b): ApiJson<AddSource>) -> ApiResult<ApiJson<Vec<PersonalizationSource>>> {
    let now = st.now();
    let chunks: Vec<Chunk> = match b.kind.as_str() {
        "url" => {
            let url = b.url.as_deref().map(str::trim).filter(|u| !u.is_empty()).ok_or_else(|| invalid("Give an address."))?;
            let host = reqwest::Url::parse(url).ok().and_then(|u| u.host_str().map(str::to_string)).ok_or_else(|| invalid("This is not a valid address."))?;
            let mut conn = st.pool.acquire().await?;
            let hosts = sources::url_hosts(&mut conn).await?;
            if !hosts.contains(&host) && hosts.len() >= sources::MAX_URLS {
                return Err(invalid(format!("Use at most {} portfolio addresses. Delete one first.", sources::MAX_URLS)));
            }
            drop(conn);
            sources::fetch_portfolio(&st.fetcher, url).await.map_err(invalid)?
        }
        "text" => vec![sources::text_chunk(b.text.as_deref().unwrap_or_default()).map_err(invalid)?],
        "detail" => {
            let rows = b.details.unwrap_or_default();
            let c = sources::detail_chunks(&rows).map_err(invalid)?;
            if c.is_empty() {
                return Err(invalid("Give at least one key and value."));
            }
            c
        }
        "guide" => {
            let g = b.guide.unwrap_or_default();
            let t = |v: &Option<String>| v.as_deref().map(|s| s.chars().take(2000).collect::<String>()).unwrap_or_default();
            vec![sources::guide_chunk(&t(&g.who_you_help), &t(&g.who_you_do_not_help), &t(&g.result_you_give), &t(&g.real_proof)).ok_or_else(|| invalid("Answer at least one question."))?]
        }
        _ => return Err(invalid("kind must be url, text, detail or guide. A CV goes to /v1/personalization/sources/cv.")),
    };
    let mut tx = st.pool.begin().await?;
    let mut saved = Vec::new();
    for c in &chunks {
        saved.push(sources::put(&mut tx, c, now).await?);
    }
    tx.commit().await?;
    Ok(ApiJson(saved))
}

/// Upload a CV. Only its text is kept, never the file.
#[utoipa::path(post, path = "/v1/personalization/sources/cv", tag = "personalization",
    request_body(content = Object, content_type = "multipart/form-data", description = "A `file` part: PDF, DOCX or TXT, up to 5 MB."),
    responses((status = 200, body = PersonalizationSource), (status = 422, description = "invalid_source: wrong type, too large, or \"No text found in this file.\"")),
    security(("api_key" = ["personalization"])),
    description = "Scope: `personalization`. The text is extracted, cleaned and saved with the tag `cv`. The file itself is not stored. A new CV replaces the old one.")]
async fn add_cv(State(st): State<AppState>, mut mp: Multipart) -> ApiResult<ApiJson<PersonalizationSource>> {
    let mut found: Option<(String, Vec<u8>)> = None;
    while let Some(field) = mp.next_field().await.map_err(|_| invalid("The upload could not be read."))? {
        if field.name() == Some("file") {
            let name = field.file_name().unwrap_or("cv").to_string();
            let bytes = field.bytes().await.map_err(|_| invalid("The upload could not be read."))?;
            found = Some((name, bytes.to_vec()));
            break;
        }
    }
    let (name, bytes) = found.ok_or_else(|| invalid("Send the file in a part named 'file'."))?;
    let chunk = sources::cv_chunk(&name, &bytes).map_err(invalid)?;
    let mut tx = st.pool.begin().await?;
    // one CV at a time
    sqlx::query("DELETE FROM personalization_sources WHERE kind = 'cv'").execute(&mut *tx).await?;
    let saved = sources::put(&mut tx, &chunk, st.now()).await?;
    tx.commit().await?;
    Ok(ApiJson(saved))
}

/// The cleaned sources. This is exactly the text the AI would read.
#[utoipa::path(get, path = "/v1/personalization/sources", tag = "personalization",
    responses((status = 200, body = Vec<PersonalizationSource>)),
    security(("api_key" = ["personalization"])), description = "Scope: `personalization`.")]
async fn list_sources(State(st): State<AppState>) -> ApiResult<ApiJson<Vec<PersonalizationSource>>> {
    let mut conn = st.pool.acquire().await?;
    Ok(ApiJson(sources::list(&mut conn).await?))
}

/// Delete one source and its text.
#[utoipa::path(delete, path = "/v1/personalization/sources/{id}", tag = "personalization",
    params(("id" = Uuid, Path)), responses((status = 204, description = "Deleted"), (status = 404, description = "No such source")),
    security(("api_key" = ["personalization"])), description = "Scope: `personalization`.")]
async fn delete_source(State(st): State<AppState>, ApiPath(id): ApiPath<Uuid>) -> ApiResult<axum::http::StatusCode> {
    let mut conn = st.pool.acquire().await?;
    if sources::delete(&mut conn, id).await? { Ok(axum::http::StatusCode::NO_CONTENT) } else { Err(ApiError::not_found("source_not_found", "No source with this id.")) }
}

/// Delete every source and every profile version.
#[utoipa::path(delete, path = "/v1/personalization", tag = "personalization",
    responses((status = 204, description = "Everything is deleted")),
    security(("api_key" = ["personalization"])),
    description = "Scope: `personalization`. For privacy: a CV is personal data. Campaigns that used the profile have no profile after this, so their mails are held.")]
async fn delete_all(State(st): State<AppState>) -> ApiResult<axum::http::StatusCode> {
    let mut tx = st.pool.begin().await?;
    sources::delete_all(&mut tx).await?;
    sender::profile_changed(&mut tx).await?;
    tx.commit().await?;
    Ok(axum::http::StatusCode::NO_CONTENT)
}

// ---------- the profile ----------

#[derive(Serialize, ToSchema)]
pub struct GenerateReply {
    /// A draft. It is not saved.
    pub draft: Draft,
    /// The tags of the sources the AI read.
    pub sources: Vec<String>,
    pub tokens_in: i64,
    pub tokens_out: i64,
}

/// The AI drafts a sender profile from the cleaned sources. Nothing is saved.
#[utoipa::path(post, path = "/v1/personalization/profile/generate", tag = "personalization",
    responses((status = 200, body = GenerateReply), (status = 422, description = "no_sources, generator_not_ready (set up an AI provider first) or generation_failed"),
        (status = 429, description = "rate_limited: 10 a minute")),
    security(("api_key" = ["personalization"])),
    description = "Scope: `personalization`. Costs tokens. The sources are cut to about 6000 tokens (portfolio pages first, guide answers last), then sent. \
        The code removes dashes, drops unknown source tags, and clears a field that no source supports. `allow_low_personalization` is always false in a draft.")]
async fn generate_profile(State(st): State<AppState>, who: Option<Extension<Uuid>>) -> ApiResult<ApiJson<GenerateReply>> {
    crate::prompting::limit::check_user(&st, who.map(|e| e.0))?;
    let now = st.now();
    let mut conn = st.pool.acquire().await?;
    let list = sources::list(&mut conn).await?;
    if list.is_empty() {
        return Err(ApiError::unprocessable("no_sources", "Add something about you first: a link, a CV, some text, or answers to the guide questions."));
    }
    let settings = crate::generator::repo::load_settings(&mut conn).await?;
    let provider = crate::campaigns::email_routes::ready_provider(&mut conn)
        .await
        .map_err(|_| ApiError::unprocessable("generator_not_ready", "Set up an AI provider first (Settings, AI)."))?;
    let (cfg, _) = crate::generator::providers::load_config(&st.pool, &st.crypto, provider.id, settings.max_attempts)
        .await
        .map_err(|_| ApiError::unprocessable("generator_not_ready", "The AI model can not be used (its key can not be read)."))?;
    let client = st.llm.build(&cfg).map_err(|_| ApiError::unprocessable("generator_not_ready", "The AI model can not be used."))?;
    let chunks = sources::fit_budget(sources::chunks_of(&list), sources::TOKEN_BUDGET);
    let result = sender_builder::build_draft(client.as_ref(), &chunks, settings.max_output_tokens, settings.temperature).await;
    let calls = match &result {
        Ok((_, c)) => c.clone(),
        Err(f) => f.calls.clone(),
    };
    for c in &calls {
        crate::generator::repo::record_call(&mut conn, &c.record("sender_profile", Some(provider.id), None, &provider.model), now).await?;
    }
    let (tin, tout, _) = crate::generator::pipeline::totals(&calls);
    let (draft, _) = result.map_err(|f| ApiError::unprocessable("generation_failed", format!("{} ({})", f.message, f.code)))?;
    Ok(ApiJson(GenerateReply { draft, sources: chunks.iter().map(|c| c.tag.clone()).collect(), tokens_in: tin as i64, tokens_out: tout as i64 }))
}

#[derive(Deserialize, ToSchema)]
pub struct SaveProfile {
    pub fields: SenderFields,
    #[schema(value_type = Option<Vec<String>>)]
    pub other_offers: Option<Vec<String>>,
    /// Field key to the source tags the AI cited (from the draft).
    #[schema(value_type = Option<Object>)]
    pub sources_used: Option<Map<String, Value>>,
}

#[derive(Serialize, ToSchema)]
pub struct ProfileReply {
    /// The saved profile. Null when none is saved yet.
    pub profile: Option<SenderProfile>,
}

/// Save the profile as a new version. The old versions stay.
#[utoipa::path(put, path = "/v1/personalization/profile", tag = "personalization",
    request_body = SaveProfile,
    responses((status = 200, body = SenderProfile), (status = 422, description = "A field is longer than 1000 characters")),
    security(("api_key" = ["personalization"])),
    description = "Scope: `personalization`. Only a saved profile is used by campaigns; an AI draft never saves by itself. `allow_low_personalization` is set only here. \
        Unsent emails of campaigns with the optimized prompt are marked out of date.")]
async fn save_profile(State(st): State<AppState>, ApiJson(b): ApiJson<SaveProfile>) -> ApiResult<ApiJson<SenderProfile>> {
    let f = b.fields.cleaned().map_err(|m| ApiError::unprocessable("invalid_profile", m))?;
    let mut tx = st.pool.begin().await?;
    let saved = sender::save(&mut tx, &f, &json!(b.other_offers.unwrap_or_default()), &Value::Object(b.sources_used.unwrap_or_default()), st.now()).await?;
    sender::profile_changed(&mut tx).await?;
    tx.commit().await?;
    Ok(ApiJson(saved))
}

/// The saved profile.
#[utoipa::path(get, path = "/v1/personalization/profile", tag = "personalization",
    responses((status = 200, body = ProfileReply)),
    security(("api_key" = ["personalization"])), description = "Scope: `personalization`. The newest saved version, or `profile: null`.")]
async fn get_profile(State(st): State<AppState>) -> ApiResult<ApiJson<ProfileReply>> {
    let mut conn = st.pool.acquire().await?;
    Ok(ApiJson(ProfileReply { profile: sender::current(&mut conn).await? }))
}

/// The last 10 saved versions, newest first.
#[utoipa::path(get, path = "/v1/personalization/profile/versions", tag = "personalization",
    responses((status = 200, body = Vec<SenderProfile>)),
    security(("api_key" = ["personalization"])), description = "Scope: `personalization`.")]
async fn list_versions(State(st): State<AppState>) -> ApiResult<ApiJson<Vec<SenderProfile>>> {
    let mut conn = st.pool.acquire().await?;
    Ok(ApiJson(sender::versions(&mut conn, 10).await?))
}
