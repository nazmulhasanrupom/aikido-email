-- Apps (integrations) and the Reoon email verifier.

ALTER TYPE campaign_skip_reason ADD VALUE IF NOT EXISTS 'verification_not_allowed';

CREATE TABLE integrations (
    id                uuid PRIMARY KEY,
    app               text NOT NULL UNIQUE CHECK (app IN ('reoon')),
    status            text NOT NULL DEFAULT 'installed' CHECK (status IN ('installed', 'auth_failed', 'disabled')),
    -- AES-256-GCM, AAD = the row id
    secret_ciphertext bytea NOT NULL,
    secret_nonce      bytea NOT NULL,
    key_version       integer NOT NULL,
    settings          jsonb NOT NULL DEFAULT '{}',
    installed_by      uuid REFERENCES users (id) ON DELETE SET NULL,
    installed_at      timestamptz NOT NULL DEFAULT now(),
    updated_at        timestamptz NOT NULL DEFAULT now()
);

-- one row only. Workspace settings of the verifier (mode, concurrency, reuse_days, send rule) outlive an uninstall.
CREATE TABLE verify_defaults (
    id            integer PRIMARY KEY CHECK (id = 1),
    settings      jsonb NOT NULL DEFAULT '{}',
    engine_paused boolean NOT NULL DEFAULT false,
    last_tick_at  timestamptz,
    updated_at    timestamptz NOT NULL DEFAULT now()
);
INSERT INTO verify_defaults (id, settings) VALUES (1, '{}');

CREATE TABLE verification_runs (
    id            uuid PRIMARY KEY,
    provider      text NOT NULL DEFAULT 'reoon',
    mode          text NOT NULL CHECK (mode IN ('quick', 'power')),
    status        text NOT NULL DEFAULT 'queued' CHECK (status IN ('queued', 'running', 'done', 'cancelled', 'failed')),
    force         boolean NOT NULL DEFAULT false,
    total         integer NOT NULL DEFAULT 0,
    done_count    integer NOT NULL DEFAULT 0,
    failed_count  integer NOT NULL DEFAULT 0,
    skipped_count integer NOT NULL DEFAULT 0,
    credits_used  integer NOT NULL DEFAULT 0,
    warning       text,
    error         text,
    idempotency_key text UNIQUE,
    created_by    uuid,
    created_at    timestamptz NOT NULL DEFAULT now(),
    finished_at   timestamptz
);
CREATE INDEX verification_runs_open_idx ON verification_runs (status) WHERE status IN ('queued', 'running');

CREATE TABLE verification_items (
    run_id     uuid NOT NULL REFERENCES verification_runs (id) ON DELETE CASCADE,
    lead_id    uuid NOT NULL REFERENCES leads (id) ON DELETE CASCADE,
    email      text NOT NULL,
    state      text NOT NULL DEFAULT 'pending' CHECK (state IN ('pending', 'claimed', 'done', 'failed', 'skipped')),
    attempts   integer NOT NULL DEFAULT 0,
    claim_id   uuid,
    claimed_at timestamptz,
    error      text,
    PRIMARY KEY (run_id, lead_id)
);
CREATE INDEX verification_items_pending_idx ON verification_items (run_id, state) WHERE state IN ('pending', 'claimed');

-- Only the newest result per lead is kept. Older results live on the lead timeline.
CREATE TABLE email_verifications (
    id           uuid PRIMARY KEY,
    lead_id      uuid NOT NULL REFERENCES leads (id) ON DELETE CASCADE,
    email        text NOT NULL CHECK (email = lower(email)),
    provider     text NOT NULL DEFAULT 'reoon',
    mode         text NOT NULL CHECK (mode IN ('quick', 'power')),
    status       text NOT NULL,
    score        integer,
    safe_to_send boolean,
    flags        jsonb NOT NULL DEFAULT '{}',
    checked_at   timestamptz NOT NULL DEFAULT now(),
    run_id       uuid REFERENCES verification_runs (id) ON DELETE SET NULL
);
CREATE UNIQUE INDEX email_verifications_lead_provider_idx ON email_verifications (lead_id, provider);

-- null: use the workspace rule
ALTER TABLE campaigns ADD COLUMN verification_rule jsonb;
