//! The own rows of a list. A CSV goes into a list as it is: every column is kept, no mapping, no leads.
//! Editing a row changes only that list. To email the rows, send them to a campaign: that makes a normal
//! import (status `uploaded`) from the rows, and the caller maps the columns as for any CSV.
use super::import::{BODY_LIMIT, csv_reader, headers_of, read_form, require_file};
use super::model::{DedupeMode, ImportStatus};
use super::routes::{get_list, limit_of, load_settings};
use crate::error::{ApiError, ApiJson, ApiPath, ApiQuery, ApiResult};
use crate::state::AppState;
use axum::extract::{DefaultBodyLimit, Multipart, State};
use axum::http::StatusCode;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::HashSet;
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

pub const MAX_COLUMNS: usize = 200;
const MAX_COLUMN_NAME: usize = 100;
const MAX_CELL: usize = 10_000;
const INSERT_BATCH: usize = 1000;

pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(import_rows))
        .routes(routes!(list_rows, add_row, delete_rows))
        .routes(routes!(patch_row))
        .routes(routes!(add_column, rename_column, delete_column))
        .routes(routes!(send_to_campaign))
        .layer(DefaultBodyLimit::max(BODY_LIMIT))
}

// ---------- pure helpers ----------

fn cut(s: &str, max: usize) -> String {
    s.chars().take(max).collect()
}

/// The headers of a file as column names: an empty one is `Column N`, a repeat gets ` (2)`, ` (3)`...
pub fn clean_headers(headers: &[String]) -> Vec<String> {
    let mut seen: HashSet<String> = HashSet::new();
    headers
        .iter()
        .enumerate()
        .map(|(i, h)| {
            let base = match cut(h.trim(), MAX_COLUMN_NAME) {
                b if b.is_empty() => format!("Column {}", i + 1),
                b => b,
            };
            let mut name = base.clone();
            let mut n = 2;
            while !seen.insert(name.to_lowercase()) {
                name = format!("{base} ({n})");
                n += 1;
            }
            name
        })
        .collect()
}

/// The columns of the list after a file with `incoming` columns: the old ones, then the new names.
/// A name that differs only in case is the same column.
pub fn merge_columns(existing: &[String], incoming: &[String]) -> Vec<String> {
    let mut out = existing.to_vec();
    for c in incoming {
        if !out.iter().any(|o| o.eq_ignore_ascii_case(c)) {
            out.push(c.clone());
        }
    }
    out
}

/// The list column a file column goes to (same name, any case).
fn column_for<'a>(columns: &'a [String], name: &str) -> Option<&'a String> {
    columns.iter().find(|c| c.eq_ignore_ascii_case(name))
}

/// Rows as a CSV with the given columns. Missing cells are empty.
pub fn rows_to_csv(columns: &[String], rows: &[Value]) -> Result<Vec<u8>, csv::Error> {
    let mut w = csv::Writer::from_writer(Vec::new());
    w.write_record(columns)?;
    for r in rows {
        w.write_record(columns.iter().map(|c| r.get(c).and_then(Value::as_str).unwrap_or("")))?;
    }
    w.into_inner().map_err(|e| csv::Error::from(e.into_error()))
}

fn columns_of(v: &Value) -> Vec<String> {
    v.as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect()).unwrap_or_default()
}

fn clean_column_name(name: &str) -> ApiResult<String> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > MAX_COLUMN_NAME {
        return Err(ApiError::unprocessable("invalid_column", format!("A column name is 1 to {MAX_COLUMN_NAME} characters.")));
    }
    Ok(name.to_string())
}

// ---------- API types ----------

#[derive(sqlx::FromRow, Serialize, ToSchema)]
pub struct ListRow {
    pub id: Uuid,
    /// Column name to text.
    #[schema(value_type = Object)]
    pub data: Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Serialize, ToSchema)]
pub struct ListRowsReply {
    pub columns: Vec<String>,
    pub rows: Vec<ListRow>,
    /// Rows that match (all of them without `q`).
    pub total: i64,
    pub next_cursor: Option<Uuid>,
}

#[derive(Deserialize, IntoParams)]
pub struct RowsQuery {
    /// 1 to 500. Default 50.
    limit: Option<i64>,
    /// `next_cursor` of the page before.
    cursor: Option<Uuid>,
    /// Text to find in any cell.
    q: Option<String>,
}

