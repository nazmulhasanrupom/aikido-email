//! CSV import. The route stores the file in `lead_import_files` (no local disk, so many server copies
//! work). A worker claims the import (`FOR UPDATE SKIP LOCKED` plus a lease), streams the rows and writes
//! them in batches of 1000 with `upsert_batch`. The counters, the row position and the lease heartbeat
//! are saved in the same transaction as the batch. So a crash in the middle loses nothing: the lease runs
//! out, another worker resumes after the last saved row, and the counters are right.
use super::email;
use super::fields::{self, FieldDefs, MAX_FIELDS};
use super::input::{LeadInput, RawLead};
use super::model::*;
use super::repo;
use super::routes::{limit_of, load_settings};
use super::upsert::{Outcome, UpsertCtx, upsert_batch};
use super::verify;
use crate::error::{ApiError, ApiJson, ApiPath, ApiQuery, ApiResult};
use crate::state::AppState;
use axum::extract::{DefaultBodyLimit, Multipart, State};
use axum::http::StatusCode;
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, HashSet};
use std::time::Duration as StdDuration;
use tokio_util::sync::CancellationToken;
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

pub const BATCH: usize = 1000;
/// A worker that does not save a batch for this long loses the import to another worker.
pub const LEASE_SECS: i64 = 120;
const MAX_ATTEMPTS: i32 = 5;
const MAX_ERRORS: usize = 1000;
/// The biggest file the setting `import_max_bytes` can allow, plus room for the form fields.
const BODY_LIMIT: usize = 201 * 1024 * 1024;
const PREVIEW_BYTES: usize = 512 * 1024;

pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(preview))
        .routes(routes!(start_import, list_imports))
        .routes(routes!(get_import))
        .routes(routes!(cancel_import))
        .layer(DefaultBodyLimit::max(BODY_LIMIT))
}

// ---------- CSV helpers (pure) ----------

/// The separator: the one of `, ; tab |` that appears most in the first line.
pub fn detect_delimiter(data: &[u8]) -> u8 {
    let data = strip_bom(data);
    let line = data.split(|b| *b == b'\n').next().unwrap_or(&[]);
    [b',', b';', b'\t', b'|'].into_iter().max_by_key(|d| line.iter().filter(|b| *b == d).count()).filter(|d| line.contains(d)).unwrap_or(b',')
}

fn strip_bom(data: &[u8]) -> &[u8] {
    data.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(data)
}

/// A reader that tolerates rows with a different number of cells.
pub fn csv_reader(data: &[u8]) -> csv::Reader<&[u8]> {
    csv::ReaderBuilder::new().delimiter(detect_delimiter(data)).flexible(true).from_reader(strip_bom(data))
}

pub(crate) fn cell(rec: &csv::ByteRecord, i: usize) -> String {
    String::from_utf8_lossy(rec.get(i).unwrap_or(b"")).trim().to_string()
}

pub use super::mapping::rules::guess_mapping;

#[derive(Clone, Debug, PartialEq)]
pub enum Target {
    Email,
    Text(String),
    Tags,
    Custom(String),
    /// One column with a full name: the last word is the last name, the rest is the first name.
    FullName,
}

fn parse_target(t: &str) -> Result<Option<Target>, String> {
    Ok(match t {
        "ignore" => None,
        "email" => Some(Target::Email),
        "tags" => Some(Target::Tags),
        "full_name" => Some(Target::FullName),
        "notes" => Some(Target::Text("notes".into())),
        t if TEXT_FIELDS.contains(&t) => Some(Target::Text(t.into())),
        t => match t.strip_prefix("custom:") {
            Some(k) => Some(Target::Custom(k.to_string())),
            None => return Err(format!("'{t}' is not a lead field. Use email, a text field, full_name, tags, notes, custom:<key> or ignore.")),
        },
    })
}

/// The columns to read: (column index, where it goes).
pub fn resolve_columns(headers: &[String], mapping: &Map<String, Value>) -> Vec<(usize, Target)> {
    let mut out = Vec::new();
    let mut seen_headers: HashSet<&str> = HashSet::new();
    for (i, h) in headers.iter().enumerate() {
        // two columns with the same name: the first one is used
        if !seen_headers.insert(h.as_str()) {
            continue;
        }
        if let Some(Ok(Some(t))) = mapping.get(h).and_then(Value::as_str).map(parse_target) {
            out.push((i, t));
        }
    }
    out
}

fn set_text(raw: &mut RawLead, name: &str, v: String) {
    let v = Some(v);
    match name {
        "first_name" => raw.first_name = v,
        "last_name" => raw.last_name = v,
        "company_name" => raw.company_name = v,
        "job_title" => raw.job_title = v,
        "phone" => raw.phone = v,
        "website" => raw.website = v,
        "linkedin_url" => raw.linkedin_url = v,
        "location" => raw.location = v,
        "timezone" => raw.timezone = v,
        "notes" => raw.notes = v,
        _ => {}
    }
}

