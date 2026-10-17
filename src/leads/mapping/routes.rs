//! Routes of the import mapping: propose, read, confirm, and the saved mappings.
use super::proposal::*;
use super::samples::SampleSet;
use super::{propose, templates};
use crate::error::{ApiError, ApiJson, ApiPath, ApiQuery, ApiResult};
use crate::leads::fields::{self, FieldDefs, MAX_FIELDS};
use crate::leads::import::{Import, csv_reader, fetch, headers_of};
use crate::leads::model::{DedupeMode, FieldKind, ImportStatus};
use crate::leads::routes::load_settings;
use crate::state::AppState;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::collections::HashMap;
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(propose_again))
        .routes(routes!(get_mapping))
        .routes(routes!(confirm))
        .routes(routes!(list_templates))
        .routes(routes!(delete_template))
}

/// The stored file of an import that is still `uploaded`.
async fn uploaded_file(st: &AppState, id: Uuid) -> ApiResult<Vec<u8>> {
    let row: Option<(ImportStatus,)> = sqlx::query_as("SELECT status FROM lead_imports WHERE id = $1").bind(id).fetch_optional(&st.pool).await?;
    match row {
        None => return Err(ApiError::not_found("import_not_found", "No such import.")),
        Some((s,)) if s != ImportStatus::Uploaded => return Err(ApiError::conflict("import_not_uploaded", "The import is not waiting for a mapping.")),
        _ => {}
    }
    let data: Option<(Vec<u8>,)> = sqlx::query_as("SELECT data FROM lead_import_files WHERE import_id = $1").bind(id).fetch_optional(&st.pool).await?;
    data.map(|d| d.0).ok_or_else(|| ApiError::conflict("file_gone", "The uploaded file is gone."))
}

/// Run the rules and the AI again.
#[utoipa::path(post, path = "/v1/leads/imports/{id}/mapping/propose", tag = "leads",
    params(("id" = Uuid, Path)),
    responses((status = 200, body = MappingProposal), (status = 404, description = "No such import"), (status = 409, description = "The import is not waiting for a mapping")),
    security(("api_key" = ["leads"])),
    description = "Scope: `leads`. Makes a new proposal and saves it. It does not use a saved mapping, so the AI runs again for the columns the rules cannot map (one more model call).")]
async fn propose_again(State(st): State<AppState>, ApiPath(id): ApiPath<Uuid>) -> ApiResult<ApiJson<MappingProposal>> {
    let data = uploaded_file(&st, id).await?;
    Ok(ApiJson(propose(&st, id, &data, false).await?))
}

#[derive(Deserialize, IntoParams)]
pub struct MappingQuery {
    /// `true`: show the real sample values (they never left the server).
    raw_samples: Option<bool>,
}

/// The last proposal.
#[utoipa::path(get, path = "/v1/leads/imports/{id}/mapping", tag = "leads",
    params(("id" = Uuid, Path), MappingQuery),
    responses((status = 200, body = MappingProposal), (status = 404, description = "No such import, or no proposal yet")),
    security(("api_key" = ["leads"])),
    description = "Scope: `leads`. The samples are masked as the model saw them (setting `mapping_mask_pii`). `raw_samples=true` shows the real values while the file is still stored.")]