#[derive(Serialize, ToSchema)]
pub struct ListImportReply {
    /// Rows added to the list.
    pub added: i64,
    /// Rows with only empty cells, left out.
    pub empty: i64,
    /// The columns of the list now.
    pub columns: Vec<String>,
    /// Columns the file added.
    pub new_columns: Vec<String>,
}

#[derive(ToSchema)]
#[allow(dead_code)]
pub struct ListImportForm {
    /// The CSV file. First row: column names. Every column is kept as it is.
    #[schema(value_type = String, format = Binary)]
    file: Vec<u8>,
}

#[derive(Deserialize, ToSchema)]
pub struct NewRow {
    /// Column name to text. Unknown columns are not allowed.
    #[serde(default)]
    pub data: Map<String, Value>,
}

#[derive(Deserialize, ToSchema)]
pub struct RowIds {
    pub row_ids: Vec<Uuid>,
}

#[derive(Serialize, ToSchema)]
pub struct DeletedRows {
    pub deleted: u64,
}

#[derive(Deserialize, ToSchema)]
pub struct ColumnName {
    pub name: String,
}

#[derive(Deserialize, ToSchema)]
pub struct ColumnRename {
    pub from: String,
    pub to: String,
}

#[derive(Serialize, ToSchema)]
pub struct ColumnsReply {
    pub columns: Vec<String>,
}

#[derive(Deserialize, ToSchema)]
pub struct SendToCampaign {
    pub campaign_id: Uuid,
    /// Only these rows. Leave out to send every row of the list.
    pub row_ids: Option<Vec<Uuid>>,
    /// `skip`, `fill_empty` or `overwrite`. Default: the setting `default_dedupe_mode`.
    pub dedupe_mode: Option<DedupeMode>,
}

// ---------- db helpers ----------

/// The columns of the list, locked for the rest of the transaction.
async fn lock_columns(conn: &mut sqlx::PgConnection, id: Uuid) -> ApiResult<Vec<String>> {
    let row: Option<(Value,)> = sqlx::query_as("SELECT columns FROM lead_lists WHERE id = $1 FOR UPDATE").bind(id).fetch_optional(conn).await?;
    row.map(|r| columns_of(&r.0)).ok_or_else(|| ApiError::not_found("list_not_found", "No such list."))
}

async fn save_columns(conn: &mut sqlx::PgConnection, id: Uuid, columns: &[String], now: DateTime<Utc>) -> ApiResult<()> {
    sqlx::query("UPDATE lead_lists SET columns = $2, updated_at = $3 WHERE id = $1").bind(id).bind(serde_json::json!(columns)).bind(now).execute(conn).await?;
    Ok(())
}

/// A row from the caller: only known columns, text values, empty and null cells left out.
fn check_cells(columns: &[String], data: &Map<String, Value>) -> ApiResult<Map<String, Value>> {
    let mut out = Map::new();
    for (k, v) in data {
        let Some(col) = column_for(columns, k) else { return Err(ApiError::unprocessable("unknown_column", format!("The list has no column '{k}'."))) };
        match v {
            Value::Null => {}
            Value::String(s) if s.trim().is_empty() => {}
            Value::String(s) => {
                out.insert(col.clone(), Value::String(cut(s.trim(), MAX_CELL)));
            }
            _ => return Err(ApiError::unprocessable("invalid_value", format!("The value of '{k}' must be text."))),
        }
    }
    Ok(out)
}

// ---------- routes ----------

/// Import a CSV into the list, as it is.
#[utoipa::path(post, path = "/v1/leads/lists/{id}/import", tag = "leads",
    params(("id" = Uuid, Path)),
    request_body(content = ListImportForm, content_type = "multipart/form-data"),
    responses((status = 201, body = ListImportReply), (status = 404, description = "No such list"), (status = 413, description = "The file is too big"),
        (status = 422, description = "No file, not a CSV, no rows, too many rows or columns")),
    security(("api_key" = ["leads"])),
    description = "Scope: `leads`. Every column is kept with its name; a column the list does not have yet is added. \
        The rows stay in this list only. They are not leads and do not change other lists. Send them to a campaign with \
        `POST /v1/leads/lists/{id}/send-to-campaign`.")]