/// "Mary Ann Smith" gives ("Mary Ann", Some("Smith")). One word gives the first name only.
pub fn split_full_name(full: &str) -> (String, Option<String>) {
    let full = full.trim();
    match full.rsplit_once(char::is_whitespace) {
        Some((first, last)) => (first.trim().to_string(), Some(last.trim().to_string())),
        None => (full.to_string(), None),
    }
}

/// One CSV row as a lead. Empty cells stay unset: they never erase data.
pub fn row_to_raw(rec: &csv::ByteRecord, columns: &[(usize, Target)]) -> RawLead {
    let mut raw = RawLead::default();
    let mut custom = Map::new();
    let mut tags: Vec<String> = Vec::new();
    for (i, target) in columns {
        let v = cell(rec, *i);
        if v.is_empty() {
            continue;
        }
        match target {
            Target::Email => raw.email = v,
            Target::Text(name) => set_text(&mut raw, name, v),
            Target::FullName => {
                let (first, last) = split_full_name(&v);
                raw.first_name = Some(first);
                raw.last_name = last;
            }
            Target::Tags => tags.extend(v.split([',', ';', '|']).map(str::to_string)),
            Target::Custom(k) => {
                custom.insert(k.clone(), Value::String(v));
            }
        }
    }
    if !custom.is_empty() {
        raw.custom_fields = Some(custom);
    }
    if !tags.is_empty() {
        raw.tags = Some(tags);
    }
    raw
}

pub(crate) fn headers_of(reader: &mut csv::Reader<&[u8]>) -> Result<Vec<String>, String> {
    let rec = reader.byte_headers().map_err(|e| format!("The file is not a readable CSV: {e}"))?;
    Ok(rec.iter().map(|c| String::from_utf8_lossy(c).trim().to_string()).collect())
}

// ---------- API types ----------

#[derive(sqlx::FromRow, Serialize, ToSchema, Clone, Debug)]
pub struct Import {
    pub id: Uuid,
    pub kind: String,
    pub file_name: Option<String>,
    pub status: ImportStatus,
    /// CSV column name to lead field, `custom:<key>`, or `ignore`.
    pub mapping: Value,
    pub dedupe_mode: DedupeMode,
    /// Every imported lead goes in this list.
    pub list_id: Option<Uuid>,
    pub campaign_id: Option<Uuid>,
    /// Added to every imported lead.
    pub tags: Vec<String>,
    /// Data rows in the file.
    pub total_rows: i32,
    /// Rows already written. The worker resumes after this row.
    pub rows_done: i32,
    pub created: i32,
    pub updated: i32,
    /// Existing leads that did not change, and repeats of an email inside the file.
    pub skipped: i32,
    pub invalid: i32,
    /// The first 1000 bad rows: `{ row, reason }`. `row` is the line in the file.
    pub errors: Value,
    /// Why the import failed.
    pub error: Option<String>,
    pub created_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
}

const IMPORT_COLS: &str = "id, kind, file_name, status, mapping, dedupe_mode, list_id, campaign_id, tags, total_rows, rows_done, created, updated, skipped, invalid, errors, error, created_at, finished_at";

#[derive(ToSchema)]
#[allow(dead_code)]
pub struct ImportForm {
    /// The CSV file. First row: column names. Separator `,` `;` tab or `|` is found by itself.
    #[schema(value_type = String, format = Binary)]
    file: Vec<u8>,
    /// JSON text: `{ "Column": "email" | "first_name" | ... | "custom:<key>" | "tags" | "ignore" }`. Leave out to guess from the column names.
    mapping: Option<String>,
    /// `skip`, `fill_empty` or `overwrite`. Default: the setting `default_dedupe_mode`.
    dedupe_mode: Option<String>,
    /// Put every imported lead in this list.
    list_id: Option<Uuid>,
    /// Comma separated. Added to every imported lead.
    tags: Option<String>,
    /// Add every imported lead to this campaign (as `pending`; the feeder enrolls them over the days).
    campaign_id: Option<Uuid>,
    /// `true`: a `custom:<key>` in the mapping that does not exist yet is made (kind `text`).
    create_fields: Option<bool>,
}

#[derive(Serialize, ToSchema)]
pub struct PreviewReply {
    /// The separator that was found.
    pub delimiter: String,
    pub headers: Vec<String>,
    /// The guessed mapping, column name to target. Send it (changed if you like) as `mapping` when you start the import.
    #[schema(value_type = Object)]
    pub mapping: BTreeMap<String, String>,
    /// The column that holds the email, if one was found.
    pub email_column: Option<String>,
    /// The first 20 rows.
    pub sample_rows: Vec<Vec<String>>,
}

