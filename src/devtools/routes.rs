//! `/v1/dev/*` (scope `admin`, so owner and admin sessions only): logs, the SQL console, database tables, info and controls.
use crate::auth::api_keys::Scope;
use crate::auth::middleware::require_scope;
use crate::error::{ApiError, ApiJson, ApiPath, ApiQuery, ApiResult};
use crate::state::AppState;
use axum::extract::{Request, State};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use sqlx::{Column, Row};
use utoipa::ToSchema;
use utoipa_axum::{router::OpenApiRouter, routes};

pub fn router(state: AppState) -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(get_logs, clear_logs))
        .routes(routes!(get_log_filter, put_log_filter))
        .routes(routes!(run_sql))
        .routes(routes!(list_tables))
        .routes(routes!(table_columns))
        .routes(routes!(info))
        .routes(routes!(action))
        .route_layer(axum::middleware::from_fn_with_state(state.clone(), dev_tools_on))
        .route_layer(axum::middleware::from_fn_with_state((state, Scope::Admin), require_scope))
}

/// `AIKIDO_DEV_TOOLS=false` turns every route off.
async fn dev_tools_on(State(st): State<AppState>, req: Request, next: Next) -> Response {
    if !st.config.dev_tools {
        return ApiError::forbidden("dev_tools_disabled", "The developer tools are turned off on this server (AIKIDO_DEV_TOOLS=false).").into_response();
    }
    next.run(req).await
}

// ---------- logs ----------

#[derive(Deserialize)]
pub struct LogsQuery {
    /// Only entries with an id after this one. 0 or empty: the newest ones.
    pub after: Option<u64>,
    /// 1 to 2000. Default 500.
    pub limit: Option<usize>,
    /// The lowest level: `debug`, `info`, `warn` or `error`.
    pub level: Option<String>,
    /// Only entries that hold this text (message, fields, target or request).
    pub q: Option<String>,
}

#[derive(Serialize, ToSchema)]
pub struct LogsReply {
    pub entries: Vec<super::logs::LogEntry>,
    /// The id of the newest entry in the ring. Ask for `after=<last_id>` next time.
    pub last_id: u64,
    /// Entries in the ring now.
    pub buffer_size: usize,
    /// The filter of the ring (what is kept).
    pub filter: String,
}

/// The latest log entries.
#[utoipa::path(get, path = "/v1/dev/logs", tag = "dev",
    params(("after" = Option<u64>, Query), ("limit" = Option<usize>, Query), ("level" = Option<String>, Query), ("q" = Option<String>, Query)),
    responses((status = 200, body = LogsReply)), security(("api_key" = ["admin"]), ("session" = [])),
    description = "Scope: `admin` (owner and admin sessions). The last 5000 events of the server, kept in memory (lost at a restart). Poll with `after=<last_id>`. \
        What is kept follows `GET /v1/dev/log-filter`.")]
async fn get_logs(State(st): State<AppState>, ApiQuery(q): ApiQuery<LogsQuery>) -> ApiResult<ApiJson<LogsReply>> {
    let (entries, last_id) = st.dev.logs.since(q.after.unwrap_or(0), q.limit.unwrap_or(500).clamp(1, 2000), q.level.as_deref(), q.q.as_deref());
    Ok(ApiJson(LogsReply { entries, last_id, buffer_size: st.dev.logs.len(), filter: st.dev.filter() }))
}

/// Empty the log ring.
#[utoipa::path(delete, path = "/v1/dev/logs", tag = "dev", responses((status = 200, body = Done)), security(("api_key" = ["admin"]), ("session" = [])),
    description = "Scope: `admin`. Empties the in-memory ring. Ids keep growing.")]
async fn clear_logs(State(st): State<AppState>) -> ApiResult<ApiJson<Done>> {
    st.dev.logs.clear();
    Ok(ApiJson(Done { ok: true, message: "The log is empty.".into() }))
}

