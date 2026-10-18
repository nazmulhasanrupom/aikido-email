-- The Optimised mode: one prompt writes the primary email and three follow-ups in one call.
-- It is a third content mode. The block/check/HOLD design of 0014 is dropped.

ALTER TABLE campaigns DROP CONSTRAINT campaigns_content_mode_check;
ALTER TABLE campaigns ADD CONSTRAINT campaigns_content_mode_check CHECK (content_mode IN ('template', 'ai', 'optimized'));

DELETE FROM lead_emails WHERE status = 'held';
ALTER TABLE lead_emails DROP CONSTRAINT lead_emails_status_check;
ALTER TABLE lead_emails ADD CONSTRAINT lead_emails_status_check CHECK (status IN ('none', 'writing', 'review', 'ready', 'stale', 'failed', 'sent'));
ALTER TABLE lead_emails
    DROP COLUMN angle_used,
    DROP COLUMN used_trigger,
    DROP COLUMN claims_check,
    DROP COLUMN hold_reason,
    DROP COLUMN check_failures;

ALTER TABLE sequences
    DROP COLUMN sender_profile_id,
    DROP COLUMN ai_prompt_kind,
    DROP COLUMN ai_prompt_core,
    DROP COLUMN ai_prompt_primary,
    DROP COLUMN ai_prompt_followup,
    DROP COLUMN ai_prompt_version;
