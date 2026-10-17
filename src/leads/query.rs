//! The filter that the list route, the export and the bulk actions share, and the paging helpers.
use super::model::{ContactState, Interest, Verification};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use sqlx::{PgConnection, Postgres, QueryBuilder};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

#[derive(Deserialize, ToSchema, IntoParams, Default, Clone, Debug)]
pub struct LeadFilter {
    /// Text search in email, first name, last name and company name (part of a word is fine).
    pub q: Option<String>,
    /// Only leads in this list.
    pub list_id: Option<Uuid>,
    /// Only leads in this campaign (any state).
    pub campaign_id: Option<Uuid>,
    /// Only leads with this tag.
    pub tag: Option<String>,
    pub contact_state: Option<ContactState>,
    pub interest: Option<Interest>,
    pub verification: Option<Verification>,
    /// `true`: only suppressed addresses (the address or its domain is blocked). `false`: only the others.
    pub suppressed: Option<bool>,
    /// Created at or after this time.
    pub created_from: Option<DateTime<Utc>>,
    /// Created before this time.
    pub created_to: Option<DateTime<Utc>>,
}

#[derive(Deserialize, IntoParams, Default, Clone, Debug)]
pub struct PageQuery {
    /// The `next_cursor` of the last page. It is the id of the last lead.
    pub cursor: Option<Uuid>,
    /// 1 to 500. Default 50.
    pub limit: Option<i64>,
}

impl LeadFilter {
    /// No condition at all: it would match every lead.
    pub fn is_empty(&self) -> bool {
        self.q.as_deref().is_none_or(|q| q.trim().is_empty())
            && self.list_id.is_none()
            && self.campaign_id.is_none()
            && self.tag.as_deref().is_none_or(|t| t.trim().is_empty())
            && self.contact_state.is_none()
            && self.interest.is_none()
            && self.verification.is_none()
            && self.suppressed.is_none()
            && self.created_from.is_none()
            && self.created_to.is_none()
    }
}

/// `%`, `_` and `\` in the search text are plain characters.
pub fn like_pattern(q: &str) -> String {
    let mut out = String::from("%");
    for c in q.trim().chars() {
        if matches!(c, '%' | '_' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('%');
    out
}

/// Pushes ` WHERE TRUE AND ...` for the filter. The table is `leads` with no alias. The caller can add more `AND`s.
pub fn push_where<'a>(qb: &mut QueryBuilder<'a, Postgres>, f: &'a LeadFilter) {
    qb.push(" WHERE TRUE");
    if let Some(q) = f.q.as_deref().map(str::trim).filter(|q| !q.is_empty()) {
        let pat = like_pattern(q);
        qb.push(" AND (email ILIKE ").push_bind(pat.clone());
        qb.push(" OR first_name ILIKE ").push_bind(pat.clone());
        qb.push(" OR last_name ILIKE ").push_bind(pat.clone());
        qb.push(" OR company_name ILIKE ").push_bind(pat);
        qb.push(")");
    }
    if let Some(list_id) = f.list_id {
        qb.push(" AND id IN (SELECT lead_id FROM lead_list_members WHERE list_id = ").push_bind(list_id).push(")");
    }
    if let Some(campaign_id) = f.campaign_id {
        qb.push(" AND id IN (SELECT lead_id FROM campaign_leads WHERE campaign_id = ").push_bind(campaign_id).push(")");
    }
    if let Some(tag) = f.tag.as_deref().map(str::trim).filter(|t| !t.is_empty()) {
        qb.push(" AND tags @> ARRAY[").push_bind(tag.to_lowercase()).push("]::text[]");
    }
    if let Some(s) = f.contact_state {
        qb.push(" AND contact_state = ").push_bind(s);
    }
    if let Some(i) = f.interest {
        qb.push(" AND interest = ").push_bind(i);
    }
    if let Some(v) = f.verification {
        qb.push(" AND verification = ").push_bind(v);
    }
    if let Some(s) = f.suppressed {
        qb.push(if s { " AND " } else { " AND NOT " });
        qb.push(
            "(EXISTS (SELECT 1 FROM suppressions s WHERE s.kind = 'email' AND s.value = leads.email::citext)
              OR EXISTS (SELECT 1 FROM suppressions s WHERE s.kind = 'domain' AND s.value = leads.email_domain::citext))",
        );
    }
    if let Some(t) = f.created_from {
        qb.push(" AND created_at >= ").push_bind(t);
    }
    if let Some(t) = f.created_to {
        qb.push(" AND created_at < ").push_bind(t);
    }
}

/// Where a cursor lead sits in the order. `None`: the lead is gone.
pub async fn cursor_position(conn: &mut PgConnection, id: Uuid) -> Result<Option<(DateTime<Utc>, Uuid)>, sqlx::Error> {
    sqlx::query_as("SELECT created_at, id FROM leads WHERE id = $1").bind(id).fetch_optional(conn).await
}

/// Ids that match the filter, newest first. Asks for one more than `max`, so the caller can tell "too many".
pub async fn matching_ids(conn: &mut PgConnection, f: &LeadFilter, max: i64) -> Result<Vec<Uuid>, sqlx::Error> {
    let mut qb = QueryBuilder::new("SELECT id FROM leads");
    push_where(&mut qb, f);
    qb.push(" ORDER BY created_at DESC, id DESC LIMIT ").push_bind(max + 1);
    let rows: Vec<(Uuid,)> = qb.build_query_as().fetch_all(conn).await?;
    Ok(rows.into_iter().map(|r| r.0).collect())
}