#[derive(Serialize, ToSchema)]
pub struct Done {
    pub ok: bool,
    pub message: String,
}

#[derive(Serialize, Deserialize, ToSchema)]
pub struct FilterBody {
    /// A `RUST_LOG` style filter, for example `info`, `debug` or `info,aikido_email=trace,sqlx=debug`.
    pub filter: String,
}

/// What the log ring keeps.
#[utoipa::path(get, path = "/v1/dev/log-filter", tag = "dev", responses((status = 200, body = FilterBody)), security(("api_key" = ["admin"]), ("session" = [])),
    description = "Scope: `admin`. The filter of the ring. It does not change what the terminal shows (`RUST_LOG`).")]
async fn get_log_filter(State(st): State<AppState>) -> ApiResult<ApiJson<FilterBody>> {
    Ok(ApiJson(FilterBody { filter: st.dev.filter() }))
}

/// Change what the log ring keeps, while the server runs.
#[utoipa::path(put, path = "/v1/dev/log-filter", tag = "dev", request_body = FilterBody,
    responses((status = 200, body = FilterBody), (status = 422, description = "`invalid_filter`")), security(("api_key" = ["admin"]), ("session" = [])),
    description = "Scope: `admin`. Takes effect at once. Lost at a restart. More detail (`debug`, `trace`) fills the ring faster.")]
async fn put_log_filter(State(st): State<AppState>, ApiJson(b): ApiJson<FilterBody>) -> ApiResult<ApiJson<FilterBody>> {
    st.dev.set_filter(b.filter.trim()).map_err(|e| ApiError::unprocessable("invalid_filter", e))?;
    tracing::info!(filter = %b.filter.trim(), "developer panel: log filter changed");
    Ok(ApiJson(FilterBody { filter: st.dev.filter() }))
}

// ---------- SQL ----------

#[derive(Deserialize, Serialize, ToSchema, Clone, Copy, PartialEq, Eq, Debug, Default)]
#[serde(rename_all = "snake_case")]
pub enum SqlMode {
    /// A read-only transaction. One statement. Nothing can change.
    #[default]
    Read,
    /// Run it in a transaction and roll it back: see what it would do (rows affected, returned rows).
    Preview,
    /// Run it and save the changes.
    Commit,
}

#[derive(Deserialize, ToSchema)]
pub struct SqlBody {
    pub sql: String,
    #[serde(default)]
    pub mode: SqlMode,
}

#[derive(Serialize, ToSchema)]
pub struct SqlReply {
    pub columns: Vec<String>,
    /// Every value as text (`null` for NULL). At most 1000 rows, each value cut at 5000 characters.
    pub rows: Vec<Vec<Option<String>>>,
    pub row_count: usize,
    pub truncated: bool,
    /// For INSERT, UPDATE and DELETE.
    pub rows_affected: u64,
    pub elapsed_ms: u64,
    pub mode: SqlMode,
    /// The changes were saved.
    pub committed: bool,
}

const MAX_ROWS: usize = 1000;
const MAX_CELL: usize = 5000;

/// A piece of SQL outside of strings and comments.
#[derive(Debug, PartialEq)]
pub enum Token {
    /// An identifier or keyword, lower-cased.
    Word(String),
    /// A `;` between statements.
    Semi,
    /// A string, a quoted name or any other mark.
    Other,
}

