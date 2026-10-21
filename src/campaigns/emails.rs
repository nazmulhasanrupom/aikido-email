//! Emails saved per lead and campaign (UI update map v2, section 4). One row in `lead_emails` per lead and step.
//!
//! Template mode: `write` renders the step templates with the lead values and saves the text (Save).
//! The sender is not known yet (the feeder picks the account later), so `{{sender_name}}` and `{{sender_email}}`
//! stay in the saved text and `fill_sender` fills them at send time.
//!
//! Staleness (decision D7): the saved text stays. `input_hash` is a hash of the template, the footer and the
//! rendered text. When the lead or the template changes, `mark_stale` renders again and marks rows whose hash differs.
use super::model::Campaign;
use crate::followups::model::{SequenceStep, STEP_COLS};
use crate::followups::template::{Escape, StepTemplates, Template, Vars, build_vars};
use crate::leads::fields::FieldDefs;
use crate::leads::model::{LEAD_COLS, Lead};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{FromRow, PgConnection};
use std::collections::HashMap;
use utoipa::ToSchema;
use uuid::Uuid;

pub const EMAIL_COLS: &str = "id, campaign_id, lead_id, step_number, variant_key, subject, body_text, body_html, source, status, edited_by_hand, \
    input_hash, error_code, tokens_in, tokens_out, job_id, run_id, created_at, updated_at";

#[derive(FromRow, Serialize, Clone, Debug, ToSchema)]
pub struct LeadEmail {
    pub id: Uuid,
    pub campaign_id: Uuid,
    pub lead_id: Uuid,
    /// 1 is the primary email. 2 and later are the follow ups.
    pub step_number: i32,
    pub variant_key: Option<String>,
    pub subject: String,
    pub body_text: Option<String>,
    pub body_html: Option<String>,
    /// `template`, `ai` or `manual`.
    pub source: String,
    /// `none`, `writing`, `review`, `ready`, `stale`, `failed` or `sent`.
    pub status: String,
    pub edited_by_hand: bool,
    #[serde(skip)]
    pub input_hash: Option<String>,
    pub error_code: Option<String>,
    pub tokens_in: i32,
    pub tokens_out: i32,
    pub job_id: Option<Uuid>,
    pub run_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(FromRow, Serialize, Clone, Debug, ToSchema)]
pub struct StepVariant {
    #[serde(skip)]
    pub id: Uuid,
    #[serde(skip)]
    pub sequence_id: Uuid,
    /// `B` to `E`. Variant `A` is step 1 itself.
    pub key: String,
    pub name: String,
    pub subject: String,
    pub body_text: Option<String>,
    pub body_html: Option<String>,
    #[schema(value_type = Vec<crate::generator::slots::AiSlot>)]
    pub ai_slots: Value,
    pub active: bool,
    pub position: i32,
}

pub const VARIANT_COLS: &str = "id, sequence_id, key, name, subject, body_text, body_html, ai_slots, active, position";

/// What `write` needs, read once.
pub struct Ctx {
    pub campaign: Campaign,
    pub steps: Vec<SequenceStep>,
    pub variants: Vec<StepVariant>,
    pub defs: FieldDefs,
    /// Variant A (step 1 itself) takes new leads.
    pub variant_a_active: bool,
    footer: Option<Template>,
}

pub async fn load_ctx(conn: &mut PgConnection, campaign_id: Uuid) -> Result<Option<Ctx>, sqlx::Error> {
    let Some(campaign) = super::routes::get(conn, campaign_id).await? else { return Ok(None) };
    let steps: Vec<SequenceStep> =
        sqlx::query_as(&format!("SELECT {STEP_COLS} FROM sequence_steps WHERE sequence_id = $1 ORDER BY step_number")).bind(campaign.sequence_id).fetch_all(&mut *conn).await?;
    let variants: Vec<StepVariant> = sqlx::query_as(&format!("SELECT {VARIANT_COLS} FROM step_variants WHERE sequence_id = $1 ORDER BY position, key"))
        .bind(campaign.sequence_id)
        .fetch_all(&mut *conn)
        .await?;
    let defs = FieldDefs::load(conn).await?;
    let footer = campaign.footer.as_deref().filter(|f| !f.trim().is_empty()).and_then(|f| Template::parse(f).ok());
    let (variant_a_active,): (bool,) = sqlx::query_as("SELECT variant_a_active FROM sequences WHERE id = $1").bind(campaign.sequence_id).fetch_one(&mut *conn).await?;
    Ok(Some(Ctx { campaign, steps, variants, defs, variant_a_active, footer }))
}

