//! Sends one job and handles the result.
use super::queue::{self, Claimed, ClaimOutcome, FailureChange, JobStatus, Plan};
use super::rules;
use super::senders::dry_run::DryRunSender;
use super::senders::fake::FakeSender;
use super::senders::{MailSender, OutgoingMessage, SendError, SendReceipt, sender_for};
use super::settings::SendingSettings;
use super::tokens;
use crate::accounts::model::{EmailAccount, OAuthSecrets, PasswordSecrets, Secrets};
use crate::accounts::repo;
use crate::email_management::oauth_common::RefreshError;
use crate::state::AppState;
use chrono::{DateTime, Duration, Utc};
use rand::Rng;
use rand::SeedableRng;
use rand::rngs::StdRng;
use serde_json::json;
use std::sync::Arc;
use uuid::Uuid;

/// Makes the sender for one account. The worker never knows which provider it uses.
pub trait SenderFactory: Send + Sync {
    fn make(&self, account: &EmailAccount, secrets: Secrets) -> Result<Box<dyn MailSender>, SendError>;
    /// `false` for senders that do no login (dry run, tests): the worker then skips decrypt and token refresh.
    fn needs_credentials(&self) -> bool {
        true
    }
}

/// The real senders. With `dry_run` it only logs.
pub struct RealSenders {
    pub dry_run: bool,
}

impl SenderFactory for RealSenders {
    fn make(&self, account: &EmailAccount, secrets: Secrets) -> Result<Box<dyn MailSender>, SendError> {
        if self.dry_run {
            return Ok(Box::new(DryRunSender));
        }
        sender_for(account, secrets)
    }
    fn needs_credentials(&self) -> bool {
        !self.dry_run
    }
}

/// Always gives the same `FakeSender` (for tests). It does no login.
pub struct FakeSenders(pub FakeSender);

impl SenderFactory for FakeSenders {
    fn make(&self, _: &EmailAccount, _: Secrets) -> Result<Box<dyn MailSender>, SendError> {
        Ok(Box::new(self.0.clone()))
    }
    fn needs_credentials(&self) -> bool {
        false
    }
}

pub const SEND_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

pub use crate::state::Clock;

pub struct Engine {
    pub state: AppState,
    pub factory: Arc<dyn SenderFactory>,
    pub clock: Clock,
    pub cancel: tokio_util::sync::CancellationToken,
    pub stuck_after: Duration,
}

impl Engine {
    pub fn new(state: AppState, factory: Arc<dyn SenderFactory>) -> Self {
        Self {
            clock: state.clock.clone(),
            state,
            factory,
            cancel: tokio_util::sync::CancellationToken::new(),
            stuck_after: Duration::minutes(10),
        }
    }

    pub fn now(&self) -> DateTime<Utc> {
        (self.clock)()
    }
}

/// Builds the email from the job and the account.
pub fn build_outgoing(c: &Claimed) -> OutgoingMessage {
    let j = &c.job;
    let headers = j
        .headers
        .as_object()
        .map(|o| o.iter().filter_map(|(k, v)| v.as_str().map(|v| (k.clone(), v.to_string()))).collect())
        .unwrap_or_default();
    OutgoingMessage {
        message_id: j.message_id_header.clone().unwrap_or_else(|| super::senders::new_message_id(&c.account.email)),
        from_email: c.account.email.clone(),
        from_name: Some(c.account.display_name.clone()).filter(|n| !n.is_empty()),
        to_email: j.to_email.clone(),
        to_name: j.to_name.clone(),
        subject: j.subject.clone(),
        body_text: j.body_text.clone().filter(|b| !b.trim().is_empty()),
        body_html: j.body_html.clone().filter(|b| !b.trim().is_empty()),
        headers,
        in_reply_to: j.in_reply_to.clone(),
        references: j.references.clone(),
        list_unsubscribe: c.settings.list_unsubscribe.clone(),
        thread_id: j.provider_thread_id.clone(),
    }
}