/// Split SQL into tokens. Strings (`'..'`, `E'..'`), quoted names, `$tag$..$tag$` strings
/// and comments (`--`, nested `/* */`) are skipped, so a `;` inside them is not seen.
pub fn sql_tokens(sql: &str) -> Vec<Token> {
    let b: Vec<char> = sql.chars().collect();
    let word_start = |c: char| c.is_alphabetic() || c == '_';
    let word_part = |c: char| c.is_alphanumeric() || c == '_' || c == '$';
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c.is_whitespace() {
            i += 1;
        } else if c == '-' && b.get(i + 1) == Some(&'-') {
            while i < b.len() && b[i] != '\n' {
                i += 1;
            }
        } else if c == '/' && b.get(i + 1) == Some(&'*') {
            let mut depth = 1;
            i += 2;
            while i < b.len() && depth > 0 {
                if b[i] == '/' && b.get(i + 1) == Some(&'*') {
                    depth += 1;
                    i += 2;
                } else if b[i] == '*' && b.get(i + 1) == Some(&'/') {
                    depth -= 1;
                    i += 2;
                } else {
                    i += 1;
                }
            }
        } else if c == '\'' || c == '"' {
            i = skip_quoted(&b, i, false);
            out.push(Token::Other);
        } else if c == '$' {
            // A dollar quote needs `$` + an optional tag + `$`. `$1` is a parameter.
            let mut j = i + 1;
            if j < b.len() && word_start(b[j]) && b[j] != '$' {
                while j < b.len() && (b[j].is_alphanumeric() || b[j] == '_') {
                    j += 1;
                }
            }
            if b.get(j) == Some(&'$') {
                let tag = &b[i..=j];
                i = j + 1;
                while i < b.len() && !b[i..].starts_with(tag) {
                    i += 1;
                }
                i = (i + tag.len()).min(b.len());
            } else {
                i += 1;
            }
            out.push(Token::Other);
        } else if word_start(c) {
            let start = i;
            while i < b.len() && word_part(b[i]) {
                i += 1;
            }
            if i - start == 1 && (c == 'e' || c == 'E') && b.get(i) == Some(&'\'') {
                i = skip_quoted(&b, i, true);
                out.push(Token::Other);
            } else {
                out.push(Token::Word(b[start..i].iter().collect::<String>().to_lowercase()));
            }
        } else {
            i += 1;
            out.push(if c == ';' { Token::Semi } else { Token::Other });
        }
    }
    out
}

/// Skip a quoted run that starts at `start`. Returns the index after the closing quote.
/// The quote doubled is an escape. With `backslash`, a backslash also escapes the next char.
fn skip_quoted(b: &[char], start: usize, backslash: bool) -> usize {
    let q = b[start];
    let mut i = start + 1;
    while i < b.len() {
        if backslash && b[i] == '\\' {
            i += 2;
        } else if b[i] == q {
            if b.get(i + 1) == Some(&q) {
                i += 2;
            } else {
                return i + 1;
            }
        } else {
            i += 1;
        }
    }
    b.len()
}

/// Is there more than one statement? Strings, quoted names and comments are skipped.
pub fn is_multi_statement(sql: &str) -> bool {
    let t = sql_tokens(sql);
    t.iter().position(|x| *x == Token::Semi).is_some_and(|p| t[p + 1..].iter().any(|x| *x != Token::Semi))
}

/// `COPY ... PROGRAM` runs a shell command on the database host. Never.
pub fn runs_a_program(sql: &str) -> bool {
    let mut copy = false;
    for t in sql_tokens(sql) {
        match t {
            Token::Semi => copy = false,
            Token::Word(w) if w == "copy" => copy = true,
            Token::Word(w) if copy && w == "program" => return true,
            _ => {}
        }
    }
    false
}

/// Run SQL on the database.
#[utoipa::path(post, path = "/v1/dev/sql", tag = "dev", request_body = SqlBody,
    responses((status = 200, body = SqlReply), (status = 422, description = "`sql_error` (the database message), `empty_sql`, `one_statement_only`, `not_allowed`")),
    security(("api_key" = ["admin"]), ("session" = [])),
    description = "Scope: `admin` (owner and admin sessions). `mode: read` (default) runs one statement in a read-only transaction. `preview` runs it and rolls back. \
        `commit` runs it and saves it. 30 second limit. Every run is written to the log at `warn` level (the first 300 characters). This is raw access to the database: \
        it can break the app. Take a backup first.")]
