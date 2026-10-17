//! Auto Database part 1: AI column mapping for CSV import. Build map: "Auto Database Build Map, part 1".
//!
//! The import file is stored with status `uploaded`. The server reads the headers and a few values per column
//! (`samples.rs`), maps the sure columns by rules (`rules.rs`), asks a model about the rest (`ai.rs`), and returns
//! one proposal. Nothing goes into `leads` until the caller confirms (`routes.rs`). The import worker is unchanged.
//!
//! The mappers take a `SampleSet`, not a CSV. A later database connector makes the same input.
pub mod ai;
pub mod proposal;
pub mod routes;
pub mod rules;
pub mod samples;
pub mod targets;
pub mod templates;

use crate::error::{ApiError, ApiResult};
use crate::generator::{draft, repo as gen_repo};
use crate::leads::fields::FieldDefs;
use crate::leads::routes::load_settings;
use crate::state::AppState;
use proposal::*;
use samples::{ColumnSample, MAX_AI_COLUMNS, SampleSet};
use std::collections::HashSet;
use uuid::Uuid;

fn column(c: &ColumnSample) -> ColumnProposal {
    ColumnProposal {
        index: c.index,
        header: c.header.clone(),
        samples: c.samples.clone(),
        empty_count: c.empty_count,
        target: MapTarget::Skip,
        transform: None,
        source: "rule".into(),
        confidence: 0.0,
        reason: "no match".into(),
    }
}

/// A name can go to one column only (`tags` is shared). The column with the higher confidence keeps it.
pub fn dedupe(cols: &mut [ColumnProposal], warnings: &mut Vec<String>) {
    let mut order: Vec<usize> = (0..cols.len()).collect();
    order.sort_by(|a, b| cols[*b].confidence.partial_cmp(&cols[*a].confidence).unwrap_or(std::cmp::Ordering::Equal).then(a.cmp(b)));
    let mut claimed: HashSet<String> = HashSet::new();
    for i in order {
        let slots = cols[i].target.slots(cols[i].transform.as_deref());
        if slots.iter().any(|s| claimed.contains(s)) {
            warnings.push(format!("duplicate_target:{}", cols[i].index));
            cols[i].target = MapTarget::Skip;
            cols[i].transform = None;
            cols[i].reason = "another column has this target".into();
        } else {
            claimed.extend(slots);
        }
    }
}

/// The AI step. Any problem becomes a warning; the rules result stays.
async fn run_ai(st: &AppState, defs: &FieldDefs, provider_id: Option<Uuid>, masked: &SampleSet, ask: &[usize], warnings: &mut Vec<String>) -> Option<(ai::AiRun, Uuid)> {
    let mut conn = st.pool.acquire().await.ok()?;
    let gs = gen_repo::load_settings(&mut conn).await.ok()?;
    drop(conn);
    let Some(pid) = provider_id.or(gs.slot_provider_id).or(gs.draft_provider_id) else {
        warnings.push("no_ai_provider".into());
        return None;
    };
    let Ok(ready) = draft::ready_provider(st, Some(pid), "mapping provider", gs.max_attempts).await else {
        warnings.push("ai_mapping_failed".into());
        return None;
    };
    if draft::check_daily_budget(st, &gs).await.is_err() {
        warnings.push("ai_budget_reached".into());
        return None;
    }
    let cols: Vec<&ColumnSample> = ask.iter().filter_map(|i| masked.columns.get(*i)).collect();
    match ai::run(st, ready.client.as_ref(), &ready.cfg, &cols, defs).await {
        Ok(r) => Some((r, pid)),
        Err(f) => {
            if f == ai::AiFail::Auth {
                crate::generator::providers::mark_auth_failed(&st.pool, pid, st.now()).await;
            }
            warnings.push("ai_mapping_failed".into());
            None
        }
    }
}

/// Build the proposal for a stored import and save it in `lead_imports.proposal`.
/// `allow_template`: use a saved mapping of the same headers (no AI call). "Map again" passes `false`.
pub async fn propose(st: &AppState, import_id: Uuid, data: &[u8], allow_template: bool) -> ApiResult<MappingProposal> {
    let settings = load_settings(&st.pool).await?;
    let defs = {
        let mut conn = st.pool.acquire().await?;
        FieldDefs::load(&mut conn).await?
    };
    let raw = SampleSet::from_csv(data, settings.mapping_sample_rows as usize).map_err(|m| ApiError::unprocessable("invalid_csv", m))?;
    let shown = if settings.mapping_mask_pii { raw.masked() } else { raw.clone() };
    let headers: Vec<String> = raw.columns.iter().map(|c| c.header.clone()).collect();
    let mut cols: Vec<ColumnProposal> = shown.columns.iter().map(column).collect();
    let mut warnings: Vec<String> = Vec::new();
    let mut template_used = None;
    let mut ai_info = AiInfo::default();

    if allow_template && settings.mapping_templates_enabled {
        let mut conn = st.pool.acquire().await?;
        if let Some((id, saved)) = templates::find(&mut conn, &templates::header_hash(&headers)).await? {
            if let Some(fitted) = templates::fit(saved, &defs, cols.len()) {
                for t in fitted {
                    let c = &mut cols[t.index];
                    c.target = t.target;
                    c.transform = t.transform;
                    c.source = "template".into();
                    c.confidence = 1.0;
                    c.reason = "saved mapping".into();
                }
                templates::touch(&mut conn, id, st.now()).await?;
                template_used = Some(id);
            }
        }
    }
    if template_used.is_none() {
        let hits = rules::map_columns(&raw, &defs);
        for (c, hit) in cols.iter_mut().zip(hits) {
            if let Some(h) = hit {
                c.target = h.target;
                c.source = "rule".into();
                c.confidence = 1.0;
                c.reason = h.reason.into();
            }
        }
        let open: Vec<usize> = cols.iter().filter(|c| c.confidence == 0.0).map(|c| c.index).collect();
        let ask: Vec<usize> = open.iter().copied().filter(|i| *i < MAX_AI_COLUMNS).collect();
        if open.len() > ask.len() {
            warnings.push("columns_over_100".into());
        }
        if !ask.is_empty() && settings.ai_mapping_enabled {
            if let Some((run, pid)) = run_ai(st, &defs, settings.ai_mapping_provider_id, &shown, &ask, &mut warnings).await {
                ai_info = AiInfo { used: true, provider_id: Some(pid), input_tokens: run.input_tokens, output_tokens: run.output_tokens };
                warnings.extend(run.warnings);
                for a in run.columns {
                    let c = &mut cols[a.index];
                    c.target = a.target;
                    c.transform = a.transform;
                    c.source = "ai".into();
                    c.confidence = a.confidence;
                    c.reason = a.reason;
                }
            }
        }
    }
    dedupe(&mut cols, &mut warnings);
    if !cols.iter().any(|c| c.target.is_email()) {
        warnings.push("no_email_column".into());
    }
    let needs_check = cols.iter().filter(|c| c.confidence < settings.mapping_min_confidence).map(|c| c.index).collect();
    let proposal = MappingProposal { import_id, columns: cols, needs_check, warnings, template_used, ai: ai_info };
    let saved = sqlx::query("UPDATE lead_imports SET proposal = $2 WHERE id = $1 AND status = 'uploaded'")
        .bind(import_id)
        .bind(serde_json::to_value(&proposal).unwrap_or_default())
        .execute(&st.pool)
        .await?;
    if saved.rows_affected() == 0 {
        return Err(ApiError::conflict("import_not_uploaded", "The import is not waiting for a mapping."));
    }
    Ok(proposal)
}