async fn import_rows(State(st): State<AppState>, ApiPath(id): ApiPath<Uuid>, mp: Multipart) -> ApiResult<(StatusCode, ApiJson<ListImportReply>)> {
    let settings = load_settings(&st.pool).await?;
    let mut form = read_form(mp, settings.import_max_bytes as usize, None).await?;
    let (_, data) = require_file(&mut form)?;
    let mut reader = csv_reader(&data);
    let headers = clean_headers(&headers_of(&mut reader).map_err(|m| ApiError::unprocessable("invalid_csv", m))?);

    let mut tx = st.pool.begin().await?;
    let existing = lock_columns(&mut tx, id).await?;
    let columns = merge_columns(&existing, &headers);
    if columns.len() > MAX_COLUMNS {
        return Err(ApiError::unprocessable("too_many_columns", format!("A list can have up to {MAX_COLUMNS} columns.")));
    }
    // file column -> list column
    let targets: Vec<String> = headers.iter().map(|h| column_for(&columns, h).cloned().unwrap_or_else(|| h.clone())).collect();

    let now = st.now();
    let (mut added, mut empty) = (0i64, 0i64);
    let mut ids: Vec<Uuid> = Vec::with_capacity(INSERT_BATCH);
    let mut datas: Vec<Value> = Vec::with_capacity(INSERT_BATCH);
    for rec in reader.byte_records() {
        let rec = rec.map_err(|e| ApiError::unprocessable("invalid_csv", format!("The file is not a readable CSV: {e}")))?;
        let mut row = Map::new();
        for (i, col) in targets.iter().enumerate() {
            let v = super::import::cell(&rec, i);
            if !v.is_empty() {
                row.insert(col.clone(), Value::String(cut(&v, MAX_CELL)));
            }
        }
        if row.is_empty() {
            empty += 1;
            continue;
        }
        added += 1;
        if added > settings.import_max_rows as i64 {
            return Err(ApiError::unprocessable("too_many_rows", format!("The file has more than {} rows (setting import_max_rows).", settings.import_max_rows)));
        }
        ids.push(Uuid::now_v7());
        datas.push(Value::Object(row));
        if ids.len() >= INSERT_BATCH {
            insert_rows(&mut tx, id, &ids, &datas, now).await?;
            ids.clear();
            datas.clear();
        }
    }
    if !ids.is_empty() {
        insert_rows(&mut tx, id, &ids, &datas, now).await?;
    }
    if added == 0 {
        return Err(ApiError::unprocessable("empty_file", "The file has a header but no rows."));
    }
    save_columns(&mut tx, id, &columns, now).await?;
    tx.commit().await?;
    let new_columns = columns[existing.len()..].to_vec();
    Ok((StatusCode::CREATED, ApiJson(ListImportReply { added, empty, columns, new_columns })))
}

async fn insert_rows(conn: &mut sqlx::PgConnection, list_id: Uuid, ids: &[Uuid], datas: &[Value], now: DateTime<Utc>) -> ApiResult<()> {
    sqlx::query("INSERT INTO lead_list_rows (id, list_id, data, created_at, updated_at) SELECT u.id, $1, u.data, $4, $4 FROM UNNEST($2::uuid[], $3::jsonb[]) AS u(id, data)")
        .bind(list_id)
        .bind(ids)
        .bind(datas)
        .bind(now)
        .execute(conn)
        .await?;
    Ok(())
}

/// The rows of the list, in file order.
#[utoipa::path(get, path = "/v1/leads/lists/{id}/rows", tag = "leads",
    params(("id" = Uuid, Path), RowsQuery),
    responses((status = 200, body = ListRowsReply), (status = 404, description = "No such list")),
    security(("api_key" = ["leads"])), description = "Scope: `leads`.")]
async fn list_rows(State(st): State<AppState>, ApiPath(id): ApiPath<Uuid>, ApiQuery(q): ApiQuery<RowsQuery>) -> ApiResult<ApiJson<ListRowsReply>> {
    let limit = limit_of(q.limit)?;
    let list = get_list(&st, id).await?;
    let find = q.q.as_deref().map(str::trim).filter(|s| !s.is_empty()).map(|s| format!("%{}%", s.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_")));
    // the search looks at the values only, not the column names
    const MATCH: &str = "($2::text IS NULL OR EXISTS (SELECT 1 FROM jsonb_each_text(r.data) e WHERE e.value ILIKE $2))";
    let mut rows: Vec<ListRow> = sqlx::query_as(&format!(
        "SELECT r.id, r.data, r.created_at, r.updated_at FROM lead_list_rows r WHERE r.list_id = $1 AND {MATCH} AND ($3::uuid IS NULL OR r.id > $3) ORDER BY r.id LIMIT $4"
    ))
    .bind(id)
    .bind(&find)
    .bind(q.cursor)
    .bind(limit + 1)
    .fetch_all(&st.pool)
    .await?;
    let next_cursor = if rows.len() as i64 > limit {
        rows.truncate(limit as usize);
        rows.last().map(|r| r.id)
    } else {
        None
    };
    let total = if find.is_none() {
        list.row_count
    } else {
        let (n,): (i64,) = sqlx::query_as(&format!("SELECT count(*) FROM lead_list_rows r WHERE r.list_id = $1 AND {MATCH}")).bind(id).bind(&find).fetch_one(&st.pool).await?;
        n
    };
    Ok(ApiJson(ListRowsReply { columns: columns_of(&list.columns), rows, total, next_cursor }))
}

/// Add one row by hand.
#[utoipa::path(post, path = "/v1/leads/lists/{id}/rows", tag = "leads",
    params(("id" = Uuid, Path)), request_body = NewRow,
    responses((status = 201, body = ListRow), (status = 404, description = "No such list"), (status = 422, description = "An unknown column, or not text")),
    security(("api_key" = ["leads"])), description = "Scope: `leads`.")]