/// What to do with the job and the account after a send attempt. Pure: no DB, no clock.
pub fn plan_outcome(
    s: &SendingSettings,
    attempts: i32,
    result: &Result<SendReceipt, SendError>,
    now: DateTime<Utc>,
    rng: &mut impl Rng,
) -> Plan {
    let normal_gap = now + Duration::seconds(rules::gap_secs(s, rng));
    let base = Plan {
        job_status: JobStatus::Failed,
        send_after: None,
        refund_attempt: false,
        error: None,
        receipt: None,
        next_send_at: normal_gap,
        count_sent: false,
        failures: FailureChange::Keep,
        disconnect_reason: None,
        suppress: false,
        event: "failed",
        detail: json!({}),
    };
    match result {
        Ok(receipt) => Plan {
            job_status: JobStatus::Sent,
            receipt: Some(receipt.clone()),
            count_sent: true,
            failures: FailureChange::Reset,
            event: "sent",
            detail: json!({ "provider_message_id": receipt.provider_message_id, "provider_thread_id": receipt.provider_thread_id }),
            ..base
        },
        Err(SendError::Transient(msg)) => {
            let out_of_attempts = attempts >= s.max_attempts as i32;
            let backoff_idx = (attempts.max(1) as usize - 1).min(s.retry_backoff_mins.len().saturating_sub(1));
            let backoff = s.retry_backoff_mins.get(backoff_idx).copied().unwrap_or(15);
            let send_after = now + Duration::minutes(backoff as i64);
            Plan {
                job_status: if out_of_attempts { JobStatus::Failed } else { JobStatus::Queued },
                send_after: (!out_of_attempts).then_some(send_after),
                error: Some(msg.clone()),
                failures: FailureChange::Add,
                event: if out_of_attempts { "failed" } else { "retry_scheduled" },
                detail: json!({ "error": msg, "attempt": attempts, "retry_at": (!out_of_attempts).then_some(send_after) }),
                ..base
            }
        }
        Err(SendError::Permanent { reason, hard_bounce }) => Plan {
            error: Some(reason.clone()),
            // A hard bounce counts toward the bounce rate. Other permanent errors count as a failure.
            failures: if *hard_bounce { FailureChange::Keep } else { FailureChange::Add },
            suppress: *hard_bounce,
            detail: json!({ "error": reason, "hard_bounce": hard_bounce }),
            ..base
        },
        Err(SendError::RateLimited { retry_after, daily }) => {
            let next = if *daily {
                let today = rules::local_date(s, now);
                rules::next_window_open(s, rules::day_start(s, today.succ_opt().unwrap_or(today)))
            } else {
                now + retry_after.map_or(Duration::hours(1), |d| Duration::seconds(d.as_secs() as i64))
            };
            Plan {
                job_status: JobStatus::Queued,
                refund_attempt: true,
                error: Some(SendError::RateLimited { retry_after: *retry_after, daily: *daily }.describe()),
                next_send_at: next,
                event: "rate_limited",
                detail: json!({ "daily": daily, "retry_at": next }),
                ..base
            }
        }
        Err(SendError::AuthFailed(msg)) => Plan {
            job_status: JobStatus::Queued,
            refund_attempt: true,
            error: Some(format!("login failed: {msg}")),
            disconnect_reason: Some(format!("Login failed: {msg}. Connect the account again.")),
            event: "auth_failed",
            detail: json!({ "error": msg }),
            ..base
        },
        Err(SendError::Unknown(msg)) => Plan {
            job_status: JobStatus::Stuck,
            error: Some(format!("unknown result: {msg}. The email may have been sent. It is not resent on its own.")),
            event: "stuck",
            detail: json!({ "error": msg }),
            ..base
        },
    }
}

/// Refresh the token of an OAuth account. Maps the result to a `SendError`.
async fn refresh_for_send(
    engine: &Engine,
    account: &EmailAccount,
    o: &OAuthSecrets,
    now: DateTime<Utc>,
) -> Result<OAuthSecrets, SendError> {
    match tokens::refresh(&engine.state.pool, &engine.state.crypto, account, o, now).await {
        Ok(fresh) => Ok(fresh),
        Err(RefreshError::InvalidGrant(m)) => Err(SendError::AuthFailed(format!("the login is no longer valid ({m})"))),
        Err(RefreshError::Other(m)) => Err(SendError::Transient(format!("token refresh failed: {m}"))),
    }
}