/// The keys that get new leads: `A` plus the active variants, in order.
pub fn active_variants(ctx: &Ctx) -> Vec<String> {
    let mut keys: Vec<String> = Vec::new();
    if ctx.variant_a_active {
        keys.push("A".to_string());
    }
    keys.extend(ctx.variants.iter().filter(|v| v.active).map(|v| v.key.clone()));
    if keys.is_empty() {
        keys.push("A".to_string());
    }
    keys
}

/// Variant for the lead at this position (0-based, in the order the leads were added): an even split.
pub fn variant_for(ctx: &Ctx, ordinal: i64) -> String {
    let keys = active_variants(ctx);
    keys[(ordinal.max(0) as usize) % keys.len()].clone()
}

struct Source<'a> {
    subject: &'a str,
    body_text: Option<&'a str>,
    body_html: Option<&'a str>,
}

fn source<'a>(ctx: &'a Ctx, step: &'a SequenceStep, variant: &str) -> Source<'a> {
    if step.step_number == 1 && variant != "A" {
        if let Some(v) = ctx.variants.iter().find(|v| v.key == variant) {
            return Source { subject: &v.subject, body_text: v.body_text.as_deref(), body_html: v.body_html.as_deref() };
        }
    }
    Source { subject: &step.subject, body_text: step.body_text.as_deref(), body_html: step.body_html.as_deref() }
}

/// A step whose text has an AI part is not saved by Save: the Email Generator writes it when the step is due.
pub fn has_ai(ctx: &Ctx, step: &SequenceStep, variant: &str) -> bool {
    let s = source(ctx, step, variant);
    StepTemplates::parse(s.subject, s.body_text, s.body_html).map(|t| !t.ai_keys().is_empty()).unwrap_or(false)
}

pub struct Built {
    pub subject: String,
    pub body_text: Option<String>,
    pub body_html: Option<String>,
    pub hash: String,
}

fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&#39;")
}

/// Fill the sender tokens that Save left in the text.
pub fn fill_sender(text: &str, name: &str, email: &str, html: bool) -> String {
    let (n, e) = if html { (escape_html(name), escape_html(email)) } else { (name.to_string(), email.to_string()) };
    text.replace("{{sender_name}}", &n).replace("{{sender_email}}", &e)
}

pub(crate) fn lead_vars(ctx: &Ctx, lead: &Lead) -> Result<Vars, String> {
    let live = crate::leads::vars::render_variables(&json!({}), Some(lead), &ctx.defs);
    build_vars(&live, &lead.email, "{{sender_name}}", "{{sender_email}}")
}

/// Render one email of one lead. `Err` lists the missing names.
pub fn build(ctx: &Ctx, lead: &Lead, step: &SequenceStep, variant: &str) -> Result<Built, Vec<String>> {
    let s = source(ctx, step, variant);
    let templates = StepTemplates::parse(s.subject, s.body_text, s.body_html).map_err(|e| vec![format!("invalid_template: {}", e.message)])?;
    let vars = lead_vars(ctx, lead).map_err(|m| vec![m])?;
    let mut r = templates.render(&vars)?;
    if let Some(f) = &ctx.footer {
        let text = f.render(&vars, Escape::Text)?;
        if let Some(b) = r.body_text.as_mut() {
            b.push_str("\n\n");
            b.push_str(&text);
        }
        if let Some(b) = r.body_html.as_mut() {
            let html = f.render(&vars, Escape::Html)?;
            b.push_str(&format!("<p>{}</p>", html.replace('\n', "<br>")));
        }
    }
    let mut h = Sha256::new();
    for part in [s.subject, s.body_text.unwrap_or(""), s.body_html.unwrap_or(""), ctx.campaign.footer.as_deref().unwrap_or(""), variant] {
        h.update(part.as_bytes());
        h.update([0]);
    }
    for part in [r.subject.as_str(), r.body_text.as_deref().unwrap_or(""), r.body_html.as_deref().unwrap_or("")] {
        h.update(part.as_bytes());
        h.update([0]);
    }
    let hash = h.finalize().iter().map(|b| format!("{b:02x}")).collect::<String>();
    Ok(Built { subject: r.subject, body_text: r.body_text, body_html: r.body_html, hash })
}

