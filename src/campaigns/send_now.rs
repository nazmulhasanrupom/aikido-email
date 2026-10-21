//! "Send now" on a campaign lead: send the next email of the lead at once, with no rules.
//!
//! It makes the job of the next step (as the follow-up scheduler would), marks it `send_now`, and runs the sending
//! engine for that account. A `send_now` job skips the send window, the daily cap, the gap, the cooldown, the safety
//! gate and the suppression check, like a test email. It is a real send: it is on the timeline of the lead, it has the
//! unsubscribe link and the tracker, and the sequence goes on to the next step after it.
//!
//! What it does not skip: a lead that is not enrolled is enrolled first with the normal enroll call, which refuses an
//! address that is on the suppression list. A lead whose sequence has ended (stopped, finished, failed) has no next
//! email. A step whose AI text is not written yet cannot be sent.
use super::feeder::pick_account;
use super::routes::{get, pool_accounts};
use crate::error::{ApiError, ApiJson, ApiPath, ApiResult};
use crate::followups::enroll::{self, EnrollError, EnrollStatus, NewEnrollment};
use crate::followups::scheduler::{self, DueOutcome};
use crate::sending::worker::{AccountResult, Engine, process_account};
use crate::state::AppState;
use axum::extract::State;
use serde::Serialize;
use utoipa::ToSchema;
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new().routes(routes!(send_now))
}

#[derive(Serialize, ToSchema)]
pub struct SendNowReply {
    pub job_id: Uuid,
    /// The step that was sent (1 = the first email).
    pub step_number: Option<i32>,
    /// `sent`, `failed` or `queued` (the account was busy: it goes out within seconds, with no rules).
    pub status: String,
    /// Why it failed, when `status` is `failed`.
    pub error: Option<String>,
}

/// Send the next email of a lead now.
#[utoipa::path(post, path = "/v1/campaigns/{id}/leads/{lead_id}/send-now", tag = "campaigns",
    params(("id" = Uuid, Path), ("lead_id" = Uuid, Path)),
    responses((status = 200, body = SendNowReply), (status = 404, description = "No such campaign, or the lead is not in it"),
        (status = 409, description = "`no_healthy_account`, `sequence_ended`, `email_not_ready`, `already_sending`, `busy`"),
        (status = 422, description = "The lead could not be enrolled or the email could not be made")),
    security(("api_key" = ["campaigns"])),
    description = "Scope: `campaigns`. Sends the next email of the lead at once and checks no rule: not the send window, the daily cap, the gap, the cooldown, the \
        inbox-read safety gate or the suppression list. A lead that is not enrolled yet is enrolled first (with the normal enroll call, which still refuses a \
        suppressed address) and gets its first email. The send is real: unsubscribe link, tracker and the timeline of the lead are the same, and the sequence goes on.")]