/// Decrypt, refresh the token if needed, and send with a 60 second timeout.
/// The plain secrets live only inside this function.
async fn attempt_send(engine: &Engine, account: &EmailAccount, msg: &OutgoingMessage, now: DateTime<Utc>) -> Result<SendReceipt, SendError> {
    let mut secrets = if engine.factory.needs_credentials() {
        repo::load_secrets(&engine.state.crypto, account).map_err(|e| {
            tracing::error!(account_id = %account.id, error = %format!("{e:#}"), "cannot decrypt account secrets");
            SendError::Transient("cannot decrypt the saved credentials (is AIKIDO_ENCRYPTION_KEY the same key?)".into())
        })?
    } else {
        Secrets::Password(PasswordSecrets::default())
    };

    let mut refreshed = false;
    if let Secrets::OAuth(o) = &secrets {
        if engine.factory.needs_credentials() && tokens::needs_refresh(account.token_expires_at, now) {
            secrets = Secrets::OAuth(refresh_for_send(engine, account, o, now).await?);
            refreshed = true;
        }
    }

    let sender = engine.factory.make(account, secrets.clone())?;
    let mut result = tokio::time::timeout(SEND_TIMEOUT, sender.send(msg))
        .await
        .unwrap_or_else(|_| Err(SendError::Unknown("no answer after 60 seconds".into())));

    // A 401 on an OAuth account: refresh once, then try again. Nothing was sent, so this is safe.
    if let (Err(SendError::AuthFailed(_)), Secrets::OAuth(o), false) = (&result, &secrets, refreshed) {
        if engine.factory.needs_credentials() {
            let fresh = refresh_for_send(engine, account, o, now).await?;
            let sender = engine.factory.make(account, Secrets::OAuth(fresh))?;
            result = tokio::time::timeout(SEND_TIMEOUT, sender.send(msg))
                .await
                .unwrap_or_else(|_| Err(SendError::Unknown("no answer after 60 seconds".into())));
        }
    }
    result
}

/// Send one email now, outside the queue: no window, cap, gap, job row or counters. For test emails.
pub async fn send_direct(state: &AppState, account: &EmailAccount, msg: &OutgoingMessage) -> Result<SendReceipt, SendError> {
    let engine = Engine::new(state.clone(), state.senders.clone());
    attempt_send(&engine, account, msg, engine.now()).await
}

/// What happened for one account in one tick.
#[derive(Debug, PartialEq)]
pub enum AccountResult {
    Sent(Uuid),
    Idle,
}

