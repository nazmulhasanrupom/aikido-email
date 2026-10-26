//! Routes for export, import, templates and the LLM guide (build map sections 14 and 15, phase 7).
//!
//! - `GET  /v1/automations/workflows/{id}/export`: the workflow file
//! - `POST /v1/automations/import`: a file, pasted text, a JSON body or a URL becomes a new inactive draft
//! - `POST /v1/automations/import/check`: the same fix-ups and mapping, nothing saved (the editor pastes onto an open draft)
//! - `GET  /v1/automations/templates`, `GET /v1/automations/templates/{id}`, `POST /v1/automations/templates/{id}/create`
//! - `GET  /v1/automations/workprocess.md`
//!
//! The logic is in `transfer.rs`, `templates.rs` and `workprocess.rs`. This file loads the names of the install and saves.
use super::definition::WorkflowDefinition;
use super::routes::{Caller, WorkflowDetail, caller, draft_name, insert_workflow, load_detail, not_found, validate_draft};
use super::transfer::{self, ExportOptions, ImportError, ImportInput, InstallNames, MAX_IMPORT_BYTES, NeedsYou};
use super::validate::{Mode, ValidationResult};
use crate::auth::api_keys::Scope;
use crate::auth::middleware::{KeyScope, require_scope};
use crate::error::{ApiError, ApiJson, ApiPath, ApiQuery, ApiResult};
use crate::net::safe_fetch::{FetchError, RawRequest, RequestOptions};
use crate::state::AppState;
use crate::users::session::SessionCtx;
use axum::Extension;
use axum::extract::State;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sqlx::PgPool;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::LazyLock;
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

pub fn router(state: AppState) -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(export_workflow))
        .routes(routes!(import_workflow))
        .routes(routes!(import_check))
        .routes(routes!(list_workflow_templates))
        .routes(routes!(get_workflow_template))
        .routes(routes!(create_workflow_from_template))
        .routes(routes!(get_workprocess))
        .route_layer(axum::middleware::from_fn_with_state((state, Scope::Automations), require_scope))
}

// ---------- errors ----------

/// `ApiError`, plus `invalid_json` with the line and column.
pub enum TransferError {
    Api(ApiError),
    Import(ImportError),
}

impl From<ApiError> for TransferError {
    fn from(e: ApiError) -> Self {
        TransferError::Api(e)
    }
}

impl From<sqlx::Error> for TransferError {
    fn from(e: sqlx::Error) -> Self {
        TransferError::Api(e.into())
    }
}

impl IntoResponse for TransferError {
    fn into_response(self) -> Response {
        match self {
            TransferError::Api(e) => e.into_response(),
            TransferError::Import(ImportError::InvalidJson { line, column, message }) => (
                StatusCode::UNPROCESSABLE_ENTITY,
                axum::Json(json!({ "error": {
                    "code": "invalid_json",
                    "message": format!("This is not valid JSON (line {line}, column {column}): {message}"),
                    "line": line, "column": column } })),
            )
                .into_response(),
            TransferError::Import(ImportError::FormatTooNew(v)) => ApiError::unprocessable(
                "format_too_new",
                format!("This file is format version {v}. This server reads up to version {}. Update the server.", super::definition::FORMAT_VERSION),
            )
            .into_response(),
            TransferError::Import(ImportError::NotAnObject(m)) => ApiError::unprocessable("invalid_format", m).into_response(),
            TransferError::Import(ImportError::TooLarge) => {
                ApiError::unprocessable("import_too_large", "The file is bigger than 2 MB.").into_response()
            }
        }
    }
}

type TResult<T> = Result<T, TransferError>;

// ---------- names of this install ----------

async fn pairs(pool: &PgPool, sql: &str) -> Result<Vec<(String, String)>, sqlx::Error> {
    sqlx::query_as(sql).fetch_all(pool).await
}

