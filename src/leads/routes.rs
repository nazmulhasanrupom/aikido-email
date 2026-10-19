//! `/v1/leads/*` (scope `leads`)
use super::context::{self, LeadContext};
use super::events::{self, EVENT_COLS};
use super::fields::{self, FieldDefs, MAX_FIELDS, MAX_VALUE_CHARS};
use super::input::{LeadInput, RawLead};
use super::model::*;
use super::query::{LeadFilter, PageQuery, cursor_position, matching_ids};
use super::repo::{self, EraseReport, LeadDetail, PatchError};
use super::settings::{LeadSettings, SettingsError};
use super::upsert::{Outcome, UpsertCtx, upsert_batch};
use super::verify::{self, VerifyReport};
use crate::auth::api_keys::Scope;
use crate::auth::middleware::require_scope;
use crate::error::{ApiError, ApiJson, ApiPath, ApiQuery, ApiResult};
use crate::state::AppState;
use axum::extract::State;
use axum::http::StatusCode;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::time::Duration;
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

/// Longest wait for DNS inside a request. The leads are saved before it starts.
const MX_BUDGET_REQUEST: Duration = Duration::from_secs(10);
pub const MAX_BULK: usize = 1000;
pub const MAX_ACTION: i64 = 10_000;

/// Routes that need a key with scope `leads`.
pub fn router(state: AppState) -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(get_settings, patch_settings))
        .routes(routes!(list_fields, create_field))
        .routes(routes!(patch_field, delete_field))
        .routes(routes!(list_lists, create_list))
        .routes(routes!(patch_list, delete_list))
        .routes(routes!(create_lead, list_leads))
        .routes(routes!(bulk_upsert))
        .routes(routes!(run_action))
        .routes(routes!(get_lead, patch_lead, delete_lead))
        .routes(routes!(list_events))
        .routes(routes!(lead_context))
        .routes(routes!(add_note))
        .merge(super::import::router())
        .merge(super::list_rows::router())
        .merge(super::mapping::routes::router())
        .merge(super::export::router())
        .route_layer(axum::middleware::from_fn_with_state((state, Scope::Leads), require_scope))
}

pub fn limit_of(limit: Option<i64>) -> ApiResult<i64> {
    let limit = limit.unwrap_or(50);
    if (1..=500).contains(&limit) { Ok(limit) } else { Err(ApiError::unprocessable("invalid_limit", "limit must be 1 to 500.")) }
}

fn lead_not_found() -> ApiError {
    ApiError::not_found("lead_not_found", "No such lead.")
}

fn invalid_lead(message: impl Into<String>) -> ApiError {
    ApiError::unprocessable("invalid_lead", message)
}

/// The cursor is the id of the last lead. Its position is read first. A deleted cursor lead is an error.
async fn resolve_cursor(st: &AppState, cursor: Option<Uuid>) -> ApiResult<Option<(DateTime<Utc>, Uuid)>> {
    let Some(id) = cursor else { return Ok(None) };
    let mut conn = st.pool.acquire().await?;
    match cursor_position(&mut conn, id).await? {
        Some(pos) => Ok(Some(pos)),
        None => Err(ApiError::bad_request("invalid_cursor", "The cursor lead does not exist any more. Start again without a cursor.")),
    }
}

// ---------- settings ----------

pub async fn load_settings<'e>(db: impl sqlx::PgExecutor<'e>) -> Result<LeadSettings, sqlx::Error> {
    let (v,): (Value,) = sqlx::query_as("SELECT settings FROM lead_defaults WHERE id = 1").fetch_one(db).await?;
    Ok(LeadSettings::from_stored(v))
}

fn settings_error(e: SettingsError) -> ApiError {
    ApiError::unprocessable(e.code, e.message)
}

#[derive(Serialize, ToSchema)]
pub struct SettingsReply {
    pub settings: LeadSettings,
}

/// Read the lead settings.
#[utoipa::path(get, path = "/v1/leads/settings", tag = "leads",
    responses((status = 200, body = SettingsReply)),
    security(("api_key" = ["leads"])), description = "Scope: `leads`.")]
async fn get_settings(State(st): State<AppState>) -> ApiResult<ApiJson<SettingsReply>> {
    Ok(ApiJson(SettingsReply { settings: load_settings(&st.pool).await? }))
}

/// Change the lead settings. Send only the fields to change.
#[utoipa::path(patch, path = "/v1/leads/settings", tag = "leads",
    request_body(content = LeadSettings, description = "Only the fields to change."),
    responses((status = 200, body = SettingsReply), (status = 422, description = "A setting is not valid")),
    security(("api_key" = ["leads"])),
    description = "Scope: `leads`. Merges the fields, validates the full result, then saves.")]
async fn patch_settings(State(st): State<AppState>, ApiJson(patch): ApiJson<Value>) -> ApiResult<ApiJson<SettingsReply>> {
    let mut tx = st.pool.begin().await?;
    let (current,): (Value,) = sqlx::query_as("SELECT settings FROM lead_defaults WHERE id = 1 FOR UPDATE").fetch_one(&mut *tx).await?;
    let mut merged = LeadSettings::from_stored(current).merged(&patch).map_err(settings_error)?;
    merged.validate().map_err(settings_error)?;
    sqlx::query("UPDATE lead_defaults SET settings = $1, updated_at = $2 WHERE id = 1").bind(serde_json::to_value(&merged).unwrap_or_default()).bind(st.now()).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(ApiJson(SettingsReply { settings: merged }))
}

