//! AI mode with the optimized prompt (`sequences.ai_prompt_kind = 'optimized'`).
//!
//! The server builds the user prompt (SENDER + LEAD as JSON), the model answers with the JSON of the default prompt,
//! and the code checks the mail. A mail that fails the checks twice is held, not sent. At most 3 model calls per mail.
use crate::generator::llm::{LlmClient, LlmRequest};
use crate::generator::pipeline::{CallInfo, FailKind, GenFailure, failure_of};
use crate::generator::prompt::repair_prompt;
use crate::generator::settings::GeneratorSettings;
use crate::leads::model::Lead;
use crate::prompting::Blocks;
use crate::prompting::checks::{self, CheckInput, ModelOut};
use crate::prompting::sender::SenderProfile;
use crate::prompting::user_prompt;
use serde_json::Value;
use sha2::{Digest, Sha256};
use sqlx::PgConnection;
use std::time::Instant;
use uuid::Uuid;

/// Model calls per mail: the first, one repair for bad JSON, one more after failed checks. Never more than 3 in all.
pub const MAX_CALLS: usize = 3;

/// What a campaign with the optimized prompt uses.
#[derive(Clone, Debug)]
pub struct Optimized {
    pub blocks: Blocks,
    /// The profile the campaign points at, else the current default. `None`: no saved profile.
    pub profile: Option<SenderProfile>,
    /// First word of the account name. Used when the profile has no first name.
    pub fallback_name: String,
}

impl Optimized {
    pub fn proof(&self) -> &str {
        self.profile.as_ref().map(|p| p.proof_or_none.as_str()).unwrap_or("none")
    }

    pub fn sender(&self) -> Value {
        user_prompt::sender_json(self.profile.as_ref(), &self.fallback_name)
    }

    /// The system and user prompt for a lead and step. `failures` is the list of failed checks of the first try.
    pub fn prompts(&self, lead: &Lead, step: i32, previous_angles: &[String], failures: &[String]) -> (String, String) {
        let system = self.blocks.system_prompt(step);
        let user = user_prompt::build(&self.sender(), &user_prompt::lead_json(lead, step, previous_angles), step, failures);
        (system, user)
    }

    /// Hash of what the mail depends on, without the angles of earlier steps (they change when an earlier mail changes).
    /// It has the sender profile version and the three blocks, so a new version marks the unsent mails stale.
    pub fn hash(&self, lead: &Lead, step: i32, footer: &str) -> String {
        let (system, user) = self.prompts(lead, step, &[], &[]);
        let version = self.profile.as_ref().map(|p| p.version).unwrap_or(0);
        let mut h = Sha256::new();
        for part in [system.as_str(), user.as_str(), footer, &version.to_string()] {
            h.update(part.as_bytes());
            h.update([0]);
        }
        h.finalize().iter().map(|b| format!("{b:02x}")).collect()
    }
}

/// The angles of the steps before this one, in step order, and whether the step right before has a usable angle.
pub async fn previous_state(conn: &mut PgConnection, campaign_id: Uuid, lead_id: Uuid, step: i32) -> Result<(Vec<String>, bool), sqlx::Error> {
    let rows: Vec<(i32, String, Option<String>)> = sqlx::query_as(
        "SELECT step_number, status, angle_used FROM lead_emails WHERE campaign_id = $1 AND lead_id = $2 AND step_number < $3 ORDER BY step_number",
    )
    .bind(campaign_id)
    .bind(lead_id)
    .bind(step)
    .fetch_all(conn)
    .await?;
    let usable = |status: &str| matches!(status, "ready" | "review" | "stale" | "sent");
    let angles: Vec<String> = rows.iter().filter(|r| usable(&r.1)).filter_map(|r| r.2.clone()).filter(|a| !a.trim().is_empty()).collect();
    let prev_ok = step <= 1 || rows.iter().any(|r| r.0 == step - 1 && usable(&r.1) && r.2.as_deref().is_some_and(|a| !a.trim().is_empty()));
    Ok((angles, prev_ok))
}

pub enum Verdict {
    Mail(ModelOut),
    Held { reason: String, failures: Vec<String> },
}

pub struct OptOut {
    pub verdict: Verdict,
    pub calls: Vec<CallInfo>,
    /// The last answer the model gave, even when the mail was held. The preview shows its checks.
    pub last: Option<ModelOut>,
}

fn held(reason: impl Into<String>, failures: Vec<String>, calls: Vec<CallInfo>, last: Option<ModelOut>) -> OptOut {
    OptOut { verdict: Verdict::Held { reason: reason.into(), failures }, calls, last }
}