/// The names and ids of this install. Never secret values.
pub async fn load_names(pool: &PgPool) -> Result<InstallNames, sqlx::Error> {
    Ok(InstallNames {
        campaigns: pairs(pool, "SELECT id::text, name FROM campaigns ORDER BY created_at DESC LIMIT 2000").await?,
        lists: pairs(pool, "SELECT id::text, name FROM lead_lists ORDER BY name LIMIT 2000").await?,
        sequences: pairs(pool, "SELECT id::text, name FROM sequences ORDER BY created_at DESC LIMIT 2000").await?,
        accounts: pairs(pool, "SELECT id::text, email::text FROM email_accounts ORDER BY created_at DESC LIMIT 2000").await?,
        providers: pairs(pool, "SELECT id::text, name FROM llm_providers ORDER BY name").await?,
        custom_fields: sqlx::query_scalar("SELECT key FROM lead_fields ORDER BY key").fetch_all(pool).await?,
        secrets: sqlx::query_scalar("SELECT name FROM automation_secrets ORDER BY name").fetch_all(pool).await?,
        apps: sqlx::query_scalar("SELECT app FROM integrations WHERE status = 'installed'").fetch_all(pool).await?,
    })
}

// ---------- export ----------

#[derive(Deserialize, IntoParams)]
pub struct ExportQuery {
    /// A published version number. Default: the draft.
    version: Option<i32>,
    /// `true`: keep the pins (saved test data; it can hold lead data). Default `false`.
    include_pins: Option<bool>,
}

/// Download a workflow as a file (`<name>.aikido-workflow.json`).
#[utoipa::path(get, path = "/v1/automations/workflows/{id}/export", tag = "automations", params(("id" = Uuid, Path), ExportQuery),
    responses((status = 200, description = "The workflow file (JSON, pretty printed, keys in a fixed order). `Content-Disposition` has the file name.", body = Object, content_type = "application/json"),
        (status = 404, description = "workflow_not_found, version_not_found"),
        (status = 422, description = "invalid_format: the draft is too broken to write as a file")),
    security(("api_key" = ["automations"]), ("session" = [])),
    description = "Scope: `automations`. The AI provider ids become names, the values of campaign/list/account/provider variables become `null` (their names are written in `meta.requires.variables[].hint`), `pins` are left out unless `include_pins=true`, and `meta` gets `exported_at`, `app_version`, `exported_from_version` and `requires` (`node_types`, `secrets`, `providers`, `variables`, `apps`). Secret values, API keys, user ids and run data are never in the file.")]
async fn export_workflow(State(st): State<AppState>, ApiPath(id): ApiPath<Uuid>, ApiQuery(q): ApiQuery<ExportQuery>) -> ApiResult<Response> {
    let raw: Value = match q.version {
        None => sqlx::query_scalar("SELECT draft FROM automation_workflows WHERE id = $1").bind(id).fetch_optional(&st.pool).await?.ok_or_else(not_found)?,
        Some(v) => {
            let exists: Option<Uuid> = sqlx::query_scalar("SELECT id FROM automation_workflows WHERE id = $1").bind(id).fetch_optional(&st.pool).await?;
            exists.ok_or_else(not_found)?;
            sqlx::query_scalar("SELECT definition FROM automation_versions WHERE workflow_id = $1 AND version = $2")
                .bind(id)
                .bind(v)
                .fetch_optional(&st.pool)
                .await?
                .ok_or_else(|| ApiError::not_found("version_not_found", "This workflow has no such version."))?
        }
    };
    let def: WorkflowDefinition = serde_json::from_value(raw)
        .map_err(|e| ApiError::unprocessable("invalid_format", format!("The workflow is too broken to export ({e}). Fix it in the editor first.")))?;
    let names = load_names(&st.pool).await?;
    let out = transfer::export_definition(&def, &ExportOptions { include_pins: q.include_pins.unwrap_or(false), exported_at: chrono::Utc::now(), from_version: q.version, names: &names });
    let filename = transfer::export_filename(&out.name);
    let mut res = (StatusCode::OK, transfer::export_text(&out)).into_response();
    res.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static("application/json"));
    if let Ok(v) = HeaderValue::from_str(&format!("attachment; filename=\"{filename}\"")) {
        res.headers_mut().insert(header::CONTENT_DISPOSITION, v);
    }
    Ok(res)
}