// ---------- custom fields ----------

/// All custom field definitions.
#[utoipa::path(get, path = "/v1/leads/fields", tag = "leads",
    responses((status = 200, body = Vec<LeadField>)),
    security(("api_key" = ["leads"])), description = "Scope: `leads`. Sorted by key.")]
async fn list_fields(State(st): State<AppState>) -> ApiResult<ApiJson<Vec<LeadField>>> {
    let mut conn = st.pool.acquire().await?;
    let mut defs: Vec<LeadField> = FieldDefs::load(&mut conn).await?.by_key.into_values().collect();
    defs.sort_by(|a, b| a.key.cmp(&b.key));
    Ok(ApiJson(defs))
}

#[derive(Deserialize, ToSchema)]
pub struct CreateField {
    /// Lower snake case, 1 to 40 characters, starts with a letter. Not a built-in name like `email`.
    pub key: String,
    /// Default: the key.
    pub label: Option<String>,
    /// Default `text`.
    pub kind: Option<FieldKind>,
    /// Used in templates when the lead has no value.
    pub fallback: Option<String>,
}

fn clean_label(label: &str) -> ApiResult<String> {
    let label = label.trim();
    if label.is_empty() || label.chars().count() > 100 {
        return Err(ApiError::unprocessable("invalid_label", "label must be 1 to 100 characters."));
    }
    Ok(label.to_string())
}

fn clean_fallback(v: Option<&str>) -> ApiResult<Option<String>> {
    super::input::clean_text("fallback", v, MAX_VALUE_CHARS).map_err(|m| ApiError::unprocessable("invalid_fallback", m))
}

/// Make a custom field. It becomes the template variable `{{key}}`.
#[utoipa::path(post, path = "/v1/leads/fields", tag = "leads",
    request_body = CreateField,
    responses((status = 201, body = LeadField), (status = 409, description = "The key exists"), (status = 422, description = "Bad key, or 50 fields already")),
    security(("api_key" = ["leads"])), description = "Scope: `leads`. Up to 50 fields.")]
async fn create_field(State(st): State<AppState>, ApiJson(body): ApiJson<CreateField>) -> ApiResult<(StatusCode, ApiJson<LeadField>)> {
    let key = body.key.trim().to_string();
    fields::check_key(&key).map_err(|m| ApiError::unprocessable("invalid_key", m))?;
    let label = clean_label(body.label.as_deref().unwrap_or(&key))?;
    let fallback = clean_fallback(body.fallback.as_deref())?;
    let mut tx = st.pool.begin().await?;
    // two requests at once must not pass 50
    sqlx::query("SELECT pg_advisory_xact_lock(hashtext('lead_fields'))").execute(&mut *tx).await?;
    let (n,): (i64,) = sqlx::query_as("SELECT count(*) FROM lead_fields").fetch_one(&mut *tx).await?;
    if n >= MAX_FIELDS {
        return Err(ApiError::unprocessable("too_many_fields", format!("There can be up to {MAX_FIELDS} custom fields.")));
    }
    let row: Option<LeadField> = sqlx::query_as(
        "INSERT INTO lead_fields (key, label, kind, fallback, created_at) VALUES ($1, $2, $3, $4, $5)
         ON CONFLICT (key) DO NOTHING RETURNING key, label, kind, fallback, created_at",
    )
    .bind(&key)
    .bind(&label)
    .bind(body.kind.unwrap_or(FieldKind::Text))
    .bind(&fallback)
    .bind(st.now())
    .fetch_optional(&mut *tx)
    .await?;
    let Some(field) = row else { return Err(ApiError::conflict("field_exists", format!("The field '{key}' exists."))) };
    tx.commit().await?;
    Ok((StatusCode::CREATED, ApiJson(field)))
}

#[derive(Deserialize, ToSchema)]
#[allow(dead_code)]
pub struct PatchField {
    pub label: Option<String>,
    /// `null` removes the fallback.
    pub fallback: Option<String>,
}

/// Change the label or the fallback of a field. The key and the kind can not change.
#[utoipa::path(patch, path = "/v1/leads/fields/{key}", tag = "leads",
    params(("key" = String, Path)), request_body = PatchField,
    responses((status = 200, body = LeadField), (status = 404, description = "No such field")),
    security(("api_key" = ["leads"])), description = "Scope: `leads`.")]
async fn patch_field(State(st): State<AppState>, ApiPath(key): ApiPath<String>, ApiJson(patch): ApiJson<Map<String, Value>>) -> ApiResult<ApiJson<LeadField>> {
    let mut tx = st.pool.begin().await?;
    let mut field: LeadField = sqlx::query_as("SELECT key, label, kind, fallback, created_at FROM lead_fields WHERE key = $1 FOR UPDATE")
        .bind(&key)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| ApiError::not_found("field_not_found", "No such field."))?;
    for (k, v) in &patch {
        match (k.as_str(), v) {
            ("label", Value::String(s)) => field.label = clean_label(s)?,
            ("fallback", Value::Null) => field.fallback = None,
            ("fallback", Value::String(s)) => field.fallback = clean_fallback(Some(s))?,
            ("label" | "fallback", _) => return Err(ApiError::unprocessable("invalid_field", format!("{k} must be text"))),
            ("key" | "kind", _) => return Err(ApiError::unprocessable("field_locked", format!("The {k} of a field can not change. Make a new field."))),
            _ => return Err(ApiError::unprocessable("unknown_field", format!("'{k}' is not a field property."))),
        }
    }
    sqlx::query("UPDATE lead_fields SET label = $2, fallback = $3 WHERE key = $1").bind(&key).bind(&field.label).bind(&field.fallback).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(ApiJson(field))
}