#[derive(Deserialize, IntoParams)]
pub struct StartQuery {
    /// `true`: store the file, do not import yet, and answer with a column mapping proposal (rules, then AI) in `proposal`.
    /// The import starts when you confirm at `POST /v1/leads/imports/{id}/mapping/confirm`. Default `false` (the
    /// import starts at once with `mapping`, or with the guess from the column names). Not allowed together with `mapping`.
    auto_map: Option<bool>,
}

#[derive(Default)]
struct Form {
    file: Option<(Option<String>, Vec<u8>)>,
    fields: BTreeMap<String, String>,
}

fn multipart_error(e: impl std::fmt::Display) -> ApiError {
    ApiError::bad_request("invalid_multipart", format!("The form could not be read: {e}"))
}

/// Read the form. The file part stops at `max_file` bytes (413), or at `prefix_only` bytes (for the preview).
async fn read_form(mut mp: Multipart, max_file: usize, prefix_only: Option<usize>) -> ApiResult<Form> {
    let mut form = Form::default();
    while let Some(mut field) = mp.next_field().await.map_err(multipart_error)? {
        let name = field.name().unwrap_or_default().to_string();
        if name == "file" {
            let file_name = field.file_name().map(str::to_string);
            let mut data: Vec<u8> = Vec::new();
            while let Some(chunk) = field.chunk().await.map_err(multipart_error)? {
                data.extend_from_slice(&chunk);
                if data.len() > max_file {
                    return Err(ApiError::new(StatusCode::PAYLOAD_TOO_LARGE, "file_too_large", format!("The file is bigger than the limit of {max_file} bytes (setting import_max_bytes).")));
                }
                if prefix_only.is_some_and(|p| data.len() >= p) {
                    break;
                }
            }
            form.file = Some((file_name, data));
            if prefix_only.is_some() {
                break;
            }
        } else {
            let text = field.text().await.map_err(multipart_error)?;
            if text.len() > 1_000_000 {
                return Err(ApiError::bad_request("invalid_multipart", format!("The field '{name}' is too long.")));
            }
            form.fields.insert(name, text);
        }
    }
    Ok(form)
}

fn require_file(form: &mut Form) -> ApiResult<(Option<String>, Vec<u8>)> {
    match form.file.take() {
        Some((n, d)) if !d.is_empty() => Ok((n, d)),
        _ => Err(ApiError::unprocessable("file_required", "Send the CSV as the multipart field 'file'.")),
    }
}

// ---------- routes ----------

/// Look at the first rows of a CSV and guess the mapping.
#[utoipa::path(post, path = "/v1/leads/imports/preview", tag = "leads",
    request_body(content = ImportForm, content_type = "multipart/form-data"),
    responses((status = 200, body = PreviewReply), (status = 422, description = "No file, or not a CSV")),
    security(("api_key" = ["leads"])),
    description = "Scope: `leads`. Reads the first 20 rows. Nothing is saved.")]
async fn preview(State(st): State<AppState>, mp: Multipart) -> ApiResult<ApiJson<PreviewReply>> {
    let max = load_settings(&st.pool).await?.import_max_bytes as usize;
    let mut form = read_form(mp, max, Some(PREVIEW_BYTES)).await?;
    let (_, data) = require_file(&mut form)?;
    let mut reader = csv_reader(&data);
    let headers = headers_of(&mut reader).map_err(|m| ApiError::unprocessable("invalid_csv", m))?;
    let mut rows = Vec::new();
    for rec in reader.byte_records().take(20) {
        match rec {
            Ok(r) => rows.push((0..headers.len().max(r.len())).map(|i| cell(&r, i)).collect()),
            // the cut at the end of the prefix can leave half a row
            Err(_) => break,
        }
    }
    let mut conn = st.pool.acquire().await?;
    let defs = FieldDefs::load(&mut conn).await?;
    let mapping: BTreeMap<String, String> = guess_mapping(&headers, &defs).into_iter().collect();
    let email_column = headers.iter().find(|h| mapping.get(*h).is_some_and(|t| t == "email")).cloned();
    Ok(ApiJson(PreviewReply { delimiter: (detect_delimiter(&data) as char).to_string(), headers, mapping, email_column, sample_rows: rows }))
}