async fn run_sql(State(st): State<AppState>, ApiJson(b): ApiJson<SqlBody>) -> ApiResult<ApiJson<SqlReply>> {
    let sql = b.sql.trim().to_string();
    if sql.is_empty() {
        return Err(ApiError::unprocessable("empty_sql", "Type a statement."));
    }
    if runs_a_program(&sql) {
        return Err(ApiError::unprocessable("not_allowed", "COPY ... PROGRAM runs a command on the database host. It is not allowed here."));
    }
    if b.mode == SqlMode::Read && is_multi_statement(&sql) {
        return Err(ApiError::unprocessable("one_statement_only", "Read only mode runs one statement. Remove the extra `;` and statements, or use Preview."));
    }
    let preview: String = sql.chars().take(300).collect::<String>().replace('\n', " ");
    tracing::warn!(mode = ?b.mode, sql = %preview, "developer panel: SQL run");
    let started = std::time::Instant::now();
    let mut tx = st.pool.begin().await?;
    let fail = |e: sqlx::Error| -> ApiError {
        let msg = match &e {
            sqlx::Error::Database(d) => d.message().to_string(),
            other => other.to_string(),
        };
        tracing::warn!(error = %msg, "developer panel: SQL failed");
        ApiError::unprocessable("sql_error", msg)
    };
    if b.mode == SqlMode::Read {
        sqlx::query("SET TRANSACTION READ ONLY").execute(&mut *tx).await.map_err(fail)?;
    }
    sqlx::query("SET LOCAL statement_timeout = 30000").execute(&mut *tx).await.map_err(fail)?;

    let mut columns: Vec<String> = Vec::new();
    let mut rows: Vec<Vec<Option<String>>> = Vec::new();
    let (mut row_count, mut rows_affected, mut truncated) = (0usize, 0u64, false);
    {
        // the simple protocol: every value comes back as text, whatever its type
        let mut stream = sqlx::raw_sql(&sql).fetch_many(&mut *tx);
        while let Some(item) = stream.next().await {
            match item.map_err(fail)? {
                sqlx::Either::Left(done) => rows_affected += done.rows_affected(),
                sqlx::Either::Right(row) => {
                    if columns.is_empty() {
                        columns = row.columns().iter().map(|c| c.name().to_string()).collect();
                    }
                    row_count += 1;
                    if rows.len() < MAX_ROWS {
                        let cells = (0..row.columns().len())
                            .map(|i| {
                                row.try_get_unchecked::<Option<String>, _>(i).ok().flatten().map(|mut s| {
                                    if s.chars().count() > MAX_CELL {
                                        s = s.chars().take(MAX_CELL).collect::<String>() + "…";
                                    }
                                    s
                                })
                            })
                            .collect();
                        rows.push(cells);
                    } else {
                        truncated = true;
                        if b.mode != SqlMode::Commit {
                            break;
                        }
                    }
                }
            }
        }
    }
    let committed = b.mode == SqlMode::Commit;
    if committed {
        tx.commit().await.map_err(fail)?;
    } else {
        tx.rollback().await?;
    }
    let elapsed_ms = started.elapsed().as_millis() as u64;
    tracing::info!(rows = row_count, rows_affected, elapsed_ms, committed, "developer panel: SQL done");
    Ok(ApiJson(SqlReply { columns, rows, row_count, truncated, rows_affected, elapsed_ms, mode: b.mode, committed }))
}

// ---------- tables ----------

#[derive(Serialize, ToSchema, sqlx::FromRow)]
pub struct TableInfo {
    pub name: String,
    /// The planner's estimate (-1: never analysed).
    pub estimated_rows: i64,
    pub bytes: i64,
}

#[derive(Serialize, ToSchema)]
pub struct TablesReply {
    pub tables: Vec<TableInfo>,
}