async fn get_mapping(State(st): State<AppState>, ApiPath(id): ApiPath<Uuid>, ApiQuery(q): ApiQuery<MappingQuery>) -> ApiResult<ApiJson<MappingProposal>> {
    let row: Option<(Option<Value>,)> = sqlx::query_as("SELECT proposal FROM lead_imports WHERE id = $1").bind(id).fetch_optional(&st.pool).await?;
    let Some((p,)) = row else { return Err(ApiError::not_found("import_not_found", "No such import.")) };
    let mut p: MappingProposal = p.and_then(|v| serde_json::from_value(v).ok()).ok_or_else(|| ApiError::not_found("no_proposal", "This import has no mapping proposal."))?;
    if q.raw_samples == Some(true) {
        let data: Option<(Vec<u8>,)> = sqlx::query_as("SELECT data FROM lead_import_files WHERE import_id = $1").bind(id).fetch_optional(&st.pool).await?;
        let n = load_settings(&st.pool).await?.mapping_sample_rows as usize;
        if let Some((data,)) = data {
            if let Ok(set) = SampleSet::from_csv(&data, n) {
                for c in p.columns.iter_mut() {
                    if let Some(s) = set.columns.get(c.index) {
                        c.samples = s.samples.clone();
                        c.empty_count = s.empty_count;
                    }
                }
            }
        }
    }
    Ok(ApiJson(p))
}

#[derive(Deserialize, ToSchema)]
pub struct ConfirmColumn {
    pub index: usize,
    /// Optional check: must be the header of that column.
    #[serde(default)]
    pub header: Option<String>,
    pub target: MapTarget,
    /// `split_full_name` (only on a column mapped to `first_name`).
    #[serde(default)]
    pub transform: Option<String>,
}

fn yes() -> bool {
    true
}

#[derive(Deserialize, ToSchema)]
pub struct ConfirmRequest {
    /// Columns to change. A column left out keeps the target of the proposal.
    #[serde(default)]
    pub columns: Vec<ConfirmColumn>,
    /// `skip`, `fill_empty` or `overwrite`. Default: the one of the upload.
    pub dedupe_mode: Option<DedupeMode>,
    pub list_id: Option<Uuid>,
    /// Replaces the tags of the upload.
    pub tags: Option<Vec<String>>,
    pub campaign_id: Option<Uuid>,
    /// Save the mapping for the same headers. Default `true`.
    #[serde(default = "yes")]
    pub remember: bool,
}

struct Final {
    index: usize,
    target: MapTarget,
    transform: Option<String>,
}

fn problems_response(problems: Vec<String>) -> Response {
    let body = json!({ "error": { "code": "mapping_invalid", "message": format!("The mapping has {} problem(s): {}", problems.len(), problems.join(" ")), "problems": problems } });
    (StatusCode::UNPROCESSABLE_ENTITY, axum::Json(body)).into_response()
}

/// The hard checks of section 12. Returns the problems.
fn check(cols: &[Final], headers: &[String], defs: &FieldDefs) -> Vec<String> {
    let mut problems = Vec::new();
    let emails = cols.iter().filter(|c| c.target.is_email()).count();
    if emails != 1 {
        problems.push(format!("Exactly one column must go to email (found {emails})."));
    }
    let mut owner: HashMap<String, usize> = HashMap::new();
    let mut seen_headers: HashMap<&str, usize> = HashMap::new();
    for c in cols {
        let h = headers[c.index].as_str();
        let first = *seen_headers.entry(h).or_insert(c.index);
        if first != c.index && c.target != MapTarget::Skip {
            problems.push(format!("Column {} has the same name as column {first}. Rename one of them in the file.", c.index));
        }
        if let Some(t) = &c.transform {
            if t != SPLIT_FULL_NAME {
                problems.push(format!("Column {}: unknown transform '{t}'.", c.index));
            } else if !matches!(&c.target, MapTarget::Builtin { key } if key == "first_name") {
                problems.push(format!("Column {}: split_full_name works only on a column mapped to first_name.", c.index));
            }
        }
        match &c.target {
            MapTarget::Builtin { key } if !c.target.is_valid_builtin() => problems.push(format!("Column {}: '{key}' is not a lead column.", c.index)),
            MapTarget::Custom { key } if !defs.by_key.contains_key(key) => problems.push(format!("Column {}: the custom field '{key}' does not exist.", c.index)),
            MapTarget::NewField { key, field_kind, .. } => {
                if let Some(f) = defs.by_key.get(key) {
                    if f.kind != *field_kind {
                        problems.push(format!("Column {}: the field '{key}' exists with the kind {}.", c.index, f.kind.as_str()));
                    }
                } else if let Err(m) = fields::check_key(key) {
                    problems.push(format!("Column {}: {m}", c.index));
                }
            }
            _ => {}
        }
        for slot in c.target.slots(c.transform.as_deref()) {
            if let Some(other) = owner.insert(slot.clone(), c.index) {
                problems.push(format!("Columns {other} and {} both go to '{}'.", c.index, slot.trim_start_matches("custom:")));
            }
        }
    }
    problems
}