/// Start a CSV import.
#[utoipa::path(post, path = "/v1/leads/imports", tag = "leads",
    request_body(content = ImportForm, content_type = "multipart/form-data"),
    params(StartQuery),
    responses((status = 201, body = Import, description = "With `auto_map=true` the body also holds `proposal` (a `MappingProposal`)."), (status = 404, description = "No such list or campaign"), (status = 409, description = "The campaign is completed or archived"), (status = 413, description = "The file is too big"),
        (status = 422, description = "No file, no email column, a bad mapping, or too many rows")),
    security(("api_key" = ["leads"])),
    description = "Scope: `leads`. Returns at once. A background worker writes the rows; poll `GET /v1/leads/imports/{id}` for the counters. \
        Duplicate modes: `skip` (keep the old lead, still add it to the list and tags), `fill_empty` (fill only empty fields), `overwrite` \
        (replace fields that have a value in the file; an empty cell never erases data). The first row of an email wins inside the file. \
        Suppressed emails are imported too; they are skipped when enrolled. A cell value over 1000 characters makes the row invalid.")]
async fn start_import(State(st): State<AppState>, ApiQuery(q): ApiQuery<StartQuery>, mp: Multipart) -> ApiResult<(StatusCode, ApiJson<Value>)> {
    let settings = load_settings(&st.pool).await?;
    let mut form = read_form(mp, settings.import_max_bytes as usize, None).await?;
    let (file_name, data) = require_file(&mut form)?;

    let mut reader = csv_reader(&data);
    let headers = headers_of(&mut reader).map_err(|m| ApiError::unprocessable("invalid_csv", m))?;
    let mut total_rows: usize = 0;
    for rec in reader.byte_records() {
        rec.map_err(|e| ApiError::unprocessable("invalid_csv", format!("The file is not a readable CSV: {e}")))?;
        total_rows += 1;
        if total_rows > settings.import_max_rows as usize {
            return Err(ApiError::unprocessable("too_many_rows", format!("The file has more than {} rows (setting import_max_rows).", settings.import_max_rows)));
        }
    }
    if total_rows == 0 {
        return Err(ApiError::unprocessable("empty_file", "The file has a header but no rows."));
    }

    let mode = match form.fields.get("dedupe_mode").map(|s| s.trim()).filter(|s| !s.is_empty()) {
        None => settings.default_dedupe_mode,
        Some(m) => serde_json::from_value(Value::String(m.to_string()))
            .map_err(|_| ApiError::unprocessable("invalid_dedupe_mode", "dedupe_mode is skip, fill_empty or overwrite."))?,
    };
    let list_id: Option<Uuid> = match form.fields.get("list_id").map(|s| s.trim()).filter(|s| !s.is_empty()) {
        None => None,
        Some(s) => Some(s.parse().map_err(|_| ApiError::unprocessable("invalid_list_id", "list_id is not a valid id."))?),
    };
    let campaign_id: Option<Uuid> = match form.fields.get("campaign_id").map(|s| s.trim()).filter(|s| !s.is_empty()) {
        None => None,
        Some(s) => Some(s.parse().map_err(|_| ApiError::unprocessable("invalid_campaign_id", "campaign_id is not a valid id."))?),
    };
    let tags: Vec<String> = fields::clean_tags(&form.fields.get("tags").map(|t| t.split(',').map(str::to_string).collect::<Vec<_>>()).unwrap_or_default())
        .map_err(|m| ApiError::unprocessable("invalid_tags", m))?;
    let create_fields = form.fields.get("create_fields").is_some_and(|v| matches!(v.trim().to_lowercase().as_str(), "true" | "1" | "yes"));

    let mut tx = st.pool.begin().await?;
    if let Some(list_id) = list_id {
        let found: Option<(Uuid,)> = sqlx::query_as("SELECT id FROM lead_lists WHERE id = $1").bind(list_id).fetch_optional(&mut *tx).await?;
        if found.is_none() {
            return Err(ApiError::not_found("list_not_found", "No such list."));
        }
    }
    if let Some(campaign_id) = campaign_id {
        super::routes::require_campaign(&mut tx, campaign_id).await?;
    }
    let auto_map = q.auto_map.unwrap_or(false);
    if auto_map && form.fields.get("mapping").is_some_and(|m| !m.trim().is_empty()) {
        return Err(ApiError::unprocessable("auto_map_with_mapping", "Send either auto_map=true or a mapping, not both."));
    }
    let defs = FieldDefs::load(&mut tx).await?;
    let mapping: Map<String, Value> = if auto_map { Map::new() } else { match form.fields.get("mapping").map(|s| s.trim()).filter(|s| !s.is_empty()) {
        Some(text) => match serde_json::from_str::<Value>(text) {
            Ok(Value::Object(m)) => m,
            _ => return Err(ApiError::unprocessable("invalid_mapping", "mapping must be a JSON object: column name to target.")),
        },
        None => guess_mapping(&headers, &defs).into_iter().map(|(h, t)| (h, Value::String(t))).collect(),
    } };
    // check the mapping
    let mut targets: HashSet<String> = HashSet::new();
    let mut new_fields: Vec<String> = Vec::new();
    for (column, target) in &mapping {
        if !headers.contains(column) {
            return Err(ApiError::unprocessable("unknown_column", format!("The file has no column '{column}'.")));
        }
        let Some(t) = target.as_str() else { return Err(ApiError::unprocessable("invalid_mapping", format!("The target of '{column}' must be text."))) };
        let parsed = parse_target(t).map_err(|m| ApiError::unprocessable("invalid_mapping", m))?;
        if let Some(Target::Custom(k)) = &parsed {
            if !defs.by_key.contains_key(k) {
                if !create_fields {
                    return Err(ApiError::unprocessable("unknown_field", format!("The custom field '{k}' does not exist. Make it first, or send create_fields=true.")));
                }
                fields::check_key(k).map_err(|m| ApiError::unprocessable("invalid_key", m))?;
                new_fields.push(k.clone());
            }
        }
        if parsed.is_some() && !targets.insert(t.to_string()) {
            return Err(ApiError::unprocessable("duplicate_target", format!("Two columns go to '{t}'. Map one of them to ignore.")));
        }
    }
    if !auto_map && !targets.contains("email") {
        return Err(ApiError::unprocessable("no_email_column", "No column is mapped to email. Check the mapping, or the column names in the first row."));
    }
    new_fields.sort();
    new_fields.dedup();
    if !new_fields.is_empty() {
        sqlx::query("SELECT pg_advisory_xact_lock(hashtext('lead_fields'))").execute(&mut *tx).await?;
        let (n,): (i64,) = sqlx::query_as("SELECT count(*) FROM lead_fields").fetch_one(&mut *tx).await?;
        if n + new_fields.len() as i64 > MAX_FIELDS {
            return Err(ApiError::unprocessable("too_many_fields", format!("There can be up to {MAX_FIELDS} custom fields.")));
        }
        for k in &new_fields {
            sqlx::query("INSERT INTO lead_fields (key, label, kind, created_at) VALUES ($1, $1, 'text', $2) ON CONFLICT (key) DO NOTHING").bind(k).bind(st.now()).execute(&mut *tx).await?;
        }
    }

    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO lead_imports (id, kind, file_name, status, mapping, dedupe_mode, list_id, tags, total_rows, created_at, campaign_id)
         VALUES ($1, 'csv', $2, $10, $3, $4, $5, $6, $7, $8, $9)",
    )
    .bind(id)
    .bind(file_name.as_deref().map(|n| n.chars().take(255).collect::<String>()))
    .bind(Value::Object(mapping))
    .bind(mode)
    .bind(list_id)
    .bind(&tags)
    .bind(total_rows as i32)
    .bind(st.now())
    .bind(campaign_id)
    .bind(if auto_map { ImportStatus::Uploaded } else { ImportStatus::Pending })
    .execute(&mut *tx)
    .await?;
    sqlx::query("INSERT INTO lead_import_files (import_id, data) VALUES ($1, $2)").bind(id).bind(&data).execute(&mut *tx).await?;
    tx.commit().await?;
    let import = fetch(&st, id).await?;
    if !auto_map {
        return Ok((StatusCode::CREATED, ApiJson(serde_json::to_value(import).unwrap_or_default())));
    }
    let proposal = super::mapping::propose(&st, id, &data, true).await?;
    let mut body = serde_json::to_value(import).unwrap_or_default();
    body["proposal"] = serde_json::to_value(proposal).unwrap_or_default();
    Ok((StatusCode::CREATED, ApiJson(body)))
}