// ---------- import ----------

#[derive(Deserialize, ToSchema)]
pub struct ImportBody {
    /// The text of a file or pasted text. Not valid JSON: `422 invalid_json` with `line` and `column`. A markdown code fence around the JSON is removed.
    text: Option<String>,
    /// The workflow as a JSON object (or `{ "workflow": ... }`).
    #[schema(value_type = Option<Object>)]
    definition: Option<Value>,
    /// A web address of a file (`http` or `https`, at most 2 MB, public hosts only). A GitHub `blob` address is turned into the raw address.
    url: Option<String>,
    /// Use this name instead of the one in the file.
    name: Option<String>,
    /// The person's choices: variable key to the id of a campaign, list, sequence, account or provider (a key for a custom field).
    #[schema(value_type = Option<Object>)]
    variables: Option<BTreeMap<String, Value>>,
}

#[derive(Serialize, ToSchema)]
pub struct ImportReport {
    /// What the importer fixed, in words.
    pub fixed: Vec<String>,
    /// Things the person must do: pick a campaign, add a secret, install an app, pick a provider.
    pub needs_you: Vec<NeedsYou>,
    /// Things the importer could only guess (an n8n file).
    pub warnings: Vec<String>,
    /// The check of the result, same shape as `POST /v1/automations/validate`.
    pub validation: ValidationResult,
    /// `exported_at`, `app_version` and `exported_from_version` of the file, if it had them.
    #[schema(value_type = Option<Object>)]
    pub source: Option<Value>,
}

#[derive(Serialize, ToSchema)]
pub struct ImportReply {
    pub workflow: WorkflowDetail,
    pub report: ImportReport,
}

#[derive(Serialize, ToSchema)]
pub struct ImportCheckReply {
    /// The fixed and mapped definition. Nothing is saved.
    #[schema(value_type = Object)]
    pub definition: Value,
    pub report: ImportReport,
}

/// Turn a GitHub `blob` page address into the raw file address.
fn raw_url(url: &str) -> String {
    let u = url.trim();
    if let Some(rest) = u.strip_prefix("https://github.com/") {
        let parts: Vec<&str> = rest.splitn(4, '/').collect();
        if parts.len() == 4 && parts[2] == "blob" {
            return format!("https://raw.githubusercontent.com/{}/{}/{}", parts[0], parts[1], parts[3]);
        }
    }
    u.to_string()
}

async fn fetch_text(st: &AppState, url: &str) -> TResult<String> {
    let opts = RequestOptions {
        max_bytes: MAX_IMPORT_BYTES,
        user_agent: format!("AikidoEmail-Import/{}", env!("CARGO_PKG_VERSION")),
        ..RequestOptions::default()
    };
    match st.fetcher.request(RawRequest::get(&raw_url(url)), &opts).await {
        Ok(r) if (200..300).contains(&r.status) => Ok(String::from_utf8_lossy(&r.body).into_owned()),
        Ok(r) => Err(ApiError::unprocessable("import_fetch_failed", format!("The address answered with status {}.", r.status)).into()),
        Err(FetchError::TooLarge) => Err(TransferError::Import(ImportError::TooLarge)),
        Err(e) => Err(ApiError::unprocessable("import_fetch_failed", e.to_string()).into()),
    }
}

fn check_name(name: &str) -> ApiResult<String> {
    let n = name.trim();
    if n.is_empty() || n.chars().count() > 120 {
        return Err(ApiError::unprocessable("invalid_name", "The name must be 1 to 120 characters."));
    }
    Ok(n.to_string())
}