// ---------- the plan: which leads and steps ----------

/// A lead of the campaign that can still get emails.
pub struct Eligible {
    pub lead_id: Uuid,
    /// Position among the leads of the campaign, in the order they were added.
    pub ordinal: i64,
    /// The first step that has not been made yet. 1 when the lead is not enrolled.
    pub first_open_step: i32,
}

pub async fn eligible(conn: &mut PgConnection, campaign_id: Uuid) -> Result<Vec<Eligible>, sqlx::Error> {
    let rows: Vec<(Uuid, i64, Option<i32>, Option<String>)> = sqlx::query_as(
        "SELECT cl.lead_id, row_number() OVER (ORDER BY cl.added_at, cl.lead_id) - 1, e.next_step, e.status::text
           FROM campaign_leads cl LEFT JOIN enrollments e ON e.id = cl.enrollment_id
          WHERE cl.campaign_id = $1 AND cl.state <> 'skipped'
          ORDER BY cl.added_at, cl.lead_id",
    )
    .bind(campaign_id)
    .fetch_all(conn)
    .await?;
    Ok(rows
        .into_iter()
        .filter_map(|(lead_id, ordinal, next, status)| match status.as_deref() {
            Some("stopped" | "failed" | "finished") => None,
            // The queued step has a job already.
            Some("waiting_send") => Some(Eligible { lead_id, ordinal, first_open_step: next.unwrap_or(1) + 1 }),
            _ => Some(Eligible { lead_id, ordinal, first_open_step: next.unwrap_or(1) }),
        })
        .collect())
}

/// How `write` treats an email that exists.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize, ToSchema, Default)]
#[serde(rename_all = "snake_case")]
pub enum Overwrite {
    /// Keep every email that is written.
    Skip,
    /// Write again, except what was edited by hand. The default of Save.
    #[default]
    KeepEdited,
    /// Write all again.
    All,
}

impl Overwrite {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Skip => "skip",
            Self::KeepEdited => "keep_edited",
            Self::All => "all",
        }
    }
}

#[derive(Default, Serialize, ToSchema, Debug, PartialEq)]
pub struct Plan {
    /// Emails that would be written (or are written).
    pub emails: i64,
    /// Emails edited by hand that `overwrite: all` would replace.
    pub edited_by_hand: i64,
    /// Steps with an AI part. Save leaves them to the generator.
    pub skipped_ai_steps: Vec<i32>,
}

pub(crate) type Existing = HashMap<(Uuid, i32), (String, bool, Option<String>)>;

pub(crate) async fn existing(conn: &mut PgConnection, campaign_id: Uuid) -> Result<Existing, sqlx::Error> {
    let rows: Vec<(Uuid, i32, String, bool, Option<String>)> =
        sqlx::query_as("SELECT lead_id, step_number, status, edited_by_hand, variant_key FROM lead_emails WHERE campaign_id = $1").bind(campaign_id).fetch_all(conn).await?;
    Ok(rows.into_iter().map(|(l, s, st, ed, v)| ((l, s), (st, ed, v))).collect())
}