#[derive(Serialize, ToSchema)]
pub struct DeletedReply {
    pub deleted: bool,
}

#[derive(Serialize, ToSchema)]
pub struct DeleteFieldReply {
    pub deleted: bool,
    /// Leads that had a value for this field. The value is removed.
    pub leads_cleaned: i64,
}

/// Delete a field. Its value is removed from every lead.
#[utoipa::path(delete, path = "/v1/leads/fields/{key}", tag = "leads",
    params(("key" = String, Path)),
    responses((status = 200, body = DeleteFieldReply), (status = 404, description = "No such field"),
        (status = 409, description = "field_in_use: a sequence step still uses it")),
    security(("api_key" = ["leads"])),
    description = "Scope: `leads`. Refused with `409 field_in_use` (the message names the sequences) while a sequence with an open enrollment, \
        or the sequence of an active or paused campaign, has a step that uses `{{key}}`. An archived sequence counts too, because it can be resumed.")]
async fn delete_field(State(st): State<AppState>, ApiPath(key): ApiPath<String>) -> ApiResult<ApiJson<DeleteFieldReply>> {
    let mut tx = st.pool.begin().await?;
    let exists: Option<(String,)> = sqlx::query_as("SELECT key FROM lead_fields WHERE key = $1 FOR UPDATE").bind(&key).fetch_optional(&mut *tx).await?;
    if exists.is_none() {
        return Err(ApiError::not_found("field_not_found", "No such field."));
    }
    let used_by = fields::sequences_using(&mut tx, &key).await?;
    if !used_by.is_empty() {
        return Err(ApiError::conflict(
            "field_in_use",
            format!("The field '{key}' is used by a step of: {}. Change those steps first, or finish or stop their enrollments.", used_by.join(", ")),
        ));
    }
    sqlx::query("DELETE FROM lead_fields WHERE key = $1").bind(&key).execute(&mut *tx).await?;
    let cleaned = sqlx::query("UPDATE leads SET custom_fields = custom_fields - $1::text WHERE custom_fields ? $1::text").bind(&key).execute(&mut *tx).await?.rows_affected();
    tx.commit().await?;
    Ok(ApiJson(DeleteFieldReply { deleted: true, leads_cleaned: cleaned as i64 }))
}

// ---------- lists ----------

pub(crate) const LIST_COLS: &str = "l.id, l.name, l.description, l.created_at, l.updated_at, (SELECT count(*) FROM lead_list_members m WHERE m.list_id = l.id) AS member_count, \
    l.columns, (SELECT count(*) FROM lead_list_rows r WHERE r.list_id = l.id) AS row_count";

/// All lists, with the number of leads in each.
#[utoipa::path(get, path = "/v1/leads/lists", tag = "leads",
    responses((status = 200, body = Vec<LeadList>)),
    security(("api_key" = ["leads"])), description = "Scope: `leads`. Sorted by name.")]
async fn list_lists(State(st): State<AppState>) -> ApiResult<ApiJson<Vec<LeadList>>> {
    Ok(ApiJson(sqlx::query_as(&format!("SELECT {LIST_COLS} FROM lead_lists l ORDER BY l.name")).fetch_all(&st.pool).await?))
}

#[derive(Deserialize, ToSchema)]
pub struct CreateList {
    /// 1 to 200 characters. Unique.
    pub name: String,
    pub description: Option<String>,
}

fn clean_list_name(name: &str) -> ApiResult<String> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 200 {
        return Err(ApiError::unprocessable("invalid_name", "name must be 1 to 200 characters."));
    }
    Ok(name.to_string())
}

fn clean_description(d: Option<&str>) -> ApiResult<Option<String>> {
    super::input::clean_text("description", d, 1000).map_err(|m| ApiError::unprocessable("invalid_description", m))
}

pub(crate) async fn get_list(st: &AppState, id: Uuid) -> ApiResult<LeadList> {
    sqlx::query_as(&format!("SELECT {LIST_COLS} FROM lead_lists l WHERE l.id = $1"))
        .bind(id)
        .fetch_optional(&st.pool)
        .await?
        .ok_or_else(|| ApiError::not_found("list_not_found", "No such list."))
}

fn name_clash(e: sqlx::Error) -> ApiError {
    if e.as_database_error().is_some_and(|d| d.is_unique_violation()) {
        ApiError::conflict("list_exists", "A list with this name exists.")
    } else {
        e.into()
    }
}

/// Make a list.
#[utoipa::path(post, path = "/v1/leads/lists", tag = "leads",
    request_body = CreateList,
    responses((status = 201, body = LeadList), (status = 409, description = "The name exists")),
    security(("api_key" = ["leads"])), description = "Scope: `leads`. Add leads with `POST /v1/leads/actions` (`add_to_list`) or at import.")]
async fn create_list(State(st): State<AppState>, ApiJson(body): ApiJson<CreateList>) -> ApiResult<(StatusCode, ApiJson<LeadList>)> {
    let name = clean_list_name(&body.name)?;
    let description = clean_description(body.description.as_deref())?;
    let id = Uuid::now_v7();
    let now = st.now();
    sqlx::query("INSERT INTO lead_lists (id, name, description, created_at, updated_at) VALUES ($1, $2, $3, $4, $4)")
        .bind(id)
        .bind(&name)
        .bind(&description)
        .bind(now)
        .execute(&st.pool)
        .await
        .map_err(name_clash)?;
    Ok((StatusCode::CREATED, ApiJson(get_list(&st, id).await?)))
}