/// The soft checks: samples that do not fit the kind, and columns that are mostly empty.
fn soft_warnings(cols: &[Final], set: &SampleSet, defs: &FieldDefs) -> Vec<String> {
    let mut out = Vec::new();
    for c in cols {
        let Some(s) = set.columns.get(c.index) else { continue };
        let kind = match &c.target {
            MapTarget::Custom { key } => defs.by_key.get(key).map(|f| f.kind),
            MapTarget::NewField { key, field_kind, .. } => Some(defs.by_key.get(key).map(|f| f.kind).unwrap_or(*field_kind)),
            _ => None,
        };
        if kind.is_some_and(|k| s.samples.iter().any(|v| fields::check_value(k, v).is_err())) {
            out.push(format!("samples_do_not_fit:{}", c.index));
        }
        if c.target != MapTarget::Skip && set.rows_scanned > 0 && s.empty_count * 2 > set.rows_scanned {
            out.push(format!("mostly_empty:{}", c.index));
        }
    }
    out
}

/// Check the final mapping and start the import.
#[utoipa::path(post, path = "/v1/leads/imports/{id}/mapping/confirm", tag = "leads",
    params(("id" = Uuid, Path)), request_body = ConfirmRequest,
    responses((status = 200, body = Import, description = "The import, now `pending`. The body also holds `warnings`."),
        (status = 404, description = "No such import, list or campaign"), (status = 409, description = "The import is not waiting for a mapping (also: a second confirm)"),
        (status = 422, description = "`mapping_invalid` with a `problems` list. Nothing is imported or made.")),
    security(("api_key" = ["leads"])),
    description = "Scope: `leads`. Checks: exactly one column on `email`; no two columns on one target (except `tags`); every `custom` key exists; every `new_field` key is valid and new (or exists with the same kind); \
        `split_full_name` only on `first_name`; the indexes match the file. Then, in one transaction: the new fields are made, the mapping is saved, the template is saved (if `remember`) and the import is queued. \
        Soft warnings (`samples_do_not_fit:<i>`, `mostly_empty:<i>`) do not stop it. Two confirms at the same moment start the import once.")]