/// What happens to an email that exists: `true` = write it again.
pub(crate) fn rewrite(existing: Option<&(String, bool, Option<String>)>, overwrite: Overwrite) -> bool {
    match existing {
        None => true,
        Some((status, edited, _)) => {
            if status == "sent" || (status == "writing" && overwrite != Overwrite::All) {
                return false;
            }
            match overwrite {
                Overwrite::All => true,
                Overwrite::KeepEdited => !*edited,
                Overwrite::Skip => matches!(status.as_str(), "none" | "failed"),
            }
        }
    }
}

pub async fn plan(conn: &mut PgConnection, ctx: &Ctx, overwrite: Overwrite, only: Option<&[Uuid]>) -> Result<Plan, sqlx::Error> {
    let leads = eligible(conn, ctx.campaign.id).await?;
    let have = existing(conn, ctx.campaign.id).await?;
    let mut plan = Plan::default();
    for step in &ctx.steps {
        if has_ai(ctx, step, "A") && !plan.skipped_ai_steps.contains(&step.step_number) {
            plan.skipped_ai_steps.push(step.step_number);
        }
    }
    for l in leads.iter().filter(|l| only.is_none_or(|o| o.contains(&l.lead_id))) {
        for step in ctx.steps.iter().filter(|s| s.step_number >= l.first_open_step) {
            if plan.skipped_ai_steps.contains(&step.step_number) {
                continue;
            }
            let cur = have.get(&(l.lead_id, step.step_number));
            if rewrite(cur, overwrite) {
                plan.emails += 1;
            }
            if let Some((_, true, _)) = cur {
                plan.edited_by_hand += 1;
            }
        }
    }
    Ok(plan)
}

#[derive(Default, Debug)]
pub struct Written {
    pub done: i64,
    pub failed: i64,
}

async fn save(conn: &mut PgConnection, ctx: &Ctx, lead: &Lead, step: &SequenceStep, variant: &str, run_id: Option<Uuid>, now: DateTime<Utc>) -> Result<bool, sqlx::Error> {
    let (status, error, built) = match build(ctx, lead, step, variant) {
        Ok(b) => ("ready", None, Some(b)),
        Err(missing) => ("failed", Some(format!("missing_variable:{}", missing.join(","))), None),
    };
    let ok = built.is_some();
    let (subject, bt, bh, hash) = match built {
        Some(b) => (b.subject, b.body_text, b.body_html, Some(b.hash)),
        None => (String::new(), None, None, None),
    };
    sqlx::query(
        "INSERT INTO lead_emails (id, campaign_id, lead_id, step_number, variant_key, subject, body_text, body_html, source, status, edited_by_hand, input_hash, error_code, run_id, created_at, updated_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, 'template', $9, false, $10, $11, $12, $13, $13)
         ON CONFLICT (campaign_id, lead_id, step_number) DO UPDATE SET variant_key = EXCLUDED.variant_key, subject = EXCLUDED.subject,
            body_text = EXCLUDED.body_text, body_html = EXCLUDED.body_html, source = 'template', status = EXCLUDED.status, edited_by_hand = false,
            input_hash = EXCLUDED.input_hash, error_code = EXCLUDED.error_code, run_id = EXCLUDED.run_id, updated_at = EXCLUDED.updated_at
         WHERE lead_emails.status <> 'sent'",
    )
    .bind(Uuid::now_v7())
    .bind(ctx.campaign.id)
    .bind(lead.id)
    .bind(step.step_number)
    .bind(if step.step_number == 1 { Some(variant) } else { None })
    .bind(subject)
    .bind(bt)
    .bind(bh)
    .bind(status)
    .bind(hash)
    .bind(error)
    .bind(run_id)
    .bind(now)
    .execute(conn)
    .await?;
    Ok(ok)
}

pub(crate) async fn load_leads(conn: &mut PgConnection, ids: &[Uuid]) -> Result<HashMap<Uuid, Lead>, sqlx::Error> {
    let rows: Vec<Lead> = sqlx::query_as(&format!("SELECT {LEAD_COLS} FROM leads WHERE id = ANY($1)")).bind(ids).fetch_all(conn).await?;
    Ok(rows.into_iter().map(|l| (l.id, l)).collect())
}

