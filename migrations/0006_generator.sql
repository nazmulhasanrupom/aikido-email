-- Email Generator: an AI writes the personal parts (slots) of a step, per lead.

-- one row only. engine_paused is the global pause flag (shared by all server copies).
CREATE TABLE generator_defaults (
    id            integer PRIMARY KEY CHECK (id = 1),
    settings      jsonb NOT NULL DEFAULT '{}',
    engine_paused boolean NOT NULL DEFAULT false,
    last_tick_at  timestamptz,
    updated_at    timestamptz NOT NULL DEFAULT now()
);
INSERT INTO generator_defaults (id, settings) VALUES (1, '{}');

CREATE TABLE llm_providers (
    id                 uuid PRIMARY KEY,
    kind               text NOT NULL CHECK (kind IN ('anthropic', 'openai_compatible')),
    name               text NOT NULL,
    base_url           text NOT NULL,
    model              text NOT NULL,
    -- AES-256-GCM, AAD = the provider id. NULL: no key (a local Ollama needs none).
    api_key_ciphertext bytea,
    api_key_nonce      bytea,
    key_version        integer,
    -- ok | auth_failed
    status             text NOT NULL DEFAULT 'ok' CHECK (status IN ('ok', 'auth_failed')),
    max_concurrency    integer NOT NULL DEFAULT 4 CHECK (max_concurrency BETWEEN 1 AND 64),
    timeout_secs       integer NOT NULL DEFAULT 60 CHECK (timeout_secs BETWEEN 5 AND 600),
    json_mode          boolean NOT NULL DEFAULT true,
    created_at         timestamptz NOT NULL DEFAULT now(),
    updated_at         timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE generations (
    -- the cursor of the list route (newest first)
    seq           bigint GENERATED ALWAYS AS IDENTITY UNIQUE,
    id            uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    -- a generation is personal data: erasing the lead or the enrollment erases it
    enrollment_id uuid NOT NULL REFERENCES enrollments (id) ON DELETE CASCADE,
    lead_id       uuid NOT NULL REFERENCES leads (id) ON DELETE CASCADE,
    sequence_id   uuid NOT NULL REFERENCES sequences (id),
    step_number   integer NOT NULL,
    -- hash of the step text and slots at the time of generation
    step_hash     text NOT NULL,
    status        text NOT NULL DEFAULT 'pending' CHECK (status IN
        ('pending', 'generating', 'ready', 'pending_review', 'approved', 'rejected', 'failed', 'cancelled', 'stale')),
    slots         jsonb NOT NULL DEFAULT '{}',
    warnings      jsonb NOT NULL DEFAULT '[]',
    due_at        timestamptz,
    attempts      integer NOT NULL DEFAULT 0,
    error_code    text,
    claim_id      uuid,
    claimed_at    timestamptz,
    -- after a transient error: not before this time
    retry_at      timestamptz,
    provider_id   uuid REFERENCES llm_providers (id) ON DELETE SET NULL,
    model         text,
    input_tokens  integer NOT NULL DEFAULT 0,
    output_tokens integer NOT NULL DEFAULT 0,
    cached_tokens integer NOT NULL DEFAULT 0,
    -- the API key that approved, rejected or edited it
    reviewed_by   uuid,
    created_at    timestamptz NOT NULL DEFAULT now(),
    updated_at    timestamptz NOT NULL DEFAULT now(),
    UNIQUE (enrollment_id, step_number, step_hash)
);
CREATE INDEX generations_claim_idx ON generations (status, due_at);
CREATE INDEX generations_lead_idx ON generations (lead_id);
CREATE INDEX generations_sequence_idx ON generations (sequence_id, status);

-- one row per LLM call. Usage and budgets read this. No prompt text and no answer text.
CREATE TABLE llm_calls (
    id            bigserial PRIMARY KEY,
    purpose       text NOT NULL CHECK (purpose IN ('slot', 'draft', 'preview', 'test')),
    provider_id   uuid REFERENCES llm_providers (id) ON DELETE SET NULL,
    model         text NOT NULL,
    campaign_id   uuid,
    input_tokens  integer NOT NULL DEFAULT 0,
    output_tokens integer NOT NULL DEFAULT 0,
    cached_tokens integer NOT NULL DEFAULT 0,
    duration_ms   integer NOT NULL DEFAULT 0,
    ok            boolean NOT NULL,
    error_code    text,
    created_at    timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX llm_calls_created_idx ON llm_calls (created_at);
CREATE INDEX llm_calls_campaign_idx ON llm_calls (campaign_id) WHERE campaign_id IS NOT NULL;

-- A campaign can override `review_required` and set a token budget (total for the campaign).
CREATE TABLE generator_campaigns (
    campaign_id     uuid PRIMARY KEY REFERENCES campaigns (id) ON DELETE CASCADE,
    review_required boolean,
    token_budget    bigint CHECK (token_budget IS NULL OR token_budget >= 0),
    updated_at      timestamptz NOT NULL DEFAULT now()
);

-- ---------- changes to existing tables ----------

-- [{key, instruction, max_words, allow_links, fallback}]
ALTER TABLE sequence_steps
    ADD COLUMN ai_slots  jsonb NOT NULL DEFAULT '[]',
    ADD COLUMN step_hash text;
ALTER TABLE sequences ADD COLUMN brief text;

-- A step that waits for its AI text: why, and since when.
ALTER TABLE enrollments
    ADD COLUMN wait_reason text,
    ADD COLUMN wait_since  timestamptz;
