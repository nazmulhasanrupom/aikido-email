//! The Optimised mode: one model call writes the primary email and three follow-ups for a lead.
//! The system prompt and the user prompt are plain text the user can edit (`sequences.ai_system_prompt` and the
//! `ai_user_prompt` of step 1); empty means the shipped default. The answer is checked only for its format.
use crate::generator::llm::{LlmClient, LlmRequest};
use crate::generator::pipeline::{CallInfo, FailKind, GenFailure, failure_of};
use crate::generator::prompt::repair_prompt;
use crate::generator::settings::GeneratorSettings;
use crate::leads::model::Lead;
use crate::prompting::optimized::{self, Mail};
use crate::prompting::sender::SenderProfile;
use sha2::{Digest, Sha256};
use std::time::Instant;

pub use crate::prompting::optimized::EMAILS;

#[derive(Clone, Debug)]
pub struct Optimized {
    /// The campaign's own instructions, or `None` for the shipped default.
    pub system: Option<String>,
    /// The campaign's own user prompt, or `None` for the shipped default.
    pub user: Option<String>,
    /// The saved sender profile. `None`: the sender values are left out.
    pub profile: Option<SenderProfile>,
    /// First word of the account name. Used when the profile has no first name.
    pub fallback_name: String,
}

impl Optimized {
    /// The filled system prompt (with the fixed answer format) and the filled user prompt for a lead.
    pub fn prompts(&self, lead: &Lead) -> (String, String) {
        let vars = optimized::vars(lead, self.profile.as_ref(), &self.fallback_name);
        let system = optimized::fill(self.system.as_deref().unwrap_or(optimized::DEFAULT_SYSTEM), &vars);
        let user = optimized::fill(self.user.as_deref().unwrap_or(optimized::DEFAULT_USER), &vars);
        (format!("{}{}", system.trim_end(), optimized::rules()), user)
    }

    /// Hash of what an email of this step depends on. A change of the lead, the profile or a prompt makes the mail out of date.
    pub fn hash(&self, lead: &Lead, step: i32) -> String {
        let (system, user) = self.prompts(lead);
        let mut h = Sha256::new();
        for part in [system.as_str(), user.as_str(), &step.to_string()] {
            h.update(part.as_bytes());
            h.update([0]);
        }
        h.finalize().iter().map(|b| format!("{b:02x}")).collect()
    }
}

/// The text of the steps the Optimised mode makes by itself. They are removed again when the mode is left.
pub const PLACEHOLDER_SUBJECT: &str = "Written by the AI";
pub const PLACEHOLDER_BODY: &str = "Written by the AI for each lead.";

/// Leaving the Optimised mode: take away the steps it made and nobody edited, so Template and AI mode stay clean.
/// A live campaign keeps its steps (leads may already wait on them).
pub async fn drop_placeholder_steps(conn: &mut sqlx::PgConnection, sequence_id: uuid::Uuid, campaign_is_draft: bool) -> Result<(), sqlx::Error> {
    if !campaign_is_draft {
        return Ok(());
    }
    sqlx::query("DELETE FROM sequence_steps WHERE sequence_id = $1 AND step_number > 1 AND body_text = $2 AND body_html IS NULL").bind(sequence_id).bind(PLACEHOLDER_BODY).execute(&mut *conn).await?;
    sqlx::query("UPDATE sequence_steps SET subject = 'Hello {{first_name|there}}', body_text = E'Hi {{first_name|there}},\n\n' WHERE sequence_id = $1 AND step_number = 1 AND subject = $2 AND body_text = $3")
        .bind(sequence_id)
        .bind(PLACEHOLDER_SUBJECT)
        .bind(PLACEHOLDER_BODY)
        .execute(&mut *conn)
        .await?;
    Ok(())
}

/// The Optimised mode writes exactly 4 emails. Make sure the sequence has 4 steps: missing ones are added
/// (3, 4 and 5 days after the step before). A sequence with more than 4 steps must be cut by hand first.
pub async fn ensure_four_steps(conn: &mut sqlx::PgConnection, sequence_id: uuid::Uuid, now: chrono::DateTime<chrono::Utc>) -> crate::error::ApiResult<()> {
    let (n,): (i64,) = sqlx::query_as("SELECT count(*) FROM sequence_steps WHERE sequence_id = $1").bind(sequence_id).fetch_one(&mut *conn).await?;
    if n > EMAILS as i64 {
        return Err(crate::error::ApiError::unprocessable("too_many_steps", "The Optimised mode writes 4 emails: the primary email and 3 follow-ups. Remove the extra steps first."));
    }
    for step in (n as i32 + 1)..=EMAILS as i32 {
        let delay = [0, 3, 4, 5][step as usize - 1];
        sqlx::query(
            "INSERT INTO sequence_steps (id, sequence_id, step_number, delay_amount, delay_type, subject, body_text, updated_at, ai_slots, step_hash)
             VALUES ($1, $2, $3, $4, 'business_days', $5, $7, $6, '[]', '')
             ON CONFLICT (sequence_id, step_number) DO NOTHING",
        )
        .bind(uuid::Uuid::now_v7())
        .bind(sequence_id)
        .bind(step)
        .bind(delay)
        .bind(if step == 1 { PLACEHOLDER_SUBJECT } else { "" })
        .bind(now)
        .bind(PLACEHOLDER_BODY)
        .execute(&mut *conn)
        .await?;
    }
    Ok(())
}