#[derive(Deserialize, ToSchema)]
#[allow(dead_code)]
pub struct PatchList {
    pub name: Option<String>,
    /// `null` removes the description.
    pub description: Option<String>,
}

/// Rename a list or change its description.
#[utoipa::path(patch, path = "/v1/leads/lists/{id}", tag = "leads",
    params(("id" = Uuid, Path)), request_body = PatchList,
    responses((status = 200, body = LeadList), (status = 404, description = "No such list"), (status = 409, description = "The name exists")),
    security(("api_key" = ["leads"])), description = "Scope: `leads`.")]
async fn patch_list(State(st): State<AppState>, ApiPath(id): ApiPath<Uuid>, ApiJson(patch): ApiJson<Map<String, Value>>) -> ApiResult<ApiJson<LeadList>> {
    let current = get_list(&st, id).await?;
    let (mut name, mut description) = (current.name, current.description);
    for (k, v) in &patch {
        match (k.as_str(), v) {
            ("name", Value::String(s)) => name = clean_list_name(s)?,
            ("description", Value::Null) => description = None,
            ("description", Value::String(s)) => description = clean_description(Some(s))?,
            ("name" | "description", _) => return Err(ApiError::unprocessable("invalid_list", format!("{k} must be text"))),
            _ => return Err(ApiError::unprocessable("unknown_field", format!("'{k}' can not be changed."))),
        }
    }
    sqlx::query("UPDATE lead_lists SET name = $2, description = $3, updated_at = $4 WHERE id = $1")
        .bind(id)
        .bind(&name)
        .bind(&description)
        .bind(st.now())
        .execute(&st.pool)
        .await
        .map_err(name_clash)?;
    Ok(ApiJson(get_list(&st, id).await?))
}

/// Delete a list. The leads in it stay.
#[utoipa::path(delete, path = "/v1/leads/lists/{id}", tag = "leads",
    params(("id" = Uuid, Path)),
    responses((status = 200, body = DeletedReply), (status = 404, description = "No such list")),
    security(("api_key" = ["leads"])), description = "Scope: `leads`. Deleting a list does not delete its leads.")]
async fn delete_list(State(st): State<AppState>, ApiPath(id): ApiPath<Uuid>) -> ApiResult<ApiJson<DeletedReply>> {
    if sqlx::query("DELETE FROM lead_lists WHERE id = $1").bind(id).execute(&st.pool).await?.rows_affected() == 0 {
        return Err(ApiError::not_found("list_not_found", "No such list."));
    }
    Ok(ApiJson(DeletedReply { deleted: true }))
}

/// The campaign must exist and still take leads.
pub async fn require_campaign(conn: &mut sqlx::PgConnection, id: Uuid) -> ApiResult<()> {
    match repo::campaign_takes_leads(conn, id).await? {
        None => Err(ApiError::not_found("campaign_not_found", "No such campaign.")),
        Some(false) => Err(ApiError::conflict("campaign_closed", "The campaign is completed or archived. It takes no more leads.")),
        Some(true) => Ok(()),
    }
}

async fn require_list(conn: &mut sqlx::PgConnection, id: Uuid) -> ApiResult<()> {
    let found: Option<(Uuid,)> = sqlx::query_as("SELECT id FROM lead_lists WHERE id = $1").bind(id).fetch_optional(conn).await?;
    found.map(|_| ()).ok_or_else(|| ApiError::not_found("list_not_found", "No such list."))
}

// ---------- one lead ----------

#[derive(Deserialize, ToSchema)]
pub struct CreateLead {
    #[serde(flatten)]
    pub lead: RawLead,
    /// What a person decided about this lead.
    pub interest: Option<Interest>,
}

/// Make one lead.
#[utoipa::path(post, path = "/v1/leads", tag = "leads",
    request_body = CreateLead,
    responses((status = 201, body = LeadDetail), (status = 409, description = "The email exists"), (status = 422, description = "A value is not valid")),
    security(("api_key" = ["leads"])),
    description = "Scope: `leads`. The email is trimmed and lower-cased. The MX check runs before the reply (up to 10 seconds); on a timeout the lead stays `syntax_ok`.")]
async fn create_lead(State(st): State<AppState>, ApiJson(body): ApiJson<CreateLead>) -> ApiResult<(StatusCode, ApiJson<LeadDetail>)> {
    let now = st.now();
    let mut tx = st.pool.begin().await?;
    let defs = FieldDefs::load(&mut tx).await?;
    let input = body.lead.check(&defs).map_err(invalid_lead)?;
    let ctx = UpsertCtx { mode: DedupeMode::Skip, source: LeadSource::Api, source_ref: None, ref_id: None, now };
    let res = upsert_batch(&mut tx, std::slice::from_ref(&input), &ctx).await?;
    let made = res.first().copied().ok_or_else(ApiError::internal)?;
    if made.outcome != Outcome::Created {
        return Err(ApiError::conflict("email_exists", format!("A lead with the email {} exists.", input.email)));
    }
    if body.interest.is_some() {
        repo::set_interest(&mut tx, &[made.id], body.interest, now).await?;
    }
    tx.commit().await?;
    verify::verify_domains(&st, std::slice::from_ref(&input.domain), false, MX_BUDGET_REQUEST).await?;
    let mut conn = st.pool.acquire().await?;
    let lead = repo::get(&mut conn, made.id).await?.ok_or_else(ApiError::internal)?;
    Ok((StatusCode::CREATED, ApiJson(repo::detail(&mut conn, lead).await?)))
}