/// Fix, map and validate a file. Nothing is saved.
async fn prepare_import(st: &AppState, c: &Caller, b: &ImportBody) -> TResult<(Value, ImportReport)> {
    let given = [b.text.is_some(), b.definition.is_some(), b.url.is_some()].iter().filter(|x| **x).count();
    if given != 1 {
        return Err(ApiError::unprocessable("import_source", "Send exactly one of `text`, `definition` or `url`.").into());
    }
    let override_name = b.name.as_deref().map(check_name).transpose()?;
    let names = load_names(&st.pool).await?;
    let empty = BTreeMap::new();
    let input = ImportInput { names: &names, variables: b.variables.as_ref().unwrap_or(&empty) };
    let prepared = if let Some(t) = &b.text {
        transfer::prepare_text(t, &input)
    } else if let Some(d) = &b.definition {
        if d.to_string().len() > MAX_IMPORT_BYTES {
            return Err(TransferError::Import(ImportError::TooLarge));
        }
        transfer::prepare(d.clone(), vec![], &input)
    } else {
        let text = fetch_text(st, b.url.as_deref().unwrap_or("")).await?;
        transfer::prepare_text(&text, &input)
    }
    .map_err(TransferError::Import)?;
    finish(st, c, prepared, override_name).await
}

async fn finish(st: &AppState, c: &Caller, prepared: transfer::Prepared, override_name: Option<String>) -> TResult<(Value, ImportReport)> {
    let mut draft = prepared.draft;
    if let Some(n) = override_name {
        draft["name"] = json!(n);
    }
    let validation = validate_draft(st, &draft, Mode::Draft, c.is_admin).await?;
    let report = ImportReport { fixed: prepared.fixed, needs_you: prepared.needs_you, warnings: prepared.warnings, validation, source: prepared.source };
    Ok((draft, report))
}

async fn save_imported(st: &AppState, c: &Caller, draft: Value, report: ImportReport) -> TResult<(StatusCode, ApiJson<ImportReply>)> {
    let name = draft_name(&draft, "Imported workflow");
    let id = insert_workflow(st, &name, draft, &[], "import", c).await?;
    let workflow = load_detail(st, id, c.is_admin).await?;
    Ok((StatusCode::CREATED, ApiJson(ImportReply { workflow, report })))
}

/// Import a workflow file as a new draft.
#[utoipa::path(post, path = "/v1/automations/import", tag = "automations", request_body = ImportBody,
    responses((status = 201, body = ImportReply),
        (status = 422, description = "invalid_json (with `line` and `column`), format_too_new, invalid_format, import_source (not exactly one of text/definition/url), import_too_large, import_fetch_failed, invalid_name")),
    security(("api_key" = ["automations"]), ("session" = [])),
    description = "Scope: `automations`. Reads the file, fixes what is safe to fix (each fix is listed in `report.fixed`), maps install-specific parts (variables by your `variables` choice, then by the name in the file; AI providers by name, else `default_draft`; missing secrets and apps are listed in `report.needs_you`), checks the result and saves a **new inactive draft**, even when it has errors (draft history source `import`). It is never published or turned on by an import.")]
async fn import_workflow(
    State(st): State<AppState>,
    session: Option<Extension<SessionCtx>>,
    key: Option<Extension<KeyScope>>,
    ApiJson(b): ApiJson<ImportBody>,
) -> TResult<(StatusCode, ApiJson<ImportReply>)> {
    let c = caller(&session, &key);
    let (draft, report) = prepare_import(&st, &c, &b).await?;
    save_imported(&st, &c, draft, report).await
}

/// Read, fix and map a workflow file without saving it.
#[utoipa::path(post, path = "/v1/automations/import/check", tag = "automations", request_body = ImportBody,
    responses((status = 200, body = ImportCheckReply),
        (status = 422, description = "The same errors as import.")),
    security(("api_key" = ["automations"]), ("session" = [])),
    description = "Scope: `automations`. Does everything `POST /v1/automations/import` does except saving. The editor uses it to paste a workflow onto an open draft: it shows the report and lets the person choose Replace, Add nodes or Cancel, then saves with `PATCH /v1/automations/workflows/{id}`.")]