/// Write the emails of the campaign (template mode). `only` limits it to some leads.
pub async fn write(conn: &mut PgConnection, ctx: &Ctx, overwrite: Overwrite, only: Option<&[Uuid]>, run_id: Option<Uuid>, now: DateTime<Utc>) -> Result<Written, sqlx::Error> {
    let leads = eligible(conn, ctx.campaign.id).await?;
    let have = existing(conn, ctx.campaign.id).await?;
    let mut out = Written::default();
    let wanted: Vec<&Eligible> = leads.iter().filter(|l| only.is_none_or(|o| o.contains(&l.lead_id))).collect();
    for chunk in wanted.chunks(500) {
        let work = chunk.iter().any(|l| ctx.steps.iter().any(|s| s.step_number >= l.first_open_step && rewrite(have.get(&(l.lead_id, s.step_number)), overwrite)));
        if !work {
            continue;
        }
        let ids: Vec<Uuid> = chunk.iter().map(|l| l.lead_id).collect();
        let map = load_leads(conn, &ids).await?;
        for l in chunk {
            let Some(lead) = map.get(&l.lead_id) else { continue };
            for step in ctx.steps.iter().filter(|s| s.step_number >= l.first_open_step) {
                let cur = have.get(&(l.lead_id, step.step_number));
                if !rewrite(cur, overwrite) {
                    continue;
                }
                // Step 1 keeps the variant of a row that is kept by hand; everything else follows the even split.
                let variant = if step.step_number == 1 { variant_for(ctx, l.ordinal) } else { "A".to_string() };
                if has_ai(ctx, step, &variant) {
                    continue;
                }
                if save(conn, ctx, lead, step, &variant, run_id, now).await? {
                    out.done += 1;
                } else {
                    out.failed += 1;
                }
            }
        }
    }
    sqlx::query("UPDATE campaigns SET emails_saved_at = $2 WHERE id = $1").bind(ctx.campaign.id).bind(now).execute(conn).await?;
    Ok(out)
}

// ---------- staleness ----------

/// Mark unsent emails whose inputs changed as `stale`. `lead_id` limits it to one lead (a lead edit);
/// `None` checks the whole campaign (a template change).
pub async fn mark_stale(conn: &mut PgConnection, campaign_id: Uuid, lead_id: Option<Uuid>) -> Result<u64, sqlx::Error> {
    let Some(ctx) = load_ctx(conn, campaign_id).await? else { return Ok(0) };
    let rows: Vec<LeadEmail> = sqlx::query_as(&format!(
        "SELECT {EMAIL_COLS} FROM lead_emails WHERE campaign_id = $1 AND source IN ('template', 'ai') AND status IN ('ready', 'review') AND ($2::uuid IS NULL OR lead_id = $2)"
    ))
    .bind(campaign_id)
    .bind(lead_id)
    .fetch_all(&mut *conn)
    .await?;
    let prompts = if rows.iter().any(|r| r.source == "ai") { Some(super::email_ai::load_prompts(conn, &ctx).await?) } else { None };
    let mut n = 0;
    for chunk in rows.chunks(500) {
        let ids: Vec<Uuid> = chunk.iter().map(|r| r.lead_id).collect();
        let leads = load_leads(conn, &ids).await?;
        for r in chunk {
            let (Some(lead), Some(step)) = (leads.get(&r.lead_id), ctx.steps.iter().find(|s| s.step_number == r.step_number)) else { continue };
            let variant = r.variant_key.as_deref().unwrap_or("A");
            let now_hash = match (r.source.as_str(), &prompts) {
                ("ai", Some(p)) => super::email_ai::prompt_hash(&ctx, p, lead, r.step_number),
                _ => build(&ctx, lead, step, variant).ok().map(|b| b.hash),
            };
            if now_hash != r.input_hash {
                sqlx::query("UPDATE lead_emails SET status = 'stale', updated_at = now() WHERE id = $1 AND status IN ('ready', 'review')").bind(r.id).execute(&mut *conn).await?;
                n += 1;
            }
        }
    }
    Ok(n)
}