pub struct Generated {
    /// The primary email and the three follow-ups, in step order.
    pub mails: Vec<Mail>,
    pub calls: Vec<CallInfo>,
}

/// One call writes all four. If the answer has the wrong format, the model gets one more try.
pub async fn generate(client: &dyn LlmClient, s: &GeneratorSettings, o: &Optimized, lead: &Lead) -> Result<Generated, GenFailure> {
    let (system, user) = o.prompts(lead);
    let mut calls: Vec<CallInfo> = Vec::new();
    let mut reason = String::new();
    for attempt in 0..2 {
        let req = LlmRequest {
            system: system.clone(),
            user: if attempt == 0 { user.clone() } else { repair_prompt(&user, &reason) },
            max_output_tokens: s.max_output_tokens.max(1400),
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
        match optimized::parse_output(&resp.text) {
            Ok(mails) => {
                calls.push(info);
                return Ok(Generated { mails, calls });
            }
            Err(msg) => {
                info.ok = false;
                info.error_code = Some("bad_output".into());
                calls.push(info);
                reason = msg;
            }
        }
    }
    Err(GenFailure { kind: FailKind::Permanent, code: "bad_output".into(), message: reason, calls })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::generator::llm::fake::{FakeLlm, response};

    fn opt() -> Optimized {
        Optimized { system: None, user: None, profile: None, fallback_name: "Dana Lee".into() }
    }

    const GOOD: &str = r#"{"primary":{"subject":"new depot","body":"one"},"followup_1":{"body":"two"},"followup_2":{"body":"three"},"followup_3":{"body":"four"}}"#;

    #[test]
    fn the_prompts_are_plain_text_with_the_lead_values_and_no_empty_lines() {
        let lead = crate::prompting::fields::test_lead();
        let (system, user) = opt().prompts(&lead);
        assert!(system.starts_with("# ROLE") && system.contains("# SELF CHECK") && system.contains("# THIN INFORMATION"));
        assert!(system.contains("\"followup_3\"") && system.contains("<optimized_request>"));
        assert!(user.contains("Company: Acme \"Co\"") && user.contains("Recent event: Opened a new depot") && user.contains("Event checked by a person: yes"));
        assert!(!user.contains("Note:"), "an empty value drops its line");
        assert!(!user.contains("{{") && !user.contains('{'));
    }

    #[test]
    fn own_prompts_replace_the_defaults() {
        let lead = crate::prompting::fields::test_lead();
        let mut o = opt();
        o.system = Some("Be brief for {{sender_first_name}}.".into());
        o.user = Some("Lead: {{first_name}} ({{company}}).".into());
        let (system, user) = o.prompts(&lead);
        assert!(system.starts_with("Be brief for Dana."));
        assert_eq!(user, "Lead: Ana (Acme \"Co\").");
    }

    #[test]
    fn the_hash_changes_with_the_lead_the_prompt_and_the_step() {
        let lead = crate::prompting::fields::test_lead();
        let a = opt().hash(&lead, 1);
        assert_eq!(a, opt().hash(&lead, 1));
        assert_ne!(a, opt().hash(&lead, 2));
        let mut l2 = lead.clone();
        l2.industry = Some("Retail".into());
        assert_ne!(a, opt().hash(&l2, 1));
        let mut o = opt();
        o.user = Some("x".into());
        assert_ne!(a, o.hash(&lead, 1));
    }

    #[tokio::test]
    async fn one_call_writes_all_four() {
        let llm = FakeLlm::new();
        llm.push(Ok(response(GOOD)));
        let g = generate(llm.as_ref(), &GeneratorSettings::default(), &opt(), &crate::prompting::fields::test_lead()).await.unwrap();
        assert_eq!(g.mails.len(), 4);
        assert_eq!((g.calls.len(), llm.call_count()), (1, 1));
    }

    #[tokio::test]
    async fn a_wrong_format_gets_one_more_try_and_then_fails() {
        let lead = crate::prompting::fields::test_lead();
        let llm = FakeLlm::new();
        llm.push(Ok(response("sorry")));
        llm.push(Ok(response(GOOD)));
        assert_eq!(generate(llm.as_ref(), &GeneratorSettings::default(), &opt(), &lead).await.unwrap().calls.len(), 2);
        assert!(llm.calls()[1].user.contains("Your last answer was refused"));
        let llm = FakeLlm::new();
        llm.push(Ok(response("a")));
        llm.push(Ok(response("b")));
        assert!(generate(llm.as_ref(), &GeneratorSettings::default(), &opt(), &lead).await.is_err());
        assert_eq!(llm.call_count(), 2);
    }
}
