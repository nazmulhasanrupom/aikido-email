-- Email optimization: lead columns for the default prompt, sender profiles, personalization sources,
-- prompt blocks on sequences, and the checks and HOLD state on saved emails.

ALTER TYPE campaign_skip_reason ADD VALUE IF NOT EXISTS 'held_skipped';

ALTER TABLE leads
    ADD COLUMN industry             text,
    ADD COLUMN personalization_note text,
    ADD COLUMN trigger_event        text,
    ADD COLUMN trigger_verified     boolean NOT NULL DEFAULT false;

-- One row per saved version. The current profile is the highest version with is_default.
-- These four names are now built-in lead columns. A custom field with the same key moves into the column.
UPDATE leads SET
    industry = COALESCE(industry, NULLIF(btrim(custom_fields ->> 'industry'), '')),
    personalization_note = COALESCE(personalization_note, NULLIF(btrim(custom_fields ->> 'personalization_note'), '')),
    trigger_event = COALESCE(trigger_event, NULLIF(btrim(custom_fields ->> 'trigger_event'), '')),
    trigger_verified = trigger_verified OR lower(btrim(COALESCE(custom_fields ->> 'trigger_verified', ''))) IN ('yes', 'true', '1', 'y'),
    custom_fields = custom_fields - 'industry' - 'personalization_note' - 'trigger_event' - 'trigger_verified'
 WHERE custom_fields ?| array['industry', 'personalization_note', 'trigger_event', 'trigger_verified'];
DELETE FROM lead_fields WHERE key IN ('industry', 'personalization_note', 'trigger_event', 'trigger_verified');

CREATE TABLE sender_profiles (
    id                        uuid PRIMARY KEY,
    is_default                boolean NOT NULL DEFAULT true,
    version                   integer NOT NULL,
    sender_first_name         text NOT NULL DEFAULT '',
    ideal_customer            text NOT NULL DEFAULT '',
    not_a_fit                 text NOT NULL DEFAULT '',
    problem_solved            text NOT NULL DEFAULT '',
    offer                     text NOT NULL DEFAULT '',
    outcome                   text NOT NULL DEFAULT '',
    proof_or_none             text NOT NULL DEFAULT 'none',
    low_friction_offer        text NOT NULL DEFAULT '',
    allow_low_personalization boolean NOT NULL DEFAULT false,
    other_offers              jsonb NOT NULL DEFAULT '[]',
    sources_used              jsonb NOT NULL DEFAULT '{}',
    created_at                timestamptz NOT NULL DEFAULT now(),
    UNIQUE (is_default, version)
);

CREATE TABLE personalization_sources (
    id          uuid PRIMARY KEY,
    kind        text NOT NULL CHECK (kind IN ('url', 'cv', 'text', 'detail', 'guide')),
    tag         text NOT NULL,
    origin      text,
    clean_text  text NOT NULL,
    token_count integer NOT NULL DEFAULT 0,
    created_at  timestamptz NOT NULL DEFAULT now()
);

ALTER TABLE sequences
    ADD COLUMN sender_profile_id  uuid REFERENCES sender_profiles (id) ON DELETE SET NULL,
    ADD COLUMN ai_prompt_kind     text NOT NULL DEFAULT 'custom' CHECK (ai_prompt_kind IN ('optimized', 'custom')),
    ADD COLUMN ai_prompt_core     text CHECK (ai_prompt_core IS NULL OR char_length(ai_prompt_core) <= 16000),
    ADD COLUMN ai_prompt_primary  text CHECK (ai_prompt_primary IS NULL OR char_length(ai_prompt_primary) <= 16000),
    ADD COLUMN ai_prompt_followup text CHECK (ai_prompt_followup IS NULL OR char_length(ai_prompt_followup) <= 16000),
    ADD COLUMN ai_prompt_version  integer;

ALTER TABLE lead_emails
    ADD COLUMN angle_used     text,
    ADD COLUMN used_trigger   boolean,
    ADD COLUMN claims_check   jsonb,
    ADD COLUMN hold_reason    text,
    ADD COLUMN check_failures jsonb;
ALTER TABLE lead_emails DROP CONSTRAINT lead_emails_status_check;
ALTER TABLE lead_emails ADD CONSTRAINT lead_emails_status_check CHECK (status IN ('none', 'writing', 'review', 'ready', 'stale', 'failed', 'sent', 'held'));

ALTER TABLE llm_calls DROP CONSTRAINT llm_calls_purpose_check;
ALTER TABLE llm_calls ADD CONSTRAINT llm_calls_purpose_check CHECK (purpose IN ('slot', 'draft', 'preview', 'test', 'import_mapping', 'lead_email', 'lead_email_test', 'sender_profile'));