/// A lead changed: check its emails in every campaign it has emails in.
pub async fn lead_changed(conn: &mut PgConnection, lead_id: Uuid) -> Result<(), sqlx::Error> {
    let ids: Vec<(Uuid,)> = sqlx::query_as("SELECT DISTINCT campaign_id FROM lead_emails WHERE lead_id = $1 AND status IN ('ready', 'review')").bind(lead_id).fetch_all(&mut *conn).await?;
    for (c,) in ids {
        mark_stale(conn, c, Some(lead_id)).await?;
    }
    Ok(())
}

/// The sequence of a campaign changed (steps, variants or footer).
pub async fn sequence_changed(conn: &mut PgConnection, sequence_id: Uuid) -> Result<(), sqlx::Error> {
    let ids: Vec<(Uuid,)> = sqlx::query_as("SELECT id FROM campaigns WHERE sequence_id = $1").bind(sequence_id).fetch_all(&mut *conn).await?;
    for (c,) in ids {
        mark_stale(conn, c, None).await?;
    }
    Ok(())
}

// ---------- send time ----------

/// What the scheduler does with the saved email of a step.
pub enum ForSend {
    /// Use this text (the sender tokens are not filled yet).
    Use(Built),
    /// The step is held. The code goes to `enrollments.wait_reason`.
    Hold(&'static str),
    /// Render fails: names that have no value.
    Fail(String),
    /// Not handled here: use the old path (the step has an AI part, or there is no campaign).
    Old,
}

/// Find the email to send. Template mode renders and saves one that is missing; a stale one that was not edited by hand is
/// rendered again. AI mode holds the step until the email is ready.
pub async fn for_send(conn: &mut PgConnection, campaign_id: Uuid, lead: &Lead, step: &SequenceStep, now: DateTime<Utc>) -> Result<ForSend, sqlx::Error> {
    let Some(ctx) = load_ctx(conn, campaign_id).await? else { return Ok(ForSend::Old) };
    let row: Option<LeadEmail> = sqlx::query_as(&format!("SELECT {EMAIL_COLS} FROM lead_emails WHERE campaign_id = $1 AND lead_id = $2 AND step_number = $3 FOR UPDATE"))
        .bind(campaign_id)
        .bind(lead.id)
        .bind(step.step_number)
        .fetch_optional(&mut *conn)
        .await?;
    let ai_mode = ctx.campaign.is_ai();
    let usable = |r: &LeadEmail| r.status == "ready" || r.status == "sent" || (r.status == "stale" && (ai_mode || r.edited_by_hand || r.source != "template"));
    if let Some(r) = row.as_ref().filter(|r| usable(r)) {
        return Ok(ForSend::Use(Built { subject: r.subject.clone(), body_text: r.body_text.clone(), body_html: r.body_html.clone(), hash: r.input_hash.clone().unwrap_or_default() }));
    }
    if ai_mode {
        return Ok(ForSend::Hold(match row.as_ref().map(|r| r.status.as_str()) {
            Some("review") => "email_review",
            _ => "email_not_ready",
        }));
    }
    // Template mode: render now.
    let variant = match row.as_ref().and_then(|r| r.variant_key.clone()) {
        Some(v) if step.step_number == 1 => v,
        _ if step.step_number == 1 => {
            let (ordinal,): (i64,) = sqlx::query_as(
                "SELECT count(*) FROM campaign_leads c, campaign_leads me WHERE c.campaign_id = $1 AND me.campaign_id = $1 AND me.lead_id = $2
                   AND c.state <> 'skipped' AND (c.added_at, c.lead_id) < (me.added_at, me.lead_id)",
            )
            .bind(campaign_id)
            .bind(lead.id)
            .fetch_one(&mut *conn)
            .await?;
            variant_for(&ctx, ordinal)
        }
        _ => "A".to_string(),
    };
    if has_ai(&ctx, step, &variant) {
        return Ok(ForSend::Old);
    }
    match build(&ctx, lead, step, &variant) {
        Ok(b) => {
            save(conn, &ctx, lead, step, &variant, None, now).await?;
            Ok(ForSend::Use(b))
        }
        Err(missing) => Ok(ForSend::Fail(format!("step {} needs: {}", step.step_number, missing.join(", ")))),
    }
}

/// The email left as a job: lock it.
pub async fn mark_sent(conn: &mut PgConnection, campaign_id: Uuid, lead_id: Uuid, step_number: i32, job_id: Uuid, now: DateTime<Utc>) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE lead_emails SET status = 'sent', job_id = $4, updated_at = $5 WHERE campaign_id = $1 AND lead_id = $2 AND step_number = $3 AND status <> 'sent'")
        .bind(campaign_id)
        .bind(lead_id)
        .bind(step_number)
        .bind(job_id)
        .bind(now)
        .execute(conn)
        .await?;
    Ok(())
}