/// The tables of the database.
#[utoipa::path(get, path = "/v1/dev/tables", tag = "dev", responses((status = 200, body = TablesReply)), security(("api_key" = ["admin"]), ("session" = [])),
    description = "Scope: `admin`. The tables of the `public` schema with an estimated row count and the size on disk.")]
async fn list_tables(State(st): State<AppState>) -> ApiResult<ApiJson<TablesReply>> {
    let tables: Vec<TableInfo> = sqlx::query_as(
        "SELECT c.relname::text AS name, c.reltuples::bigint AS estimated_rows, pg_total_relation_size(c.oid)::bigint AS bytes
           FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace WHERE n.nspname = 'public' AND c.relkind = 'r' ORDER BY c.relname",
    )
    .fetch_all(&st.pool)
    .await?;
    Ok(ApiJson(TablesReply { tables }))
}

#[derive(Serialize, ToSchema, sqlx::FromRow)]
pub struct ColumnInfo {
    pub name: String,
    pub data_type: String,
    pub nullable: bool,
}

#[derive(Serialize, ToSchema)]
pub struct ColumnsReply {
    pub columns: Vec<ColumnInfo>,
}

/// The columns of one table.
#[utoipa::path(get, path = "/v1/dev/tables/{name}", tag = "dev", params(("name" = String, Path)),
    responses((status = 200, body = ColumnsReply), (status = 404, description = "No such table")), security(("api_key" = ["admin"]), ("session" = [])),
    description = "Scope: `admin`. Column names, types and whether they can be null.")]
async fn table_columns(State(st): State<AppState>, ApiPath(name): ApiPath<String>) -> ApiResult<ApiJson<ColumnsReply>> {
    let columns: Vec<ColumnInfo> = sqlx::query_as(
        "SELECT column_name::text AS name, data_type::text AS data_type, (is_nullable = 'YES') AS nullable FROM information_schema.columns
          WHERE table_schema = 'public' AND table_name = $1 ORDER BY ordinal_position",
    )
    .bind(&name)
    .fetch_all(&st.pool)
    .await?;
    if columns.is_empty() {
        return Err(ApiError::not_found("table_not_found", "No table with this name."));
    }
    Ok(ApiJson(ColumnsReply { columns }))
}

// ---------- info ----------

#[derive(Serialize, ToSchema)]
pub struct InfoReply {
    pub version: String,
    pub started_at: chrono::DateTime<chrono::Utc>,
    pub uptime_secs: i64,
    pub database_version: String,
    pub database_bytes: i64,
    pub pool_size: u32,
    pub pool_idle: usize,
    /// Settings of the server that hold no secret.
    pub config: serde_json::Value,
    /// Counts that tell what the engines are doing.
    pub counts: serde_json::Value,
    pub log_buffer: usize,
    pub log_filter: String,
}

async fn count(st: &AppState, sql: &str) -> Option<i64> {
    sqlx::query_scalar::<_, i64>(sql).fetch_one(&st.pool).await.ok()
}

/// A summary of the server.
#[utoipa::path(get, path = "/v1/dev/info", tag = "dev", responses((status = 200, body = InfoReply)), security(("api_key" = ["admin"]), ("session" = [])),
    description = "Scope: `admin`. Version, uptime, database size, connection pool, settings without secrets, and counts of queued, stuck and failed work.")]