pub(crate) async fn fetch(st: &AppState, id: Uuid) -> ApiResult<Import> {
    sqlx::query_as(&format!("SELECT {IMPORT_COLS} FROM lead_imports WHERE id = $1"))
        .bind(id)
        .fetch_optional(&st.pool)
        .await?
        .ok_or_else(|| ApiError::not_found("import_not_found", "No such import."))
}

#[derive(Deserialize, IntoParams)]
pub struct ListImportsQuery {
    /// Only imports with this status.
    status: Option<ImportStatus>,
    /// The `next_cursor` of the last page.
    cursor: Option<Uuid>,
    /// 1 to 500. Default 50.
    limit: Option<i64>,
}

#[derive(Serialize, ToSchema)]
pub struct ListImportsReply {
    pub imports: Vec<Import>,
    pub next_cursor: Option<Uuid>,
}

/// List imports, newest first. The `errors` list is left out here; read one import to see it.
#[utoipa::path(get, path = "/v1/leads/imports", tag = "leads",
    params(ListImportsQuery),
    responses((status = 200, body = ListImportsReply)),
    security(("api_key" = ["leads"])), description = "Scope: `leads`.")]
async fn list_imports(State(st): State<AppState>, ApiQuery(q): ApiQuery<ListImportsQuery>) -> ApiResult<ApiJson<ListImportsReply>> {
    let limit = limit_of(q.limit)?;
    let cols = IMPORT_COLS.replace(" errors,", " '[]'::jsonb AS errors,");
    let mut imports: Vec<Import> = sqlx::query_as(&format!(
        "SELECT {cols} FROM lead_imports WHERE ($1::lead_import_status IS NULL OR status = $1) AND ($2::uuid IS NULL OR id < $2) ORDER BY id DESC LIMIT $3"
    ))
    .bind(q.status)
    .bind(q.cursor)
    .bind(limit + 1)
    .fetch_all(&st.pool)
    .await?;
    let next_cursor = if imports.len() as i64 > limit {
        imports.truncate(limit as usize);
        imports.last().map(|i| i.id)
    } else {
        None
    };
    Ok(ApiJson(ListImportsReply { imports, next_cursor }))
}