#[derive(Serialize, ToSchema)]
pub struct ListLeadsReply {
    pub leads: Vec<Lead>,
    /// Pass it as `cursor` to get the next page. `null`: this was the last page.
    pub next_cursor: Option<Uuid>,
}

/// List leads, newest first. Filters and cursor paging.
#[utoipa::path(get, path = "/v1/leads", tag = "leads",
    params(LeadFilter, PageQuery),
    responses((status = 200, body = ListLeadsReply), (status = 400, description = "invalid_cursor: the cursor lead was deleted")),
    security(("api_key" = ["leads"])),
    description = "Scope: `leads`. `cursor` is the id of the last lead of the previous page; the server reads that lead's position first, so a deleted cursor lead gives `invalid_cursor`.")]
async fn list_leads(State(st): State<AppState>, ApiQuery(filter): ApiQuery<LeadFilter>, ApiQuery(page): ApiQuery<PageQuery>) -> ApiResult<ApiJson<ListLeadsReply>> {
    let limit = limit_of(page.limit)?;
    let after = resolve_cursor(&st, page.cursor).await?;
    let mut conn = st.pool.acquire().await?;
    let mut leads = repo::list(&mut conn, &filter, after, limit).await?;
    let next_cursor = if leads.len() as i64 > limit {
        leads.truncate(limit as usize);
        leads.last().map(|l| l.id)
    } else {
        None
    };
    let ids: Vec<Uuid> = leads.iter().map(|l| l.id).collect();
    let briefs = crate::verify_reoon::auto::briefs(&mut conn, &ids).await?;
    for l in &mut leads {
        if let Some(b) = briefs.get(&l.id) {
            (l.verification_status, l.verification_score, l.verification_checked_at) = (Some(b.status.clone()), b.score, Some(b.checked_at));
        }
    }
    Ok(ApiJson(ListLeadsReply { leads, next_cursor }))
}

/// One lead, with its lists, the suppressed flag and the open enrollment.
#[utoipa::path(get, path = "/v1/leads/{id}", tag = "leads",
    params(("id" = Uuid, Path)),
    responses((status = 200, body = LeadDetail), (status = 404, description = "No such lead")),
    security(("api_key" = ["leads"])), description = "Scope: `leads`.")]
async fn get_lead(State(st): State<AppState>, ApiPath(id): ApiPath<Uuid>) -> ApiResult<ApiJson<LeadDetail>> {
    let mut conn = st.pool.acquire().await?;
    let lead = repo::get(&mut conn, id).await?.ok_or_else(lead_not_found)?;
    Ok(ApiJson(repo::detail(&mut conn, lead).await?))
}

#[derive(Deserialize, ToSchema)]
#[allow(dead_code)]
pub struct PatchLead {
    /// Refused (409 `email_locked`) while the lead has an open enrollment.
    pub email: Option<String>,
    pub first_name: Option<String>,
    pub last_name: Option<String>,
    pub company_name: Option<String>,
    pub job_title: Option<String>,
    pub phone: Option<String>,
    pub website: Option<String>,
    pub linkedin_url: Option<String>,
    pub location: Option<String>,
    pub timezone: Option<String>,
    pub industry: Option<String>,
    pub personalization_note: Option<String>,
    pub trigger_event: Option<String>,
    pub trigger_verified: Option<bool>,
    /// Merged into the old values. A `null` value removes the key.
    #[schema(value_type = Option<Object>)]
    pub custom_fields: Option<Map<String, Value>>,
    /// Replaces the tags.
    pub tags: Option<Vec<String>>,
    /// `null` clears it.
    pub interest: Option<Interest>,
    pub notes: Option<String>,
}

/// Change a lead. Send only the fields to change. `null` or empty text clears a field.
#[utoipa::path(patch, path = "/v1/leads/{id}", tag = "leads",
    params(("id" = Uuid, Path)), request_body = PatchLead,
    responses((status = 200, body = LeadDetail), (status = 404, description = "No such lead"),
        (status = 409, description = "email_exists, or email_locked (an open enrollment)"), (status = 422, description = "A value is not valid")),
    security(("api_key" = ["leads"])), description = "Scope: `leads`. Contact state and verification are set by the system, not here.")]
async fn patch_lead(State(st): State<AppState>, ApiPath(id): ApiPath<Uuid>, ApiJson(patch): ApiJson<Value>) -> ApiResult<ApiJson<LeadDetail>> {
    let Value::Object(patch) = patch else { return Err(invalid_lead("The body must be a JSON object.")) };
    let now = st.now();
    let mut tx = st.pool.begin().await?;
    let defs = FieldDefs::load(&mut tx).await?;
    let before = repo::get(&mut tx, id).await?.ok_or_else(lead_not_found)?;
    let lead = repo::patch(&mut tx, id, &patch, &defs, now).await.map_err(|e| match e {
        PatchError::NotFound => lead_not_found(),
        PatchError::Invalid(m) => invalid_lead(m),
        PatchError::EmailTaken => ApiError::conflict("email_exists", "A lead with this email exists."),
        PatchError::EmailLocked => ApiError::conflict("email_locked", "The lead has an open enrollment. Stop it before you change the email."),
        PatchError::Db(e) => e.into(),
    })?;
    tx.commit().await?;
    if lead.email != before.email {
        verify::verify_domains(&st, std::slice::from_ref(&lead.email_domain), false, MX_BUDGET_REQUEST).await?;
    }
    let mut conn = st.pool.acquire().await?;
    // Saved emails that used this lead's values are now out of date.
    crate::campaigns::emails::lead_changed(&mut conn, id).await?;
    let lead = repo::get(&mut conn, id).await?.ok_or_else(lead_not_found)?;
    Ok(ApiJson(repo::detail(&mut conn, lead).await?))
}