/// Handle one ready account: claim a job, send it, write the result.
pub async fn process_account(engine: &Engine, account_id: Uuid) -> anyhow::Result<AccountResult> {
    let now = engine.now();
    let ClaimOutcome::Claimed(claimed) = queue::claim_next(&engine.state.pool, account_id, now).await? else {
        return Ok(AccountResult::Idle);
    };
    let job_id = claimed.job.id;
    let mut msg = build_outgoing(&claimed);
    let site = engine.state.site_url();
    let web = super::unsubscribe::web_link(site.as_deref(), &engine.state.crypto, job_id);
    let link = web.clone().unwrap_or_else(|| super::unsubscribe::mailto_link(&claimed.account.email));
    super::unsubscribe::finish(&mut msg, &claimed.settings, &link);
    let https = site.as_deref().is_some_and(|u| u.starts_with("https://"));
    msg.list_unsubscribe = super::unsubscribe::header(&claimed.settings, web.as_deref(), https);
    // Open tracking: the last step before the MIME is built, on the final HTML (placeholders, AI text, footer,
    // signature and unsubscribe link are all in). It never stops the send.
    // A plain-text email gets a matching HTML part (the same text) if the tracker can be placed in it.
    crate::tracking::send::apply_to_message(&engine.state, &claimed.job, &claimed.account, &mut msg).await;
    let result = attempt_send(engine, &claimed.account, &msg, now).await;

    let finished = engine.now();
    let mut rng = StdRng::from_entropy();
    let plan = plan_outcome(&claimed.settings, claimed.job.attempts, &result, finished, &mut rng);
    match &result {
        Ok(_) => tracing::info!(%account_id, %job_id, "email sent"),
        Err(e) => tracing::warn!(%account_id, %job_id, result = %e.describe(), "send did not succeed"),
    }
    queue::apply_plan(&engine.state.pool, &claimed, &plan, finished).await?;
    Ok(AccountResult::Sent(job_id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use std::time::Duration as StdDuration;

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 19, 10, 0, 0).unwrap()
    }

    fn plan(attempts: i32, r: Result<SendReceipt, SendError>) -> Plan {
        let s = SendingSettings::default();
        plan_outcome(&s, attempts, &r, now(), &mut StdRng::seed_from_u64(1))
    }

    #[test]
    fn ok_marks_sent_and_starts_the_gap() {
        let p = plan(1, Ok(SendReceipt { provider_message_id: Some("m1".into()), provider_thread_id: None, message_id_header: None }));
        assert_eq!(p.job_status, JobStatus::Sent);
        assert!(p.count_sent);
        assert_eq!(p.failures, FailureChange::Reset);
        let gap = (p.next_send_at - now()).num_seconds();
        assert!((420..=900).contains(&gap));
    }

    #[test]
    fn transient_retries_with_backoff_then_fails() {
        let p = plan(1, Err(SendError::Transient("x".into())));
        assert_eq!(p.job_status, JobStatus::Queued);
        assert_eq!(p.send_after, Some(now() + Duration::minutes(15)));
        assert_eq!(p.failures, FailureChange::Add);
        let p = plan(2, Err(SendError::Transient("x".into())));
        assert_eq!(p.send_after, Some(now() + Duration::minutes(60)));
        let p = plan(3, Err(SendError::Transient("x".into())));
        assert_eq!(p.job_status, JobStatus::Failed);
        assert_eq!(p.send_after, None);
        // The gap applies after every real send attempt.
        assert!(p.next_send_at > now() + Duration::seconds(419));
    }

    #[test]
    fn hard_bounce_suppresses() {
        let p = plan(1, Err(SendError::Permanent { reason: "no such user".into(), hard_bounce: true }));
        assert_eq!(p.job_status, JobStatus::Failed);
        assert!(p.suppress);
        assert_eq!(p.failures, FailureChange::Keep);
        let p = plan(1, Err(SendError::Permanent { reason: "bad".into(), hard_bounce: false }));
        assert!(!p.suppress);
        assert_eq!(p.failures, FailureChange::Add);
    }

    #[test]
    fn rate_limit_does_not_count_the_attempt() {
        let p = plan(1, Err(SendError::RateLimited { retry_after: Some(StdDuration::from_secs(120)), daily: false }));
        assert_eq!(p.job_status, JobStatus::Queued);
        assert!(p.refund_attempt);
        assert_eq!(p.next_send_at, now() + Duration::seconds(120));
        let p = plan(1, Err(SendError::RateLimited { retry_after: None, daily: false }));
        assert_eq!(p.next_send_at, now() + Duration::hours(1));
        // Daily quota: wait until the next allowed day's window.
        let p = plan(1, Err(SendError::RateLimited { retry_after: None, daily: true }));
        assert_eq!(p.next_send_at, Utc.with_ymd_and_hms(2026, 10, 20, 9, 0, 0).unwrap());
    }

    #[test]
    fn auth_failure_disconnects_and_requeues() {
        let p = plan(1, Err(SendError::AuthFailed("bad".into())));
        assert_eq!(p.job_status, JobStatus::Queued);
        assert!(p.refund_attempt);
        assert!(p.disconnect_reason.is_some());
    }

    #[test]
    fn unknown_is_stuck_and_never_resent() {
        let p = plan(1, Err(SendError::Unknown("timeout".into())));
        assert_eq!(p.job_status, JobStatus::Stuck);
        assert_eq!(p.send_after, None);
        assert!(!p.count_sent);
    }
}