/// One import, with live counters and the bad rows.
#[utoipa::path(get, path = "/v1/leads/imports/{id}", tag = "leads",
    params(("id" = Uuid, Path)),
    responses((status = 200, body = Import), (status = 404, description = "No such import")),
    security(("api_key" = ["leads"])),
    description = "Scope: `leads`. The counters are saved with every batch of 1000 rows, so this shows live progress.")]
async fn get_import(State(st): State<AppState>, ApiPath(id): ApiPath<Uuid>) -> ApiResult<ApiJson<Import>> {
    Ok(ApiJson(fetch(&st, id).await?))
}

/// Stop an import. Rows already written stay.
#[utoipa::path(post, path = "/v1/leads/imports/{id}/cancel", tag = "leads",
    params(("id" = Uuid, Path)),
    responses((status = 200, body = Import), (status = 404, description = "No such import"), (status = 409, description = "The import already ended")),
    security(("api_key" = ["leads"])),
    description = "Scope: `leads`. The worker stops after the batch it is writing.")]
async fn cancel_import(State(st): State<AppState>, ApiPath(id): ApiPath<Uuid>) -> ApiResult<ApiJson<Import>> {
    let mut tx = st.pool.begin().await?;
    let done = sqlx::query("UPDATE lead_imports SET status = 'cancelled', finished_at = $2, claimed_at = NULL, claim_id = NULL WHERE id = $1 AND status IN ('uploaded', 'pending', 'running')")
        .bind(id)
        .bind(st.now())
        .execute(&mut *tx)
        .await?
        .rows_affected();
    if done == 0 {
        let exists: Option<(Uuid,)> = sqlx::query_as("SELECT id FROM lead_imports WHERE id = $1").bind(id).fetch_optional(&mut *tx).await?;
        return Err(match exists {
            Some(_) => ApiError::conflict("import_ended", "The import already ended."),
            None => ApiError::not_found("import_not_found", "No such import."),
        });
    }
    sqlx::query("DELETE FROM lead_import_files WHERE import_id = $1").bind(id).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(ApiJson(fetch(&st, id).await?))
}

// ---------- the worker ----------

struct Claimed {
    id: Uuid,
    claim_id: Uuid,
    mapping: Value,
    dedupe_mode: DedupeMode,
    list_id: Option<Uuid>,
    campaign_id: Option<Uuid>,
    tags: Vec<String>,
    rows_done: i32,
}