/// Delete a lead for good.
#[utoipa::path(delete, path = "/v1/leads/{id}", tag = "leads",
    params(("id" = Uuid, Path)),
    responses((status = 200, body = EraseReport), (status = 404, description = "No such lead")),
    security(("api_key" = ["leads"])),
    description = "Scope: `leads`. Open enrollments are stopped and their queued emails are cancelled. The address is added to the suppression list with reason `erased`, so it is never contacted again, even if it is imported again. The timeline is deleted with the lead. To email the address again, remove it with `DELETE /v1/sending/suppressions`.")]
async fn delete_lead(State(st): State<AppState>, ApiPath(id): ApiPath<Uuid>) -> ApiResult<ApiJson<EraseReport>> {
    let mut tx = st.pool.begin().await?;
    if repo::get(&mut tx, id).await?.is_none() {
        return Err(lead_not_found());
    }
    let report = repo::erase(&mut tx, &[id], st.now()).await?;
    tx.commit().await?;
    Ok(ApiJson(report))
}

// ---------- timeline and notes ----------

#[derive(Deserialize, IntoParams)]
pub struct EventsQuery {
    /// The `next_cursor` of the last page.
    cursor: Option<i64>,
    /// 1 to 500. Default 50.
    limit: Option<i64>,
}

#[derive(Serialize, ToSchema)]
pub struct EventsReply {
    pub events: Vec<LeadEvent>,
    pub next_cursor: Option<i64>,
}

/// The timeline of a lead, newest first.
#[utoipa::path(get, path = "/v1/leads/{id}/events", tag = "leads",
    params(("id" = Uuid, Path), EventsQuery),
    responses((status = 200, body = EventsReply), (status = 404, description = "No such lead")),
    security(("api_key" = ["leads"])), description = "Scope: `leads`.")]
async fn list_events(State(st): State<AppState>, ApiPath(id): ApiPath<Uuid>, ApiQuery(q): ApiQuery<EventsQuery>) -> ApiResult<ApiJson<EventsReply>> {
    let limit = limit_of(q.limit)?;
    let mut conn = st.pool.acquire().await?;
    if repo::get(&mut conn, id).await?.is_none() {
        return Err(lead_not_found());
    }
    let mut events = events::list(&mut conn, id, q.cursor, limit + 1).await?;
    let next_cursor = if events.len() as i64 > limit {
        events.truncate(limit as usize);
        events.last().map(|e| e.id)
    } else {
        None
    };
    Ok(ApiJson(EventsReply { events, next_cursor }))
}

#[derive(Deserialize, ToSchema)]
pub struct AddNote {
    /// 1 to 5000 characters.
    pub text: String,
}

/// Add a note to the timeline.
#[utoipa::path(post, path = "/v1/leads/{id}/notes", tag = "leads",
    params(("id" = Uuid, Path)), request_body = AddNote,
    responses((status = 201, body = LeadEvent), (status = 404, description = "No such lead")),
    security(("api_key" = ["leads"])), description = "Scope: `leads`. Writes an event of kind `note`.")]
async fn add_note(State(st): State<AppState>, ApiPath(id): ApiPath<Uuid>, ApiJson(body): ApiJson<AddNote>) -> ApiResult<(StatusCode, ApiJson<LeadEvent>)> {
    let text = body.text.trim();
    if text.is_empty() || text.chars().count() > 5000 {
        return Err(ApiError::unprocessable("invalid_note", "text must be 1 to 5000 characters."));
    }
    let mut conn = st.pool.acquire().await?;
    if repo::get(&mut conn, id).await?.is_none() {
        return Err(lead_not_found());
    }
    let event: LeadEvent = sqlx::query_as(&format!("INSERT INTO lead_events (lead_id, kind, detail, at) VALUES ($1, 'note', $2, $3) RETURNING {EVENT_COLS}"))
        .bind(id)
        .bind(json!({ "text": text }))
        .bind(st.now())
        .fetch_one(&mut *conn)
        .await?;
    Ok((StatusCode::CREATED, ApiJson(event)))
}

// ---------- bulk upsert (JSON) ----------

#[derive(Deserialize, ToSchema)]
pub struct BulkLeads {
    /// 1 to 1000 leads.
    pub leads: Vec<RawLead>,
    /// What to do when an email exists. Default: the `default_dedupe_mode` setting.
    pub dedupe_mode: Option<DedupeMode>,
    /// Put every lead in this list.
    pub list_id: Option<Uuid>,
    /// Add these tags to every lead.
    pub tags: Option<Vec<String>>,
    /// Add every lead to this campaign (as `pending`; the feeder enrolls them over the days).
    pub campaign_id: Option<Uuid>,
}

#[derive(Serialize, ToSchema, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum ItemStatus {
    Created,
    Updated,
    Skipped,
    Invalid,
}

#[derive(Serialize, ToSchema, Debug)]
pub struct ItemError {
    pub code: String,
    pub message: String,
}

#[derive(Serialize, ToSchema, Debug)]
pub struct BulkItem {
    pub index: usize,
    pub status: ItemStatus,
    pub lead_id: Option<Uuid>,
    pub error: Option<ItemError>,
}

