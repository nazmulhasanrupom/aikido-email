-- UI update map v2, U3: AI mode. Work claim columns on saved emails, and a new llm_calls purpose.

ALTER TABLE lead_emails
    ADD COLUMN claim_id   uuid,
    ADD COLUMN claimed_at timestamptz,
    ADD COLUMN attempts   integer NOT NULL DEFAULT 0,
    ADD COLUMN retry_at   timestamptz;
-- the worker: rows that wait to be written
CREATE INDEX lead_emails_writing_idx ON lead_emails (created_at) WHERE status = 'writing';

ALTER TABLE llm_calls DROP CONSTRAINT llm_calls_purpose_check;
ALTER TABLE llm_calls ADD CONSTRAINT llm_calls_purpose_check CHECK (purpose IN ('slot', 'draft', 'preview', 'test', 'import_mapping', 'lead_email', 'lead_email_test'));