async fn import_check(
    State(st): State<AppState>,
    session: Option<Extension<SessionCtx>>,
    key: Option<Extension<KeyScope>>,
    ApiJson(b): ApiJson<ImportBody>,
) -> TResult<ApiJson<ImportCheckReply>> {
    let c = caller(&session, &key);
    let (definition, report) = prepare_import(&st, &c, &b).await?;
    Ok(ApiJson(ImportCheckReply { definition, report }))
}

// ---------- templates ----------

#[derive(Serialize, ToSchema)]
pub struct TemplateVariable {
    pub key: String,
    pub label: String,
    pub kind: String,
    pub required: bool,
}

#[derive(Serialize, ToSchema)]
pub struct TemplateInfo {
    /// Use it in `/v1/automations/templates/{id}`.
    pub id: String,
    pub name: String,
    pub description: String,
    pub node_count: usize,
    pub node_types: Vec<String>,
    /// The variables the person must pick after creating the workflow.
    pub variables: Vec<TemplateVariable>,
    /// Secret names the template uses.
    pub secrets: Vec<String>,
}

#[derive(Serialize, ToSchema)]
pub struct TemplateList {
    pub templates: Vec<TemplateInfo>,
}

fn template_info(id: &str, def: &Value) -> TemplateInfo {
    let nodes: Vec<&Value> = def["nodes"].as_array().map(|a| a.iter().collect()).unwrap_or_default();
    let types: BTreeSet<String> = nodes.iter().filter_map(|n| n["type"].as_str()).map(str::to_string).collect();
    let params: Vec<&Map<String, Value>> = nodes.iter().filter_map(|n| n["params"].as_object()).collect();
    TemplateInfo {
        id: id.into(),
        name: def["name"].as_str().unwrap_or(id).into(),
        description: def["description"].as_str().unwrap_or("").into(),
        node_count: nodes.len(),
        node_types: types.into_iter().collect(),
        variables: def["variables"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|v| TemplateVariable {
                key: v["key"].as_str().unwrap_or("").into(),
                label: v["label"].as_str().unwrap_or("").into(),
                kind: v["kind"].as_str().unwrap_or("").into(),
                required: v["required"].as_bool().unwrap_or(false),
            })
            .collect(),
        secrets: transfer::secret_names(params).into_iter().collect(),
    }
}

/// The built-in templates.
#[utoipa::path(get, path = "/v1/automations/templates", tag = "automations",
    responses((status = 200, body = TemplateList)),
    security(("api_key" = ["automations"]), ("session" = [])),
    description = "Scope: `automations`. Four templates are built in: research a lead and write the first email, tag hot replies, website form to lead, daily reply digest to Slack.")]
async fn list_workflow_templates() -> ApiJson<TemplateList> {
    ApiJson(TemplateList { templates: super::templates::all().map(|(id, def)| template_info(id, def)).collect() })
}

/// One template, as a workflow file.
#[utoipa::path(get, path = "/v1/automations/templates/{id}", tag = "automations", params(("id" = String, Path)),
    responses((status = 200, description = "The workflow definition of the template.", body = Object), (status = 404, description = "template_not_found")),
    security(("api_key" = ["automations"]), ("session" = [])),
    description = "Scope: `automations`. The file as shipped (not mapped to this install).")]
async fn get_workflow_template(ApiPath(id): ApiPath<String>) -> ApiResult<axum::Json<Value>> {
    super::templates::get(&id).cloned().map(axum::Json).ok_or_else(template_not_found)
}

fn template_not_found() -> ApiError {
    ApiError::not_found("template_not_found", "There is no template with this id.")
}

#[derive(Deserialize, ToSchema, Default)]
pub struct CreateFromTemplate {
    /// Use this name instead of the template's.
    name: Option<String>,
    /// Choices for the template's variables (see `ImportBody.variables`).
    #[schema(value_type = Option<Object>)]
    variables: Option<BTreeMap<String, Value>>,
}