/// One mail. Required fields empty: held, no model call. Model says HOLD: held, no retry.
/// Checks fail: one more call with the failure list. Fail again: held `checks_failed`.
pub async fn generate(client: &dyn LlmClient, s: &GeneratorSettings, o: &Optimized, lead: &Lead, step: i32, previous_angles: &[String]) -> Result<OptOut, GenFailure> {
    let missing = user_prompt::missing_required(lead);
    if !missing.is_empty() {
        return Ok(held(format!("missing_required: {}", missing.join(", ")), vec![], vec![], None));
    }
    let mut calls: Vec<CallInfo> = Vec::new();
    let mut failures: Vec<String> = Vec::new();
    let mut repair: Option<String> = None;
    let mut bad_json_retry = false;
    let mut check_retry = false;
    for call_no in 1..=MAX_CALLS {
        let (system, user) = o.prompts(lead, step, previous_angles, &failures);
        let req = LlmRequest {
            system,
            user: match &repair {
                Some(reason) => repair_prompt(&user, reason),
                None => user,
            },
            max_output_tokens: s.max_output_tokens.max(700),
            json: true,
            temperature: s.temperature,
        };
        let started = Instant::now();
        let resp = match client.complete(&req).await {
            Ok(r) => r,
            Err(e) => {
                calls.push(CallInfo { duration_ms: started.elapsed().as_millis() as u32, ok: false, error_code: Some(e.code()), ..Default::default() });
                return Err(failure_of(&e, calls));
            }
        };
        let mut info = CallInfo {
            model: resp.model.clone(),
            input_tokens: resp.input_tokens,
            output_tokens: resp.output_tokens,
            cached_tokens: resp.cached_tokens,
            duration_ms: started.elapsed().as_millis() as u32,
            ok: true,
            error_code: None,
        };
        let can_retry = call_no < MAX_CALLS;
        match checks::parse_out(&resp.text) {
            Err(msg) => {
                info.ok = false;
                info.error_code = Some("bad_output".into());
                calls.push(info);
                if bad_json_retry || !can_retry {
                    return Err(GenFailure { kind: FailKind::Permanent, code: "bad_output".into(), message: msg, calls });
                }
                bad_json_retry = true;
                repair = Some(msg);
            }
            Ok(out) if out.is_hold() => {
                calls.push(info);
                let reason = if out.hold_reason.is_empty() { "model_hold".to_string() } else { out.hold_reason.clone() };
                return Ok(held(reason, vec![], calls, Some(out)));
            }
            Ok(out) => {
                let found = checks::run(&out, &CheckInput { step, previous_angles, proof: o.proof(), extra_banned: &s.banned_phrases });
                if found.is_empty() {
                    calls.push(info);
                    return Ok(OptOut { last: Some(out.clone()), verdict: Verdict::Mail(out), calls });
                }
                info.ok = false;
                info.error_code = Some("checks_failed".into());
                calls.push(info);
                if check_retry || !can_retry {
                    return Ok(held(format!("checks_failed: {}", found.join("; ")), found, calls, Some(out)));
                }
                check_retry = true;
                failures = found;
                repair = None;
            }
        }
    }
    Err(GenFailure { kind: FailKind::Permanent, code: "bad_output".into(), message: "no valid answer".into(), calls })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::generator::llm::fake::{FakeLlm, response};
    use serde_json::json;

    fn lead() -> Lead {
        let mut l = crate::prompting::user_prompt::test_lead();
        l.first_name = Some("Ana".into());
        l.company_name = Some("Acme".into());
        l
    }

    fn opt() -> Optimized {
        Optimized { blocks: Blocks::default(), profile: None, fallback_name: "Dana Lee".into() }
    }

    fn good(step: i32) -> String {
        json!({ "status": "OK", "subject": if checks::subject_allowed(step) { "new depot plans" } else { "" }, "body": "Hi Ana, the new depot caught my eye. Routing gets slow at that size. Worth a look?\nDana",
                "angle_used": format!("angle {step}"), "used_trigger": true, "claims_check": ["opened a depot (LEAD trigger)"], "hold_reason": "" })
        .to_string()
    }

    fn bad_dash() -> String {
        json!({ "status": "OK", "subject": "new depot plans", "body": "Hi Ana \u{2014} routing is slow.", "angle_used": "x", "used_trigger": false, "claims_check": [], "hold_reason": "" }).to_string()
    }

    fn settings() -> GeneratorSettings {
        GeneratorSettings::default()
    }

    #[tokio::test]
    async fn a_good_mail_takes_one_call() {
        let llm = FakeLlm::new();
        llm.push(Ok(response(good(1))));
        let r = generate(llm.as_ref(), &settings(), &opt(), &lead(), 1, &[]).await.unwrap();
        assert!(matches!(r.verdict, Verdict::Mail(_)));
        assert_eq!(r.calls.len(), 1);
        let sent = &llm.calls()[0];
        assert!(sent.system.contains("PRIMARY MAIL") && sent.user.contains("\"company\": \"Acme\""));
    }

    #[tokio::test]
    async fn empty_required_fields_hold_without_a_call() {
        let llm = FakeLlm::new();
        let mut l = lead();
        l.company_name = None;
        let r = generate(llm.as_ref(), &settings(), &opt(), &l, 1, &[]).await.unwrap();
        match r.verdict {
            Verdict::Held { reason, .. } => assert_eq!(reason, "missing_required: company"),
            _ => panic!("must hold"),
        }
        assert_eq!(llm.call_count(), 0);
    }

    #[tokio::test]
    async fn a_model_hold_is_saved_with_its_reason_and_not_retried() {
        let llm = FakeLlm::new();
        llm.push(Ok(response(json!({ "status": "HOLD", "subject": "", "body": "", "angle_used": "", "used_trigger": false, "claims_check": [], "hold_reason": "sender_incomplete: offer is empty" }).to_string())));
        let r = generate(llm.as_ref(), &settings(), &opt(), &lead(), 1, &[]).await.unwrap();
        match r.verdict {
            Verdict::Held { reason, .. } => assert_eq!(reason, "sender_incomplete: offer is empty"),
            _ => panic!("must hold"),
        }
        assert_eq!(llm.call_count(), 1);
    }

    #[tokio::test]
    async fn a_failed_check_gets_one_more_call_with_the_failure_list() {
        let llm = FakeLlm::new();
        llm.push(Ok(response(bad_dash())));
        llm.push(Ok(response(good(1))));
        let r = generate(llm.as_ref(), &settings(), &opt(), &lead(), 1, &[]).await.unwrap();
        assert!(matches!(r.verdict, Verdict::Mail(_)));
        assert_eq!(r.calls.len(), 2);
        assert!(llm.calls()[1].user.contains("dash"), "the failure list is in the second prompt");
    }

    #[tokio::test]
    async fn two_failed_checks_hold_the_mail() {
        let llm = FakeLlm::new();
        llm.push(Ok(response(bad_dash())));
        llm.push(Ok(response(bad_dash())));
        let r = generate(llm.as_ref(), &settings(), &opt(), &lead(), 1, &[]).await.unwrap();
        match r.verdict {
            Verdict::Held { reason, failures } => {
                assert!(reason.starts_with("checks_failed: ") && reason.contains("dash"));
                assert!(!failures.is_empty());
            }
            _ => panic!("must hold"),
        }
        assert_eq!(llm.call_count(), 2);
    }

    #[tokio::test]
    async fn never_more_than_three_calls() {
        // bad json, then a failed check, then a failed check again: 3 calls, then hold
        let llm = FakeLlm::new();
        llm.push(Ok(response("not json")));
        llm.push(Ok(response(bad_dash())));
        llm.push(Ok(response(bad_dash())));
        llm.push(Ok(response(good(1))));
        let r = generate(llm.as_ref(), &settings(), &opt(), &lead(), 1, &[]).await.unwrap();
        assert!(matches!(r.verdict, Verdict::Held { .. }));
        assert_eq!(llm.call_count(), 3);
        // bad json twice is an error, after 2 calls
        let llm = FakeLlm::new();
        llm.push(Ok(response("x")));
        llm.push(Ok(response("y")));
        assert!(generate(llm.as_ref(), &settings(), &opt(), &lead(), 1, &[]).await.is_err());
        assert_eq!(llm.call_count(), 2);
    }

    #[tokio::test]
    async fn a_repeated_angle_fails_the_check() {
        let llm = FakeLlm::new();
        llm.push(Ok(response(good(2))));
        llm.push(Ok(response(good(2))));
        let prev = vec!["angle 2".to_string()];
        let r = generate(llm.as_ref(), &settings(), &opt(), &lead(), 2, &prev).await.unwrap();
        match r.verdict {
            Verdict::Held { reason, .. } => assert!(reason.contains("previous_angles")),
            _ => panic!("must hold"),
        }
        assert!(llm.calls()[0].user.contains("angle 2"), "previous_angles are in the LEAD block");
    }

    #[test]
    fn the_hash_changes_with_the_profile_version_and_the_blocks() {
        let l = lead();
        let a = opt().hash(&l, 1, "");
        let mut b = opt();
        b.blocks.core = Some("# OUTPUT\nchanged".into());
        assert_ne!(a, b.hash(&l, 1, ""));
        let mut c = opt();
        c.profile = Some(crate::prompting::sender::test_profile(2));
        let mut d = opt();
        d.profile = Some(crate::prompting::sender::test_profile(3));
        assert_ne!(c.hash(&l, 1, ""), d.hash(&l, 1, ""));
        assert_eq!(a, opt().hash(&l, 1, ""));
    }
}