async fn add_row(State(st): State<AppState>, ApiPath(id): ApiPath<Uuid>, ApiJson(body): ApiJson<NewRow>) -> ApiResult<(StatusCode, ApiJson<ListRow>)> {
    let mut tx = st.pool.begin().await?;
    let columns = lock_columns(&mut tx, id).await?;
    let data = check_cells(&columns, &body.data)?;
    let now = st.now();
    let row: ListRow = sqlx::query_as("INSERT INTO lead_list_rows (id, list_id, data, created_at, updated_at) VALUES ($1, $2, $3, $4, $4) RETURNING id, data, created_at, updated_at")
        .bind(Uuid::now_v7())
        .bind(id)
        .bind(Value::Object(data))
        .bind(now)
        .fetch_one(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, ApiJson(row)))
}

/// Change cells of one row.
#[utoipa::path(patch, path = "/v1/leads/lists/{id}/rows/{row_id}", tag = "leads",
    params(("id" = Uuid, Path), ("row_id" = Uuid, Path)), request_body(content = Object, description = "Column name to text. `null` or empty text clears the cell."),
    responses((status = 200, body = ListRow), (status = 404, description = "No such list or row"), (status = 422, description = "An unknown column, or not text")),
    security(("api_key" = ["leads"])), description = "Scope: `leads`. Changes this list only.")]
async fn patch_row(State(st): State<AppState>, ApiPath((id, row_id)): ApiPath<(Uuid, Uuid)>, ApiJson(patch): ApiJson<Map<String, Value>>) -> ApiResult<ApiJson<ListRow>> {
    let list = get_list(&st, id).await?;
    let columns = columns_of(&list.columns);
    let set = check_cells(&columns, &patch)?;
    let clear: Vec<String> = patch.keys().filter_map(|k| column_for(&columns, k)).filter(|c| !set.contains_key(*c)).cloned().collect();
    let row: Option<ListRow> = sqlx::query_as(
        "UPDATE lead_list_rows SET data = (data - $3::text[]) || $4, updated_at = $5 WHERE id = $2 AND list_id = $1 RETURNING id, data, created_at, updated_at",
    )
    .bind(id)
    .bind(row_id)
    .bind(&clear)
    .bind(Value::Object(set))
    .bind(st.now())
    .fetch_optional(&st.pool)
    .await?;
    row.map(ApiJson).ok_or_else(|| ApiError::not_found("row_not_found", "No such row in this list."))
}

/// Delete rows of the list.
#[utoipa::path(delete, path = "/v1/leads/lists/{id}/rows", tag = "leads",
    params(("id" = Uuid, Path)), request_body = RowIds,
    responses((status = 200, body = DeletedRows), (status = 404, description = "No such list")),
    security(("api_key" = ["leads"])), description = "Scope: `leads`. Leads made from these rows stay.")]
async fn delete_rows(State(st): State<AppState>, ApiPath(id): ApiPath<Uuid>, ApiJson(body): ApiJson<RowIds>) -> ApiResult<ApiJson<DeletedRows>> {
    get_list(&st, id).await?;
    let n = sqlx::query("DELETE FROM lead_list_rows WHERE list_id = $1 AND id = ANY($2)").bind(id).bind(&body.row_ids).execute(&st.pool).await?.rows_affected();
    Ok(ApiJson(DeletedRows { deleted: n }))
}