/// Make a new draft from a template.
#[utoipa::path(post, path = "/v1/automations/templates/{id}/create", tag = "automations", params(("id" = String, Path)), request_body(content = CreateFromTemplate, description = "Optional; an empty body is fine."),
    responses((status = 201, body = ImportReply), (status = 404, description = "template_not_found"), (status = 422, description = "invalid_name")),
    security(("api_key" = ["automations"]), ("session" = [])),
    description = "Scope: `automations`. Goes through the same code as an import: the same mapping and the same `report` (variables to pick, secrets to add). The draft is not published and not active.")]
async fn create_workflow_from_template(
    State(st): State<AppState>,
    session: Option<Extension<SessionCtx>>,
    key: Option<Extension<KeyScope>>,
    ApiPath(id): ApiPath<String>,
    body: axum::body::Bytes,
) -> TResult<(StatusCode, ApiJson<ImportReply>)> {
    let c = caller(&session, &key);
    let b: CreateFromTemplate = crate::error::parse_optional_json(&body)?;
    let def = super::templates::get(&id).ok_or_else(template_not_found)?.clone();
    let override_name = b.name.as_deref().map(check_name).transpose()?;
    let names = load_names(&st.pool).await?;
    let empty = BTreeMap::new();
    let input = ImportInput { names: &names, variables: b.variables.as_ref().unwrap_or(&empty) };
    let prepared = transfer::prepare(def, vec![], &input).map_err(TransferError::Import)?;
    let (draft, report) = finish(&st, &c, prepared, override_name).await?;
    save_imported(&st, &c, draft, report).await
}

// ---------- workprocess.md ----------

#[derive(Deserialize, IntoParams)]
pub struct WorkprocessQuery {
    /// `true`: add the "This install" part: names and ids of campaigns and lists, provider names, custom field keys and secret names (never values). Default `false`.
    install: Option<bool>,
}

static PLAIN: LazyLock<String> = LazyLock::new(|| super::workprocess::workprocess(None));

/// The guide that teaches an LLM to write workflows, with the live node catalog.
#[utoipa::path(get, path = "/v1/automations/workprocess.md", tag = "automations", params(WorkprocessQuery),
    responses((status = 200, description = "Markdown", body = String, content_type = "text/markdown")),
    security(("api_key" = ["automations"]), ("session" = [])),
    description = "Scope: `automations`. `docs/workprocess.md` with the `<!-- NODE_CATALOG -->` line replaced by a catalog made from the node specs (every node type, its params, ports and an output example). Give it to any LLM together with a request, and the answer is a workflow file that `POST /v1/automations/import` accepts.")]
async fn get_workprocess(State(st): State<AppState>, ApiQuery(q): ApiQuery<WorkprocessQuery>) -> ApiResult<Response> {
    let text = if q.install.unwrap_or(false) {
        let names = load_names(&st.pool).await?;
        super::workprocess::workprocess(Some(&names))
    } else {
        PLAIN.clone()
    };
    let mut res = (StatusCode::OK, text).into_response();
    res.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static("text/markdown; charset=utf-8"));
    Ok(res)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn github_blob_addresses_become_raw() {
        assert_eq!(raw_url("https://github.com/acme/flows/blob/main/a/b.json"), "https://raw.githubusercontent.com/acme/flows/main/a/b.json");
        assert_eq!(raw_url(" https://example.com/x.json "), "https://example.com/x.json");
        assert_eq!(raw_url("https://github.com/acme/flows"), "https://github.com/acme/flows");
    }

    #[test]
    fn template_summaries() {
        let (id, def) = super::super::templates::all().find(|(i, _)| *i == "website-form-to-lead").unwrap();
        let info = template_info(id, def);
        assert_eq!(info.node_count, 2);
        assert!(info.variables.iter().any(|v| v.key == "list_id" && v.required));
        assert_eq!(info.secrets, vec!["FORM_SECRET".to_string()]);
    }
}
