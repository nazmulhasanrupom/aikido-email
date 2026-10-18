//! `GET /v1/leads/export.csv`: streamed, in pages of 1000 by keyset, so memory stays small at any size.
use super::fields::FieldDefs;
use super::model::*;
use super::query::{LeadFilter, push_where};
use crate::error::{ApiQuery, ApiResult};
use crate::state::AppState;
use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderValue, header};
use axum::response::Response;
use chrono::{DateTime, Utc};
use futures::stream;
use sqlx::QueryBuilder;
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

const PAGE: i64 = 1000;

pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new().routes(routes!(export_csv))
}

/// A cell that starts with `=`, `+`, `-` or `@` (or a tab or carriage return) could run as a formula in
/// Excel. A `'` in front makes it plain text.
pub fn safe_cell(v: &str) -> String {
    if v.starts_with(['=', '+', '-', '@', '\t', '\r']) { format!("'{v}") } else { v.to_string() }
}

const BASE_COLUMNS: [&str; 23] = [
    "email", "first_name", "last_name", "company_name", "job_title", "phone", "website", "linkedin_url", "location", "timezone", "tags", "contact_state",
    "interest", "verification", "source", "notes", "last_contacted_at", "last_replied_at", "created_at", "industry", "personalization_note", "trigger_event", "trigger_verified",
];

fn row_of(l: &Lead, custom_keys: &[String]) -> Vec<String> {
    let t = |v: &Option<String>| v.clone().unwrap_or_default();
    let d = |v: &Option<DateTime<Utc>>| v.map(|d| d.to_rfc3339()).unwrap_or_default();
    let mut row = vec![
        l.email.clone(),
        t(&l.first_name),
        t(&l.last_name),
        t(&l.company_name),
        t(&l.job_title),
        t(&l.phone),
        t(&l.website),
        t(&l.linkedin_url),
        t(&l.location),
        t(&l.timezone),
        l.tags.join(";"),
        l.contact_state.as_str().to_string(),
        l.interest.map(|i| i.as_str().to_string()).unwrap_or_default(),
        l.verification.as_str().to_string(),
        l.source.as_str().to_string(),
        t(&l.notes),
        d(&l.last_contacted_at),
        d(&l.last_replied_at),
        l.created_at.to_rfc3339(),
        t(&l.industry),
        t(&l.personalization_note),
        t(&l.trigger_event),
        if l.trigger_verified { "yes" } else { "no" }.to_string(),
    ];
    for k in custom_keys {
        row.push(l.custom_fields.get(k).and_then(|v| v.as_str()).unwrap_or_default().to_string());
    }
    row.into_iter().map(|c| safe_cell(&c)).collect()
}

fn to_csv(rows: &[Vec<String>]) -> Vec<u8> {
    let mut w = csv::WriterBuilder::new().from_writer(Vec::new());
    for r in rows {
        let _ = w.write_record(r);
    }
    w.into_inner().unwrap_or_default()
}

struct Cursor {
    st: AppState,
    filter: LeadFilter,
    custom_keys: Vec<String>,
    after: Option<(DateTime<Utc>, Uuid)>,
    header_sent: bool,
    done: bool,
}

/// Export leads as CSV. Same filters as the list.
#[utoipa::path(get, path = "/v1/leads/export.csv", tag = "leads",
    params(LeadFilter),
    responses((status = 200, description = "A CSV file", content_type = "text/csv", body = String)),
    security(("api_key" = ["leads"])),
    description = "Scope: `leads`. Streamed, newest first, no size limit. One column per field, then one per custom field (named by its key). \
        Tags are joined with `;`. A cell that starts with `=`, `+`, `-` or `@` gets a `'` in front, so Excel does not run it as a formula.")]
async fn export_csv(State(st): State<AppState>, ApiQuery(filter): ApiQuery<LeadFilter>) -> ApiResult<Response> {
    let mut conn = st.pool.acquire().await?;
    let defs = FieldDefs::load(&mut conn).await?;
    drop(conn);
    let mut custom_keys: Vec<String> = defs.by_key.into_keys().collect();
    custom_keys.sort();
    let state = Cursor { st, filter, custom_keys, after: None, header_sent: false, done: false };
    let body = Body::from_stream(stream::unfold(state, |mut s| async move {
        if s.done {
            return None;
        }
        let mut out: Vec<Vec<String>> = Vec::new();
        if !s.header_sent {
            s.header_sent = true;
            out.push(BASE_COLUMNS.iter().map(|c| c.to_string()).chain(s.custom_keys.iter().cloned()).collect());
        }
        let mut qb = QueryBuilder::new(format!("SELECT {LEAD_COLS} FROM leads"));
        push_where(&mut qb, &s.filter);
        if let Some((at, id)) = s.after {
            qb.push(" AND (created_at, id) < (").push_bind(at).push(", ").push_bind(id).push(")");
        }
        qb.push(" ORDER BY created_at DESC, id DESC LIMIT ").push_bind(PAGE);
        let page: Result<Vec<Lead>, sqlx::Error> = qb.build_query_as().fetch_all(&s.st.pool).await;
        match page {
            Ok(leads) => {
                if (leads.len() as i64) < PAGE {
                    s.done = true;
                }
                s.after = leads.last().map(|l| (l.created_at, l.id));
                out.extend(leads.iter().map(|l| row_of(l, &s.custom_keys)));
                Some((Ok::<_, std::io::Error>(to_csv(&out)), s))
            }
            Err(e) => {
                tracing::error!(error = %e, "export failed");
                s.done = true;
                Some((Err(std::io::Error::other("database error")), s))
            }
        }
    }));
    let mut resp = Response::new(body);
    resp.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static("text/csv; charset=utf-8"));
    resp.headers_mut().insert(header::CONTENT_DISPOSITION, HeaderValue::from_static("attachment; filename=\"leads.csv\""));
    Ok(resp)
}

#[cfg(test)]
mod tests {
    use super::safe_cell;

    #[test]
    fn formula_cells_are_escaped() {
        assert_eq!(safe_cell("=cmd|' /C calc'!A0"), "'=cmd|' /C calc'!A0");
        for c in ["+1", "-1", "@SUM(A1)", "\tx"] {
            assert!(safe_cell(c).starts_with('\''), "{c}");
        }
        assert_eq!(safe_cell("normal"), "normal");
        assert_eq!(safe_cell(""), "");
    }
}