#[derive(Serialize, ToSchema)]
pub struct BulkReply {
    pub created: i64,
    pub updated: i64,
    pub skipped: i64,
    pub invalid: i64,
    /// One entry per lead you sent, in the same order.
    pub items: Vec<BulkItem>,
}

/// Add or update up to 1000 leads in one call.
#[utoipa::path(post, path = "/v1/leads/bulk", tag = "leads",
    request_body = BulkLeads,
    responses((status = 200, body = BulkReply), (status = 404, description = "No such list or campaign"), (status = 409, description = "The campaign is completed or archived"), (status = 422, description = "Not 1 to 1000 leads, or a bad tag")),
    security(("api_key" = ["leads"])),
    description = "Scope: `leads`. A bad lead does not stop the others: it gets `invalid` with the reason. Duplicate modes: `skip`, `fill_empty`, `overwrite`. Two rows with the same email: the first wins, the second is `skipped`. Suppressed emails are saved too. They are skipped when enrolled.")]
async fn bulk_upsert(State(st): State<AppState>, ApiJson(body): ApiJson<BulkLeads>) -> ApiResult<ApiJson<BulkReply>> {
    if body.leads.is_empty() || body.leads.len() > MAX_BULK {
        return Err(ApiError::unprocessable("invalid_bulk", format!("Send 1 to {MAX_BULK} leads.")));
    }
    let tags = fields::clean_tags(body.tags.as_deref().unwrap_or(&[])).map_err(|m| ApiError::unprocessable("invalid_tags", m))?;
    let mode = match body.dedupe_mode {
        Some(m) => m,
        None => load_settings(&st.pool).await?.default_dedupe_mode,
    };
    let now = st.now();
    let mut tx = st.pool.begin().await?;
    if let Some(list_id) = body.list_id {
        require_list(&mut tx, list_id).await?;
    }
    if let Some(campaign_id) = body.campaign_id {
        require_campaign(&mut tx, campaign_id).await?;
    }
    let defs = FieldDefs::load(&mut tx).await?;
    let mut items: Vec<Option<BulkItem>> = Vec::new();
    let mut valid: Vec<(usize, LeadInput)> = Vec::new();
    for (i, raw) in body.leads.iter().enumerate() {
        match raw.check(&defs) {
            Ok(l) => {
                valid.push((i, l));
                items.push(None);
            }
            Err(message) => items.push(Some(BulkItem { index: i, status: ItemStatus::Invalid, lead_id: None, error: Some(ItemError { code: "invalid_lead".into(), message }) })),
        }
    }
    let inputs: Vec<LeadInput> = valid.iter().map(|(_, l)| l.clone()).collect();
    let ctx = UpsertCtx { mode, source: LeadSource::Api, source_ref: None, ref_id: None, now };
    let results = upsert_batch(&mut tx, &inputs, &ctx).await?;
    let ids: Vec<Uuid> = results.iter().map(|r| r.id).collect();
    if let Some(list_id) = body.list_id {
        repo::add_to_list(&mut tx, list_id, &ids, now).await?;
    }
    if !tags.is_empty() {
        repo::add_tags(&mut tx, &ids, &tags, now).await?;
    }
    if let Some(campaign_id) = body.campaign_id {
        repo::add_to_campaign(&mut tx, campaign_id, &ids, now).await?;
    }
    tx.commit().await?;
    for ((i, _), r) in valid.iter().zip(&results) {
        let status = match r.outcome {
            Outcome::Created => ItemStatus::Created,
            Outcome::Updated => ItemStatus::Updated,
            Outcome::Skipped => ItemStatus::Skipped,
        };
        items[*i] = Some(BulkItem { index: *i, status, lead_id: Some(r.id), error: None });
    }
    let items: Vec<BulkItem> = items.into_iter().flatten().collect();
    let count = |s: ItemStatus| items.iter().filter(|i| i.status == s).count() as i64;
    let reply = BulkReply { created: count(ItemStatus::Created), updated: count(ItemStatus::Updated), skipped: count(ItemStatus::Skipped), invalid: count(ItemStatus::Invalid), items };
    let new_domains: Vec<String> = valid.iter().zip(&results).filter(|(_, r)| r.outcome == Outcome::Created).map(|((_, l), _)| l.domain.clone()).collect();
    verify::verify_domains(&st, &new_domains, false, MX_BUDGET_REQUEST).await?;
    Ok(ApiJson(reply))
}

// ---------- bulk actions ----------

#[derive(Deserialize, ToSchema, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum LeadAction {
    AddToList,
    RemoveFromList,
    AddTags,
    RemoveTags,
    SetInterest,
    Delete,
    Verify,
}

#[derive(Deserialize, ToSchema)]
pub struct ActionBody {
    pub action: LeadAction,
    /// Pick leads by id. Give this or `filter`, not both.
    pub lead_ids: Option<Vec<Uuid>>,
    /// Pick leads with the same filter as `GET /v1/leads`. It needs at least one condition.
    pub filter: Option<LeadFilter>,
    /// For `add_to_list` and `remove_from_list`.
    pub list_id: Option<Uuid>,
    /// For `add_tags` and `remove_tags`.
    pub tags: Option<Vec<String>>,
    /// For `set_interest`. Leave out or `null` to clear it.
    pub interest: Option<Interest>,
}