async fn info(State(st): State<AppState>) -> ApiResult<ApiJson<InfoReply>> {
    let db_version: String = sqlx::query_scalar("SHOW server_version").fetch_one(&st.pool).await?;
    let db_bytes: i64 = sqlx::query_scalar("SELECT pg_database_size(current_database())").fetch_one(&st.pool).await?;
    let c = &st.config;
    let config = serde_json::json!({
        "bind_addr": c.bind_addr, "public_url": c.public_url, "site_url": st.site_url(), "dry_run": c.dry_run, "trust_proxy": c.trust_proxy,
        "insecure_cookies": c.insecure_cookies, "scheduler_tick_secs": c.scheduler_tick_secs, "reply_tick_secs": c.reply_tick_secs,
        "reply_check_enabled": c.reply_check_enabled, "followups_enabled": c.followups_enabled, "leads_enabled": c.leads_enabled, "generator_enabled": c.generator_enabled,
        "tracking": { "enabled": c.tracking.enabled, "cname_target": c.tracking_cname_target(), "https_addr": c.tracking.https_addr, "http_addr": c.tracking.http_addr,
                      "acme_directory": c.tracking.acme_directory },
    });
    let counts = serde_json::json!({
        "jobs_queued": count(&st, "SELECT count(*) FROM send_jobs WHERE status = 'queued'").await,
        "jobs_sending": count(&st, "SELECT count(*) FROM send_jobs WHERE status = 'sending'").await,
        "jobs_stuck": count(&st, "SELECT count(*) FROM send_jobs WHERE status = 'stuck'").await,
        "jobs_failed_24h": count(&st, "SELECT count(*) FROM send_jobs WHERE status = 'failed' AND updated_at > now() - interval '24 hours'").await,
        "jobs_sent_24h": count(&st, "SELECT count(*) FROM send_jobs WHERE status = 'sent' AND sent_at > now() - interval '24 hours'").await,
        "enrollments_open": count(&st, "SELECT count(*) FROM enrollments WHERE status IN ('active', 'waiting_send', 'paused')").await,
        "enrollments_failed": count(&st, "SELECT count(*) FROM enrollments WHERE status = 'failed'").await,
        "accounts": count(&st, "SELECT count(*) FROM email_accounts WHERE deleted_at IS NULL").await,
        "accounts_not_active": count(&st, "SELECT count(*) FROM email_accounts WHERE deleted_at IS NULL AND status <> 'active'").await,
        "leads": count(&st, "SELECT count(*) FROM leads").await,
        "campaigns_active": count(&st, "SELECT count(*) FROM campaigns WHERE status = 'active'").await,
        "opens_24h": count(&st, "SELECT count(*) FROM email_opens WHERE opened_at > now() - interval '24 hours'").await,
        "tracking_domains_not_green": count(&st, "SELECT count(*) FROM tracking_domains WHERE status <> 'connected'").await,
    });
    Ok(ApiJson(InfoReply {
        version: env!("CARGO_PKG_VERSION").into(),
        started_at: st.dev.started,
        uptime_secs: (st.now() - st.dev.started).num_seconds().max(0),
        database_version: db_version,
        database_bytes: db_bytes,
        pool_size: st.pool.size(),
        pool_idle: st.pool.num_idle(),
        config,
        counts,
        log_buffer: st.dev.logs.len(),
        log_filter: st.dev.filter(),
    }))
}

// ---------- controls ----------

/// The controls: `followups_tick`, `feeder_tick`, `sending_tick`, `tracking_tick`, `reload_certs`, `test_log`, `automations_tick`, `automations_trigger_tick`.
pub const ACTIONS: [&str; 8] = ["followups_tick", "feeder_tick", "sending_tick", "tracking_tick", "reload_certs", "test_log", "automations_tick", "automations_trigger_tick"];

/// Run a control now.
#[utoipa::path(post, path = "/v1/dev/actions/{name}", tag = "dev", params(("name" = String, Path, description = "`followups_tick`, `feeder_tick`, `sending_tick`, `tracking_tick`, `reload_certs`, `test_log`, `automations_tick` or `automations_trigger_tick`")),
    responses((status = 200, body = Done), (status = 404, description = "`unknown_action`")), security(("api_key" = ["admin"]), ("session" = [])),
    description = "Scope: `admin`. Runs one round of an engine now, without waiting for its timer: `followups_tick` (make the jobs of due steps), `feeder_tick` (enroll campaign leads), \
        `sending_tick` (send what the rules allow), `tracking_tick` (check tracking domains), `reload_certs` (read the tracking certificates again), `test_log` (write a line of each level), \
        `automations_tick` (claim and run the queued automation runs), `automations_trigger_tick` (fire due time triggers and read new events for event triggers).")]