#[derive(Clone, Debug, Default)]
pub struct RunOpts {
    /// Tests: stop after this many batches, as if the worker had been killed. The import stays `running`.
    pub max_batches: Option<usize>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum RunEnd {
    Finished,
    Failed,
    /// Cancelled, or taken over by another worker.
    Lost,
    /// `RunOpts::max_batches` was reached.
    Stopped,
}

/// Take one import: a `pending` one, or a `running` one whose lease ran out.
async fn claim(st: &AppState) -> Result<Option<Claimed>, sqlx::Error> {
    let now = st.now();
    let claim_id = Uuid::now_v7();
    let row: Option<(Uuid, Value, DedupeMode, Option<Uuid>, Option<Uuid>, Vec<String>, i32, i32)> = sqlx::query_as(
        "UPDATE lead_imports SET status = 'running', claimed_at = $1, claim_id = $2, attempts = attempts + 1
          WHERE id = (SELECT id FROM lead_imports WHERE status = 'pending' OR (status = 'running' AND claimed_at < $3)
                      ORDER BY created_at, id LIMIT 1 FOR UPDATE SKIP LOCKED)
         RETURNING id, mapping, dedupe_mode, list_id, campaign_id, tags, rows_done, attempts",
    )
    .bind(now)
    .bind(claim_id)
    .bind(now - Duration::seconds(LEASE_SECS))
    .fetch_optional(&st.pool)
    .await?;
    let Some((id, mapping, dedupe_mode, list_id, campaign_id, tags, rows_done, attempts)) = row else { return Ok(None) };
    if attempts > MAX_ATTEMPTS {
        fail(st, id, claim_id, "The import was tried 5 times and did not finish.").await?;
        return Ok(None);
    }
    Ok(Some(Claimed { id, claim_id, mapping, dedupe_mode, list_id, campaign_id, tags, rows_done }))
}

async fn fail(st: &AppState, id: Uuid, claim_id: Uuid, why: &str) -> Result<(), sqlx::Error> {
    tracing::warn!(import = %id, why, "import failed");
    let mut tx = st.pool.begin().await?;
    sqlx::query("UPDATE lead_imports SET status = 'failed', error = $3, finished_at = $4, claimed_at = NULL, claim_id = NULL WHERE id = $1 AND claim_id = $2 AND status = 'running'")
        .bind(id)
        .bind(claim_id)
        .bind(why)
        .bind(st.now())
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM lead_import_files WHERE import_id = $1").bind(id).execute(&mut *tx).await?;
    tx.commit().await
}

#[derive(Default)]
struct Batch {
    valid: Vec<LeadInput>,
    /// Rows that were bad.
    invalid: i32,
    /// Rows whose email came earlier in the file.
    repeats: i32,
    errors: Vec<Value>,
    /// The number of the last row in the batch.
    last_row: i32,
    rows: usize,
}

enum Flushed {
    Ok,
    Lost,
}

/// Write one batch, its counters and the heartbeat, in one transaction. Then check the MX of the new domains.
async fn flush(st: &AppState, c: &Claimed, b: &mut Batch) -> Result<Flushed, sqlx::Error> {
    let now = st.now();
    let mut tx = st.pool.begin().await?;
    // the worker still owns the import? (not cancelled, not taken over)
    let owner: Option<(Uuid,)> = sqlx::query_as("SELECT id FROM lead_imports WHERE id = $1 AND claim_id = $2 AND status = 'running' FOR UPDATE").bind(c.id).bind(c.claim_id).fetch_optional(&mut *tx).await?;
    if owner.is_none() {
        return Ok(Flushed::Lost);
    }
    let ref_text = c.id.to_string();
    let ctx = UpsertCtx { mode: c.dedupe_mode, source: LeadSource::Csv, source_ref: Some(&ref_text), ref_id: Some(c.id), now };
    let results = upsert_batch(&mut tx, &b.valid, &ctx).await?;
    let ids: Vec<Uuid> = results.iter().map(|r| r.id).collect();
    if let Some(list_id) = c.list_id {
        repo::add_to_list(&mut tx, list_id, &ids, now).await?;
    }
    if !c.tags.is_empty() {
        repo::add_tags(&mut tx, &ids, &c.tags, now).await?;
    }
    if let Some(campaign_id) = c.campaign_id {
        // a campaign that ended meanwhile takes nothing: the import goes on without it
        repo::add_to_campaign(&mut tx, campaign_id, &ids, now).await?;
    }
    let count = |o: Outcome| results.iter().filter(|r| r.outcome == o).count() as i32;
    let (created, updated) = (count(Outcome::Created), count(Outcome::Updated));
    let skipped = count(Outcome::Skipped) + b.repeats;
    sqlx::query(
        "UPDATE lead_imports SET created = created + $3, updated = updated + $4, skipped = skipped + $5, invalid = invalid + $6, rows_done = $7, claimed_at = $8,
                errors = COALESCE((SELECT jsonb_agg(e ORDER BY n) FROM (SELECT e, n FROM jsonb_array_elements(errors || $9::jsonb) WITH ORDINALITY x(e, n) ORDER BY n LIMIT $10) y), '[]')
          WHERE id = $1 AND claim_id = $2",
    )
    .bind(c.id)
    .bind(c.claim_id)
    .bind(created)
    .bind(updated)
    .bind(skipped)
    .bind(b.invalid)
    .bind(b.last_row)
    .bind(now)
    .bind(Value::Array(std::mem::take(&mut b.errors)))
    .bind(MAX_ERRORS as i64)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;

    // the MX check of the new domains (one lookup per domain; cached)
    let domains: Vec<String> = b.valid.iter().zip(&results).filter(|(_, r)| r.outcome == Outcome::Created).map(|(l, _)| l.domain.clone()).collect();
    if !domains.is_empty() {
        verify::verify_domains(st, &domains, false, StdDuration::from_secs(60)).await?;
        sqlx::query("UPDATE lead_imports SET claimed_at = $3 WHERE id = $1 AND claim_id = $2").bind(c.id).bind(c.claim_id).bind(st.now()).execute(&st.pool).await?;
    }
    *b = Batch::default();
    Ok(Flushed::Ok)
}

/// Run one claimed import to its end.
async fn process(st: &AppState, c: Claimed, opts: &RunOpts) -> anyhow::Result<RunEnd> {
    let data: Option<(Vec<u8>,)> = sqlx::query_as("SELECT data FROM lead_import_files WHERE import_id = $1").bind(c.id).fetch_optional(&st.pool).await?;
    let Some((data,)) = data else {
        fail(st, c.id, c.claim_id, "The uploaded file is gone.").await?;
        return Ok(RunEnd::Failed);
    };
    let defs = {
        let mut conn = st.pool.acquire().await?;
        FieldDefs::load(&mut conn).await?
    };
    let mut reader = csv_reader(&data);
    let headers = match headers_of(&mut reader) {
        Ok(h) => h,
        Err(m) => {
            fail(st, c.id, c.claim_id, &m).await?;
            return Ok(RunEnd::Failed);
        }
    };
    let mapping = c.mapping.as_object().cloned().unwrap_or_default();
    let columns = resolve_columns(&headers, &mapping);
    if !columns.iter().any(|(_, t)| *t == Target::Email) {
        fail(st, c.id, c.claim_id, "No column is mapped to email.").await?;
        return Ok(RunEnd::Failed);
    }

    // emails seen in the file. Rows before `rows_done` were written by an earlier run: only their emails are read.
    let mut seen: HashSet<String> = HashSet::new();
    let mut row_no: i32 = 0;
    let mut batch = Batch::default();
    let mut batches = 0usize;
    for rec in reader.byte_records() {
        let rec = match rec {
            Ok(r) => r,
            Err(e) => {
                fail(st, c.id, c.claim_id, &format!("The file is not a readable CSV: {e}")).await?;
                return Ok(RunEnd::Failed);
            }
        };
        row_no += 1;
        let raw = row_to_raw(&rec, &columns);
        if row_no <= c.rows_done {
            if let Ok(e) = email::normalize(&raw.email) {
                seen.insert(e);
            }
            continue;
        }
        batch.rows += 1;
        batch.last_row = row_no;
        match raw.check(&defs) {
            Ok(lead) => {
                if seen.insert(lead.email.clone()) {
                    batch.valid.push(lead);
                } else {
                    batch.repeats += 1;
                }
            }
            Err(reason) => {
                batch.invalid += 1;
                if batch.errors.len() < MAX_ERRORS {
                    batch.errors.push(json!({ "row": rec.position().map(|p| p.line()).unwrap_or(row_no as u64 + 1), "reason": reason }));
                }
            }
        }
        if batch.rows >= BATCH {
            if let Flushed::Lost = flush(st, &c, &mut batch).await? {
                return Ok(RunEnd::Lost);
            }
            batches += 1;
            if opts.max_batches.is_some_and(|m| batches >= m) {
                return Ok(RunEnd::Stopped);
            }
        }
    }
    if batch.rows > 0 {
        if let Flushed::Lost = flush(st, &c, &mut batch).await? {
            return Ok(RunEnd::Lost);
        }
    }

    let mut tx = st.pool.begin().await?;
    let done = sqlx::query("UPDATE lead_imports SET status = 'done', finished_at = $3, claimed_at = NULL, claim_id = NULL, rows_done = GREATEST(rows_done, $4) WHERE id = $1 AND claim_id = $2 AND status = 'running'")
        .bind(c.id)
        .bind(c.claim_id)
        .bind(st.now())
        .bind(row_no)
        .execute(&mut *tx)
        .await?
        .rows_affected();
    if done == 1 {
        sqlx::query("DELETE FROM lead_import_files WHERE import_id = $1").bind(c.id).execute(&mut *tx).await?;
    }
    tx.commit().await?;
    Ok(if done == 1 { RunEnd::Finished } else { RunEnd::Lost })
}

/// Process every import that is waiting. Returns how many ended (any way).
pub async fn tick(st: &AppState, opts: &RunOpts) -> anyhow::Result<usize> {
    let mut n = 0;
    while let Some(c) = claim(st).await? {
        let id = c.id;
        match process(st, c, opts).await {
            Ok(RunEnd::Stopped) => return Ok(n),
            Ok(end) => {
                tracing::info!(import = %id, ?end, "import ended");
                n += 1;
            }
            // a database error: the lease runs out and the import is tried again
            Err(e) => {
                tracing::error!(import = %id, error = %format!("{e:#}"), "import worker error");
                return Ok(n);
            }
        }
    }
    Ok(n)
}

/// The worker loop. Looks for imports every 2 seconds.
pub async fn run(st: AppState, cancel: CancellationToken) {
    loop {
        if let Err(e) = tick(&st, &RunOpts::default()).await {
            tracing::error!(error = %format!("{e:#}"), "import tick failed");
        }
        tokio::select! {
            _ = cancel.cancelled() => return,
            _ = tokio::time::sleep(StdDuration::from_secs(2)) => {}
        }
    }
}