#[derive(Serialize, ToSchema)]
pub struct ActionReply {
    /// Leads that matched the selection.
    pub matched: i64,
    /// Leads that changed.
    pub changed: i64,
    /// For `delete`: what was stopped.
    pub erased: Option<EraseReport>,
    /// For `verify`: the MX check result.
    pub verify: Option<VerifyReport>,
}

/// Do one action on many leads.
#[utoipa::path(post, path = "/v1/leads/actions", tag = "leads",
    request_body = ActionBody,
    responses((status = 200, body = ActionReply), (status = 404, description = "No such list"),
        (status = 422, description = "Bad selection, or more than 10000 leads")),
    security(("api_key" = ["leads"])),
    description = "Scope: `leads`. Actions: `add_to_list`, `remove_from_list`, `add_tags`, `remove_tags`, `set_interest`, `delete`, `verify`. \
        Up to 10000 leads per call. `add_tags` leaves a lead alone if it would pass 20 tags. `delete` works like `DELETE /v1/leads/{id}`. \
        `verify` checks again, ignoring the 7-day cache, and waits up to 25 seconds for DNS. Its `changed` counts the leads whose status is different now.")]
async fn run_action(State(st): State<AppState>, ApiJson(body): ApiJson<ActionBody>) -> ApiResult<ApiJson<ActionReply>> {
    let now = st.now();
    let mut tx = st.pool.begin().await?;
    let ids: Vec<Uuid> = match (&body.lead_ids, &body.filter) {
        (Some(_), Some(_)) | (None, None) => return Err(ApiError::unprocessable("bad_selection", "Give lead_ids or filter, one of them.")),
        (Some(ids), None) => {
            if ids.is_empty() {
                return Err(ApiError::unprocessable("empty_selection", "lead_ids is empty."));
            }
            if ids.len() as i64 > MAX_ACTION {
                return Err(ApiError::unprocessable("too_many_leads", format!("An action takes up to {MAX_ACTION} leads.")));
            }
            let rows: Vec<(Uuid,)> = sqlx::query_as("SELECT id FROM leads WHERE id = ANY($1)").bind(ids).fetch_all(&mut *tx).await?;
            rows.into_iter().map(|r| r.0).collect()
        }
        (None, Some(f)) => {
            if f.is_empty() {
                return Err(ApiError::unprocessable("empty_selection", "The filter has no condition: it would match every lead."));
            }
            let ids = matching_ids(&mut tx, f, MAX_ACTION).await?;
            if ids.len() as i64 > MAX_ACTION {
                return Err(ApiError::unprocessable("too_many_leads", format!("The filter matches more than {MAX_ACTION} leads. Narrow it.")));
            }
            ids
        }
    };
    let matched = ids.len() as i64;
    let need_list = || body.list_id.ok_or_else(|| ApiError::unprocessable("list_required", "This action needs list_id."));
    let tags = || -> ApiResult<Vec<String>> {
        let t = fields::clean_tags(body.tags.as_deref().unwrap_or(&[])).map_err(|m| ApiError::unprocessable("invalid_tags", m))?;
        if t.is_empty() { Err(ApiError::unprocessable("tags_required", "This action needs tags.")) } else { Ok(t) }
    };
    let mut reply = ActionReply { matched, changed: 0, erased: None, verify: None };
    match body.action {
        LeadAction::AddToList => {
            let list_id = need_list()?;
            require_list(&mut tx, list_id).await?;
            reply.changed = repo::add_to_list(&mut tx, list_id, &ids, now).await?.len() as i64;
        }
        LeadAction::RemoveFromList => {
            let list_id = need_list()?;
            require_list(&mut tx, list_id).await?;
            reply.changed = repo::remove_from_list(&mut tx, list_id, &ids, now).await?.len() as i64;
        }
        LeadAction::AddTags => reply.changed = repo::add_tags(&mut tx, &ids, &tags()?, now).await?.len() as i64,
        LeadAction::RemoveTags => reply.changed = repo::remove_tags(&mut tx, &ids, &tags()?, now).await?.len() as i64,
        LeadAction::SetInterest => reply.changed = repo::set_interest(&mut tx, &ids, body.interest, now).await?.len() as i64,
        LeadAction::Delete => {
            let report = repo::erase(&mut tx, &ids, now).await?;
            reply.changed = report.deleted;
            reply.erased = Some(report);
        }
        LeadAction::Verify => {
            tx.commit().await?;
            let report = verify::verify_leads(&st, &ids, Duration::from_secs(25), now).await?;
            reply.changed = report.leads_changed;
            reply.verify = Some(report);
            return Ok(ApiJson(reply));
        }
    }
    tx.commit().await?;
    Ok(ApiJson(reply))
}

// ---------- context for the AI agent and the Email Generator ----------

/// Everything about a lead in one call.
#[utoipa::path(get, path = "/v1/leads/{id}/context", tag = "leads",
    params(("id" = Uuid, Path)),
    responses((status = 200, body = LeadContext), (status = 404, description = "No such lead")),
    security(("api_key" = ["leads"])),
    description = "Scope: `leads`. For the AI agent and the Email Generator: the lead with its lists, campaigns and flags, its custom fields with labels, \
        the last 50 timeline events, and the sent emails and linked replies as subject and text.")]
async fn lead_context(State(st): State<AppState>, ApiPath(id): ApiPath<Uuid>) -> ApiResult<ApiJson<LeadContext>> {
    let mut conn = st.pool.acquire().await?;
    let ctx = context::build(&mut conn, id, &context::ContextOpts::default()).await?.ok_or_else(lead_not_found)?;
    Ok(ApiJson(ctx))
}