async fn action(State(st): State<AppState>, ApiPath(name): ApiPath<String>) -> ApiResult<ApiJson<Done>> {
    tracing::info!(action = %name, "developer panel: control run");
    let message = match name.as_str() {
        "followups_tick" => {
            let engine = crate::followups::scheduler::FollowupEngine::new(st.clone());
            format!("{:?}", crate::followups::scheduler::tick(&engine).await?)
        }
        "feeder_tick" => format!("{:?}", crate::campaigns::feeder::tick(&st).await?),
        "sending_tick" => {
            let engine = std::sync::Arc::new(crate::sending::worker::Engine::new(st.clone(), st.senders.clone()));
            format!("{:?}", crate::sending::scheduler::tick(&engine, st.config.max_parallel_accounts).await?)
        }
        "tracking_tick" => format!("{} domain(s) checked", crate::tracking::domains::tick(&st).await?),
        "reload_certs" => format!("{} certificate(s) loaded", st.certs.refresh(&st).await?),
        "automations_tick" => format!("{} run(s) processed", crate::automations::engine::worker::tick(&st, &crate::automations::engine::worker::WorkerOpts::from_env()).await?),
        "automations_trigger_tick" => {
            let (time, event) = crate::automations::production::tick::tick_all(&st).await?;
            format!("time triggers: {time:?}; event triggers: {event:?}")
        }
        "test_log" => {
            tracing::debug!("test line: debug");
            tracing::info!("test line: info");
            tracing::warn!("test line: warn");
            tracing::error!("test line: error");
            "Four lines were written.".to_string()
        }
        _ => return Err(ApiError::not_found("unknown_action", format!("Known controls: {}.", ACTIONS.join(", ")))),
    };
    Ok(ApiJson(Done { ok: true, message }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_statement_is_told_from_many() {
        assert!(!is_multi_statement("SELECT 1"));
        assert!(!is_multi_statement("SELECT 1;"));
        assert!(!is_multi_statement("SELECT 1;  \n -- done"));
        assert!(!is_multi_statement("SELECT ';' , \"a;b\", $$x;y$$ /* ; */"));
        assert!(is_multi_statement("SELECT 1; SELECT 2"));
        assert!(is_multi_statement("SELECT 1;\nDROP TABLE leads"));
    }

    #[test]
    fn copy_program_is_refused() {
        assert!(runs_a_program("COPY t TO PROGRAM 'id'"));
        assert!(runs_a_program("copy t from\nprogram 'x'"));
        assert!(runs_a_program("COPY t TO\tPROGRAM 'id'"));
        assert!(runs_a_program("COPY t TO/**/PROGRAM 'id'"));
        assert!(runs_a_program("copy t to -- x\nprogram 'id'"));
        assert!(!runs_a_program("SELECT program FROM t"));
        assert!(!runs_a_program("SELECT 'copy program'"));
    }

    #[test]
    fn tricky_strings_and_comments_are_understood() {
        assert!(is_multi_statement(r"SELECT E'\''; COMMIT; DROP TABLE x; --'"));
        assert!(!is_multi_statement(r"SELECT E'it\'s; ok'"));
        assert!(!is_multi_statement("DO $fn$a;b$fn$"));
        assert!(!is_multi_statement("SELECT $$a;b$$"));
        assert!(!is_multi_statement("SELECT 1 /* /* ; */ ; */"));
        assert!(!is_multi_statement(r#"SELECT "a;b""#));
        assert!(!is_multi_statement("SELECT $1;  -- end"));
        assert!(!is_multi_statement("SELECT 1; /* end */ "));
    }
}