async fn confirm(State(st): State<AppState>, ApiPath(id): ApiPath<Uuid>, ApiJson(body): ApiJson<ConfirmRequest>) -> ApiResult<Response> {
    let mut tx = st.pool.begin().await?;
    #[allow(clippy::type_complexity)]
    let row: Option<(ImportStatus, Option<Value>, DedupeMode, Option<Uuid>, Option<Uuid>, Vec<String>)> =
        sqlx::query_as("SELECT status, proposal, dedupe_mode, list_id, campaign_id, tags FROM lead_imports WHERE id = $1 FOR UPDATE").bind(id).fetch_optional(&mut *tx).await?;
    let Some((status, proposal, mode, list_id, campaign_id, tags)) = row else { return Err(ApiError::not_found("import_not_found", "No such import.")) };
    if status != ImportStatus::Uploaded {
        return Err(ApiError::conflict("import_not_uploaded", "The import is not waiting for a mapping. It was confirmed, cancelled or never uploaded for mapping."));
    }
    let proposal: MappingProposal = proposal.and_then(|v| serde_json::from_value(v).ok()).ok_or_else(|| ApiError::conflict("no_proposal", "This import has no mapping proposal."))?;
    let data: Option<(Vec<u8>,)> = sqlx::query_as("SELECT data FROM lead_import_files WHERE import_id = $1").bind(id).fetch_optional(&mut *tx).await?;
    let Some((data,)) = data else { return Err(ApiError::conflict("file_gone", "The uploaded file is gone.")) };
    let headers = headers_of(&mut csv_reader(&data)).map_err(|m| ApiError::unprocessable("invalid_csv", m))?;

    // the final mapping: the proposal, changed by the caller
    let mut problems = Vec::new();
    let mut cols: Vec<Final> = proposal.columns.iter().filter(|c| c.index < headers.len()).map(|c| Final { index: c.index, target: c.target.clone(), transform: c.transform.clone() }).collect();
    for c in body.columns {
        match (c.index < headers.len(), c.header.as_deref()) {
            (false, _) => problems.push(format!("Column {} is not in the file (it has {} columns).", c.index, headers.len())),
            (true, Some(h)) if h != headers[c.index] => problems.push(format!("Column {} is named '{}' in the file, not '{h}'.", c.index, headers[c.index])),
            _ => match cols.iter_mut().find(|f| f.index == c.index) {
                Some(f) => {
                    f.target = c.target;
                    f.transform = c.transform;
                }
                None => cols.push(Final { index: c.index, target: c.target, transform: c.transform }),
            },
        }
    }
    cols.sort_by_key(|c| c.index);
    let defs = FieldDefs::load(&mut tx).await?;
    problems.extend(check(&cols, &headers, &defs));
    if !problems.is_empty() {
        return Ok(problems_response(problems));
    }

    let mode = body.dedupe_mode.unwrap_or(mode);
    let list_id = body.list_id.or(list_id);
    let campaign_id = body.campaign_id.or(campaign_id);
    let tags = match &body.tags {
        Some(t) => fields::clean_tags(t).map_err(|m| ApiError::unprocessable("invalid_tags", m))?,
        None => tags,
    };
    if let Some(list_id) = list_id {
        let found: Option<(Uuid,)> = sqlx::query_as("SELECT id FROM lead_lists WHERE id = $1").bind(list_id).fetch_optional(&mut *tx).await?;
        if found.is_none() {
            return Err(ApiError::not_found("list_not_found", "No such list."));
        }
    }
    if let Some(campaign_id) = campaign_id {
        crate::leads::routes::require_campaign(&mut tx, campaign_id).await?;
    }

    // the new fields
    let mut seen = Vec::new();
    for c in &cols {
        if let MapTarget::NewField { key, label, field_kind } = &c.target {
            if !defs.by_key.contains_key(key) {
                seen.push((key.clone(), label.clone().filter(|l| !l.trim().is_empty()).unwrap_or_else(|| key.clone()), *field_kind));
            }
        }
    }
    if !seen.is_empty() {
        sqlx::query("SELECT pg_advisory_xact_lock(hashtext('lead_fields'))").execute(&mut *tx).await?;
        let (n,): (i64,) = sqlx::query_as("SELECT count(*) FROM lead_fields").fetch_one(&mut *tx).await?;
        if n + seen.len() as i64 > MAX_FIELDS {
            return Err(ApiError::unprocessable("too_many_fields", format!("There can be up to {MAX_FIELDS} custom fields.")));
        }
        for (key, label, kind) in &seen {
            sqlx::query("INSERT INTO lead_fields (key, label, kind, created_at) VALUES ($1, $2, $3, $4) ON CONFLICT (key) DO NOTHING")
                .bind(key)
                .bind(label.chars().take(100).collect::<String>())
                .bind::<FieldKind>(*kind)
                .bind(st.now())
                .execute(&mut *tx)
                .await?;
        }
    }

    // the mapping the worker reads: column name -> target
    let mut mapping = Map::new();
    for c in &cols {
        if c.target != MapTarget::Skip {
            mapping.insert(headers[c.index].clone(), Value::String(c.target.worker_string(c.transform.as_deref())));
        }
    }
    sqlx::query("UPDATE lead_imports SET mapping = $2, status = 'pending', dedupe_mode = $3, list_id = $4, campaign_id = $5, tags = $6 WHERE id = $1 AND status = 'uploaded'")
        .bind(id)
        .bind(Value::Object(mapping))
        .bind(mode)
        .bind(list_id)
        .bind(campaign_id)
        .bind(&tags)
        .execute(&mut *tx)
        .await?;
    if body.remember && load_settings(&mut *tx).await?.mapping_templates_enabled {
        let saved: Vec<TemplateColumn> = cols.iter().map(|c| TemplateColumn { index: c.index, target: c.target.clone(), transform: c.transform.clone() }).collect();
        templates::save(&mut tx, &headers, &saved, st.now()).await?;
    }
    tx.commit().await?;

    // the soft checks look at the real samples (the file is still stored until the worker ends)
    let n = load_settings(&st.pool).await?.mapping_sample_rows as usize;
    let warnings = SampleSet::from_csv(&data, n.max(5)).map(|s| soft_warnings(&cols, &s, &defs)).unwrap_or_default();
    let mut out = serde_json::to_value(fetch(&st, id).await?).unwrap_or_default();
    out["warnings"] = json!(warnings);
    Ok(axum::Json(out).into_response())
}