// ---------- values the emails need (I4 and I2) ----------

/// Every name the emails of a campaign use without a fallback, except the built-in and prompt-only names.
pub fn required_names(ctx: &Ctx, prompts: Option<&super::email_ai::Prompts>) -> Vec<String> {
    use crate::followups::template::BUILTINS;
    let mut names: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let mut step_texts: Vec<(String, Option<String>, Option<String>)> = ctx.steps.iter().map(|s| (s.subject.clone(), s.body_text.clone(), s.body_html.clone())).collect();
    step_texts.extend(ctx.variants.iter().map(|v| (v.subject.clone(), v.body_text.clone(), v.body_html.clone())));
    if !ctx.campaign.is_ai() {
        for (sub, text, html) in &step_texts {
            if let Ok(t) = StepTemplates::parse(sub, text.as_deref(), html.as_deref()) {
                names.extend(t.required());
            }
        }
    }
    if let Some(f) = ctx.campaign.footer.as_deref() {
        if let Ok(t) = Template::parse(f) {
            names.extend(t.required().into_iter().map(String::from));
        }
    }
    if let (Some(p), true) = (prompts, ctx.campaign.content_mode == "ai") {
        for text in p.system.iter().chain(p.users.values()) {
            if let Ok(t) = Template::parse(text) {
                names.extend(t.required().into_iter().map(String::from));
            }
        }
    }
    names.into_iter().filter(|n| !BUILTINS.contains(&n.as_str()) && !super::email_ai::PROMPT_VARS.contains(&n.as_str()) && !super::email_ai::is_step_tag(n)).collect()
}

/// The names this lead has no value for (and no fallback covers).
pub fn missing_for(lead: &Lead, names: &[String], defs: &FieldDefs) -> Vec<String> {
    let have = crate::leads::vars::lead_vars(lead);
    names
        .iter()
        .filter(|n| {
            have.get(n.as_str()).is_none_or(|v| v.as_str().is_none_or(|s| s.trim().is_empty()))
                && defs.by_key.get(n.as_str()).and_then(|d| d.fallback.as_deref()).is_none_or(|f| f.trim().is_empty())
        })
        .cloned()
        .collect()
}

/// A SQL condition on `l` (the `leads` table) that is true when the lead lacks a value for one of the names.
/// Names are checked here: a built-in text column from a fixed list, or a custom field key of letters, digits and `_`.
pub fn missing_sql(names: &[String], defs: &FieldDefs) -> String {
    let mut parts: Vec<String> = Vec::new();
    for n in names {
        if crate::leads::model::TEXT_FIELDS.contains(&n.as_str()) {
            parts.push(format!("btrim(COALESCE(l.{n}, '')) = ''"));
        } else if let Some(d) = defs.by_key.get(n.as_str()) {
            if d.fallback.as_deref().is_some_and(|f| !f.trim().is_empty()) {
                continue;
            }
            if n.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_') {
                parts.push(format!("btrim(COALESCE(l.custom_fields ->> '{n}', '')) = ''"));
            }
        } else {
            // no such field: no lead has a value
            parts.push("true".into());
        }
    }
    if parts.is_empty() { "false".into() } else { parts.join(" OR ") }
}