async fn send_now(State(st): State<AppState>, ApiPath((id, lead_id)): ApiPath<(Uuid, Uuid)>) -> ApiResult<ApiJson<SendNowReply>> {
    let now = st.now();
    let mut conn = st.pool.acquire().await?;
    let c = get(&mut conn, id).await?.ok_or_else(|| ApiError::not_found("campaign_not_found", "No campaign with this id."))?;
    let row: Option<(Option<Uuid>,)> = sqlx::query_as("SELECT enrollment_id FROM campaign_leads WHERE campaign_id = $1 AND lead_id = $2").bind(id).bind(lead_id).fetch_optional(&mut *conn).await?;
    let Some((existing,)) = row else { return Err(ApiError::not_found("lead_not_in_campaign", "This lead is not in the campaign.")) };

    // 1. A lead that is not enrolled yet: enroll it now (no daily limit), on an account of the pool.
    let enrollment_id = match existing {
        Some(e) => e,
        None => {
            let accounts = pool_accounts(&mut conn, id).await?;
            let (last,): (Option<Uuid>,) = sqlx::query_as("SELECT last_account_id FROM campaigns WHERE id = $1").bind(id).fetch_one(&mut *conn).await?;
            let Some(account) = pick_account(&mut conn, &c, last, &accounts).await? else {
                return Err(ApiError::conflict("no_healthy_account", "No account of the campaign is healthy (connected and not paused)."));
            };
            let item = NewEnrollment {
                to_email: None,
                account_id: account,
                variables: None,
                idempotency_key: Some(format!("campaign:{id}:lead:{lead_id}")),
                start_at: None,
                lead_id: Some(lead_id),
            };
            let items = match enroll::enroll(&st.pool, c.sequence_id, vec![item], now).await {
                Ok(i) => i,
                Err(EnrollError::MissingVariables(names)) => {
                    return Err(ApiError::unprocessable("missing_variable", format!("The emails need a value for: {}.", names.join(", "))));
                }
                Err(EnrollError::Db(e)) => return Err(e.into()),
            };
            let Some(item) = items.into_iter().next() else { return Err(ApiError::internal()) };
            let enrollment = match (item.status, item.enrollment, item.error) {
                (EnrollStatus::Enrolled | EnrollStatus::Duplicate, Some(e), _) => e,
                (_, _, Some(err)) => return Err(ApiError::unprocessable(err.code, err.message)),
                _ => return Err(ApiError::internal()),
            };
            sqlx::query("UPDATE campaign_leads SET state = 'enrolled', skip_reason = NULL, enrollment_id = $3, enrolled_at = $4 WHERE campaign_id = $1 AND lead_id = $2")
                .bind(id)
                .bind(lead_id)
                .bind(enrollment.id)
                .bind(now)
                .execute(&mut *conn)
                .await?;
            sqlx::query("UPDATE campaigns SET last_account_id = $2 WHERE id = $1").bind(id).bind(account).execute(&mut *conn).await?;
            enrollment.id
        }
    };
    drop(conn);

    // 2. The job of the next step.
    let (status, current_job, account_id): (String, Option<Uuid>, Uuid) =
        sqlx::query_as("SELECT status::text, current_job_id, account_id FROM enrollments WHERE id = $1").bind(enrollment_id).fetch_one(&st.pool).await?;
    let job_id = match status.as_str() {
        "active" | "paused" => match scheduler::process_now(&st.pool, enrollment_id, now).await? {
            DueOutcome::Queued(job) => job,
            DueOutcome::Waiting(_) => return Err(ApiError::conflict("email_not_ready", "The email of this step is not written yet (AI text waiting). Write or approve it first.")),
            DueOutcome::Failed(code) => return Err(ApiError::unprocessable(code, "The email could not be made. Look at the lead's timeline.")),
            DueOutcome::Finished | DueOutcome::Stopped(_) => return Err(ApiError::conflict("sequence_ended", "This lead has no next email.")),
            DueOutcome::Skipped => return Err(ApiError::conflict("busy", "This lead is being handled right now. Try again in a moment.")),
        },
        // the next step is already a queued job: let it go out now
        "waiting_send" => {
            let Some(job) = current_job else { return Err(ApiError::conflict("busy", "This lead is being handled right now. Try again in a moment.")) };
            let marked = sqlx::query("UPDATE send_jobs SET metadata = metadata || '{\"send_now\": true}'::jsonb, send_after = NULL, updated_at = $2 WHERE id = $1 AND status = 'queued'")
                .bind(job)
                .bind(now)
                .execute(&st.pool)
                .await?;
            if marked.rows_affected() == 0 {
                return Err(ApiError::conflict("already_sending", "This email is being sent right now."));
            }
            job
        }
        _ => return Err(ApiError::conflict("sequence_ended", format!("This lead's sequence is {status}: there is no next email."))),
    };

    // 3. Send. The engine takes a `send_now` job before it looks at any rule. If the account is busy with another email,
    // the job stays queued and the next tick sends it with no rules too.
    let engine = Engine::new(st.clone(), st.senders.clone());
    for _ in 0..5 {
        let job_status: String = sqlx::query_scalar("SELECT status::text FROM send_jobs WHERE id = $1").bind(job_id).fetch_one(&st.pool).await?;
        if job_status != "queued" {
            break;
        }
        match process_account(&engine, account_id).await {
            Ok(AccountResult::Sent(_)) => {}
            Ok(AccountResult::Idle) => break,
            Err(e) => {
                tracing::warn!(error = %format!("{e:#}"), %job_id, "send now: the engine failed");
                break;
            }
        }
    }
    let (job_status, step, error): (String, Option<i32>, Option<String>) =
        sqlx::query_as("SELECT status::text, step_number, last_error FROM send_jobs WHERE id = $1").bind(job_id).fetch_one(&st.pool).await?;
    let status = match job_status.as_str() {
        "sent" => "sent",
        "failed" | "skipped" | "cancelled" | "stuck" => "failed",
        _ => "queued",
    };
    Ok(ApiJson(SendNowReply { job_id, step_number: step, status: status.into(), error: error.filter(|_| status == "failed") }))
}