/// Add an empty column.
#[utoipa::path(post, path = "/v1/leads/lists/{id}/columns", tag = "leads",
    params(("id" = Uuid, Path)), request_body = ColumnName,
    responses((status = 200, body = ColumnsReply), (status = 404, description = "No such list"), (status = 409, description = "The column exists"), (status = 422, description = "A bad name, or too many columns")),
    security(("api_key" = ["leads"])), description = "Scope: `leads`.")]
async fn add_column(State(st): State<AppState>, ApiPath(id): ApiPath<Uuid>, ApiJson(body): ApiJson<ColumnName>) -> ApiResult<ApiJson<ColumnsReply>> {
    let name = clean_column_name(&body.name)?;
    let mut tx = st.pool.begin().await?;
    let mut columns = lock_columns(&mut tx, id).await?;
    if column_for(&columns, &name).is_some() {
        return Err(ApiError::conflict("column_exists", "The list has a column with this name."));
    }
    if columns.len() >= MAX_COLUMNS {
        return Err(ApiError::unprocessable("too_many_columns", format!("A list can have up to {MAX_COLUMNS} columns.")));
    }
    columns.push(name);
    save_columns(&mut tx, id, &columns, st.now()).await?;
    tx.commit().await?;
    Ok(ApiJson(ColumnsReply { columns }))
}

/// Rename a column.
#[utoipa::path(patch, path = "/v1/leads/lists/{id}/columns", tag = "leads",
    params(("id" = Uuid, Path)), request_body = ColumnRename,
    responses((status = 200, body = ColumnsReply), (status = 404, description = "No such list or column"), (status = 409, description = "The new name exists")),
    security(("api_key" = ["leads"])), description = "Scope: `leads`.")]
async fn rename_column(State(st): State<AppState>, ApiPath(id): ApiPath<Uuid>, ApiJson(body): ApiJson<ColumnRename>) -> ApiResult<ApiJson<ColumnsReply>> {
    let to = clean_column_name(&body.to)?;
    let mut tx = st.pool.begin().await?;
    let mut columns = lock_columns(&mut tx, id).await?;
    let Some(pos) = columns.iter().position(|c| c == &body.from) else { return Err(ApiError::not_found("column_not_found", "The list has no such column.")) };
    if columns.iter().enumerate().any(|(i, c)| i != pos && c.eq_ignore_ascii_case(&to)) {
        return Err(ApiError::conflict("column_exists", "The list has a column with this name."));
    }
    let from = std::mem::replace(&mut columns[pos], to.clone());
    sqlx::query("UPDATE lead_list_rows SET data = (data - $2::text) || jsonb_build_object($3::text, data -> $2::text) WHERE list_id = $1 AND data ? $2::text")
        .bind(id)
        .bind(&from)
        .bind(&to)
        .execute(&mut *tx)
        .await?;
    save_columns(&mut tx, id, &columns, st.now()).await?;
    tx.commit().await?;
    Ok(ApiJson(ColumnsReply { columns }))
}

/// Delete a column and its values.
#[utoipa::path(delete, path = "/v1/leads/lists/{id}/columns", tag = "leads",
    params(("id" = Uuid, Path)), request_body = ColumnName,
    responses((status = 200, body = ColumnsReply), (status = 404, description = "No such list or column")),
    security(("api_key" = ["leads"])), description = "Scope: `leads`. Changes this list only.")]
async fn delete_column(State(st): State<AppState>, ApiPath(id): ApiPath<Uuid>, ApiJson(body): ApiJson<ColumnName>) -> ApiResult<ApiJson<ColumnsReply>> {
    let mut tx = st.pool.begin().await?;
    let mut columns = lock_columns(&mut tx, id).await?;
    let Some(pos) = columns.iter().position(|c| c == &body.name) else { return Err(ApiError::not_found("column_not_found", "The list has no such column.")) };
    let name = columns.remove(pos);
    sqlx::query("UPDATE lead_list_rows SET data = data - $2::text WHERE list_id = $1 AND data ? $2::text").bind(id).bind(&name).execute(&mut *tx).await?;
    save_columns(&mut tx, id, &columns, st.now()).await?;
    tx.commit().await?;
    Ok(ApiJson(ColumnsReply { columns }))
}

/// Send rows of the list to a campaign. Answers like an upload with `auto_map=true`.
#[utoipa::path(post, path = "/v1/leads/lists/{id}/send-to-campaign", tag = "leads",
    params(("id" = Uuid, Path)), request_body = SendToCampaign,
    responses((status = 201, body = crate::leads::import::Import, description = "The import, `uploaded`. The body also holds `proposal` (a `MappingProposal`)."),
        (status = 404, description = "No such list or campaign"), (status = 409, description = "The campaign is completed or archived"), (status = 422, description = "No rows to send")),
    security(("api_key" = ["leads"])),
    description = "Scope: `leads`. Makes an import from the rows (the list columns are the file columns) and proposes a column mapping. \
        Confirm it at `POST /v1/leads/imports/{id}/mapping/confirm`: then the rows become leads (or update leads with the same email) and are added to the campaign. \
        The list and its rows do not change.")]
