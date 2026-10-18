-- UI update map v2, B13: emails saved per lead and campaign, step variants, runs.

ALTER TABLE campaigns
    ADD COLUMN content_mode text NOT NULL DEFAULT 'template' CHECK (content_mode IN ('template', 'ai')),
    ADD COLUMN auto_write boolean NOT NULL DEFAULT true,
    ADD COLUMN footer text CHECK (footer IS NULL OR char_length(footer) <= 2000),
    -- set when the emails were written once (Save). New leads get their emails at once after that.
    ADD COLUMN emails_saved_at timestamptz;

ALTER TABLE sequences ADD COLUMN ai_system_prompt text CHECK (ai_system_prompt IS NULL OR char_length(ai_system_prompt) <= 8000);
ALTER TABLE sequence_steps ADD COLUMN ai_user_prompt text CHECK (ai_user_prompt IS NULL OR char_length(ai_user_prompt) <= 8000);

-- Variants of the first email. Variant A is the text of step 1 itself, so only B to E are stored here.
CREATE TABLE step_variants (
    id          uuid PRIMARY KEY,
    sequence_id uuid NOT NULL REFERENCES sequences (id) ON DELETE CASCADE,
    key         text NOT NULL CHECK (key IN ('B', 'C', 'D', 'E')),
    name        text NOT NULL DEFAULT '',
    subject     text NOT NULL DEFAULT '',
    body_text   text,
    body_html   text,
    ai_slots    jsonb NOT NULL DEFAULT '[]',
    active      boolean NOT NULL DEFAULT true,
    position    integer NOT NULL DEFAULT 0,
    UNIQUE (sequence_id, key)
);

CREATE TABLE lead_email_runs (
    id          uuid PRIMARY KEY,
    campaign_id uuid NOT NULL REFERENCES campaigns (id) ON DELETE CASCADE,
    kind        text NOT NULL CHECK (kind IN ('ai', 'template')),
    steps       integer[] NOT NULL DEFAULT '{}',
    scope       jsonb NOT NULL DEFAULT '{}',
    overwrite   text NOT NULL DEFAULT 'skip' CHECK (overwrite IN ('skip', 'keep_edited', 'all')),
    status      text NOT NULL DEFAULT 'queued' CHECK (status IN ('queued', 'running', 'done', 'cancelled', 'failed')),
    total       integer NOT NULL DEFAULT 0,
    done        integer NOT NULL DEFAULT 0,
    failed      integer NOT NULL DEFAULT 0,
    tokens      bigint NOT NULL DEFAULT 0,
    created_by  uuid,
    created_at  timestamptz NOT NULL DEFAULT now(),
    started_at  timestamptz,
    finished_at timestamptz
);
CREATE INDEX lead_email_runs_campaign_idx ON lead_email_runs (campaign_id, created_at DESC);

CREATE TABLE lead_emails (
    id             uuid PRIMARY KEY,
    campaign_id    uuid NOT NULL REFERENCES campaigns (id) ON DELETE CASCADE,
    lead_id        uuid NOT NULL REFERENCES leads (id) ON DELETE CASCADE,
    step_number    integer NOT NULL CHECK (step_number BETWEEN 1 AND 10),
    variant_key    text,
    subject        text NOT NULL DEFAULT '',
    body_text      text,
    body_html      text,
    source         text NOT NULL CHECK (source IN ('template', 'ai', 'manual')),
    status         text NOT NULL CHECK (status IN ('none', 'writing', 'review', 'ready', 'stale', 'failed', 'sent')),
    edited_by_hand boolean NOT NULL DEFAULT false,
    -- hash of the template (or prompt), the lead values it used and the footer
    input_hash     text,
    error_code     text,
    tokens_in      integer NOT NULL DEFAULT 0,
    tokens_out     integer NOT NULL DEFAULT 0,
    job_id         uuid,
    run_id         uuid REFERENCES lead_email_runs (id) ON DELETE SET NULL,
    created_at     timestamptz NOT NULL DEFAULT now(),
    updated_at     timestamptz NOT NULL DEFAULT now(),
    UNIQUE (campaign_id, lead_id, step_number)
);
CREATE INDEX lead_emails_lead_idx ON lead_emails (lead_id);
CREATE INDEX lead_emails_status_idx ON lead_emails (campaign_id, step_number, status);