#[derive(Serialize, ToSchema)]
pub struct TemplateView {
    pub id: Uuid,
    pub headers: Vec<String>,
    pub mapping: Vec<TemplateColumn>,
    pub used_count: i32,
    pub last_used_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

#[derive(Serialize, ToSchema)]
pub struct TemplatesReply {
    pub templates: Vec<TemplateView>,
}

/// The saved mappings, newest first.
#[utoipa::path(get, path = "/v1/leads/mapping-templates", tag = "leads",
    responses((status = 200, body = TemplatesReply)),
    security(("api_key" = ["leads"])),
    description = "Scope: `leads`. A saved mapping is used when a new upload has the same headers in the same order. No AI call is made then.")]
async fn list_templates(State(st): State<AppState>) -> ApiResult<ApiJson<TemplatesReply>> {
    let rows: Vec<(Uuid, Value, Value, i32, Option<DateTime<Utc>>, DateTime<Utc>)> =
        sqlx::query_as("SELECT id, headers, mapping, used_count, last_used_at, created_at FROM import_mapping_templates ORDER BY created_at DESC, id DESC LIMIT 500").fetch_all(&st.pool).await?;
    let templates = rows
        .into_iter()
        .map(|(id, h, m, used_count, last_used_at, created_at)| TemplateView {
            id,
            headers: serde_json::from_value(h).unwrap_or_default(),
            mapping: serde_json::from_value(m).unwrap_or_default(),
            used_count,
            last_used_at,
            created_at,
        })
        .collect();
    Ok(ApiJson(TemplatesReply { templates }))
}

#[derive(Serialize, ToSchema)]
pub struct DeletedTemplate {
    pub deleted: bool,
}

/// Delete one saved mapping.
#[utoipa::path(delete, path = "/v1/leads/mapping-templates/{id}", tag = "leads",
    params(("id" = Uuid, Path)),
    responses((status = 200, body = DeletedTemplate), (status = 404, description = "No such template")),
    security(("api_key" = ["leads"])), description = "Scope: `leads`.")]
async fn delete_template(State(st): State<AppState>, ApiPath(id): ApiPath<Uuid>) -> ApiResult<ApiJson<DeletedTemplate>> {
    if sqlx::query("DELETE FROM import_mapping_templates WHERE id = $1").bind(id).execute(&st.pool).await?.rows_affected() == 0 {
        return Err(ApiError::not_found("template_not_found", "No such template."));
    }
    Ok(ApiJson(DeletedTemplate { deleted: true }))
}