async fn send_to_campaign(State(st): State<AppState>, ApiPath(id): ApiPath<Uuid>, ApiJson(body): ApiJson<SendToCampaign>) -> ApiResult<(StatusCode, ApiJson<Value>)> {
    let settings = load_settings(&st.pool).await?;
    let list = get_list(&st, id).await?;
    let columns = columns_of(&list.columns);
    let datas: Vec<(Value,)> = sqlx::query_as("SELECT data FROM lead_list_rows WHERE list_id = $1 AND ($2::uuid[] IS NULL OR id = ANY($2)) ORDER BY id")
        .bind(id)
        .bind(&body.row_ids)
        .fetch_all(&st.pool)
        .await?;
    if datas.is_empty() || columns.is_empty() {
        return Err(ApiError::unprocessable("no_rows", "There are no rows to send."));
    }
    if datas.len() > settings.import_max_rows as usize {
        return Err(ApiError::unprocessable("too_many_rows", format!("Send up to {} rows at a time (setting import_max_rows).", settings.import_max_rows)));
    }
    let rows: Vec<Value> = datas.into_iter().map(|d| d.0).collect();
    let data = rows_to_csv(&columns, &rows).map_err(|_| ApiError::internal())?;
    let mode = body.dedupe_mode.unwrap_or(settings.default_dedupe_mode);

    let mut tx = st.pool.begin().await?;
    super::routes::require_campaign(&mut tx, body.campaign_id).await?;
    let import_id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO lead_imports (id, kind, file_name, status, mapping, dedupe_mode, total_rows, created_at, campaign_id)
         VALUES ($1, 'csv', $2, $3, '{}', $4, $5, $6, $7)",
    )
    .bind(import_id)
    .bind(cut(&format!("List: {}", list.name), 255))
    .bind(ImportStatus::Uploaded)
    .bind(mode)
    .bind(rows.len() as i32)
    .bind(st.now())
    .bind(body.campaign_id)
    .execute(&mut *tx)
    .await?;
    sqlx::query("INSERT INTO lead_import_files (import_id, data) VALUES ($1, $2)").bind(import_id).bind(&data).execute(&mut *tx).await?;
    tx.commit().await?;
    let import = super::import::fetch(&st, import_id).await?;
    let proposal = super::mapping::propose(&st, import_id, &data, true).await?;
    let mut out = serde_json::to_value(import).unwrap_or_default();
    out["proposal"] = serde_json::to_value(proposal).unwrap_or_default();
    Ok((StatusCode::CREATED, ApiJson(out)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn headers_get_names_and_no_repeats() {
        assert_eq!(clean_headers(&s(&["Email", "", "email", " Name "])), s(&["Email", "Column 2", "email (2)", "Name"]));
    }

    #[test]
    fn merge_keeps_order_and_ignores_case() {
        assert_eq!(merge_columns(&s(&["Email", "Name"]), &s(&["name", "City"])), s(&["Email", "Name", "City"]));
    }

    #[test]
    fn csv_round_trip() {
        let cols = s(&["Email", "Note"]);
        let rows = vec![serde_json::json!({"Email": "a@x.com", "Note": "hi, there"}), serde_json::json!({"Note": "only"})];
        let data = rows_to_csv(&cols, &rows).unwrap();
        let mut r = csv_reader(&data);
        assert_eq!(headers_of(&mut r).unwrap(), cols);
        let recs: Vec<_> = r.byte_records().map(|x| x.unwrap()).collect();
        assert_eq!(super::super::import::cell(&recs[0], 1), "hi, there");
        assert_eq!(super::super::import::cell(&recs[1], 0), "");
    }

    #[test]
    fn cells_must_be_known_text() {
        let cols = s(&["Email"]);
        let mut m = Map::new();
        m.insert("email".into(), Value::String(" a@x.com ".into()));
        assert_eq!(check_cells(&cols, &m).unwrap().get("Email"), Some(&Value::String("a@x.com".into())));
        m.insert("Other".into(), Value::String("x".into()));
        assert!(check_cells(&cols, &m).is_err());
    }
}
