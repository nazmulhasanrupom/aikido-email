CREATE EXTENSION IF NOT EXISTS citext;

CREATE TYPE email_provider AS ENUM ('gmail', 'microsoft', 'smtp');
CREATE TYPE account_status AS ENUM ('active', 'paused', 'error', 'disconnected');
CREATE TYPE tls_mode AS ENUM ('implicit', 'starttls', 'none');
CREATE TYPE job_status AS ENUM ('queued', 'sending', 'sent', 'failed', 'cancelled', 'skipped', 'stuck');
CREATE TYPE suppression_kind AS ENUM ('email', 'domain');
CREATE TYPE suppression_reason AS ENUM ('unsubscribed', 'hard_bounce', 'replied', 'complained', 'manual');

CREATE TABLE api_keys (
    id           uuid PRIMARY KEY,
    name         text NOT NULL,
    scope        text NOT NULL,
    key_prefix   text NOT NULL,
    key_hash     bytea NOT NULL UNIQUE,
    created_at   timestamptz NOT NULL DEFAULT now(),
    last_used_at timestamptz,
    revoked_at   timestamptz
);

CREATE TABLE email_accounts (
    id                uuid PRIMARY KEY,
    email             citext NOT NULL,
    provider          email_provider NOT NULL,
    display_name      text NOT NULL DEFAULT '',
    status            account_status NOT NULL DEFAULT 'active',
    smtp_host         text,
    smtp_port         integer,
    smtp_tls          tls_mode,
    imap_host         text,
    imap_port         integer,
    imap_tls          tls_mode,
    secret_ciphertext bytea NOT NULL,
    secret_nonce      bytea NOT NULL,
    key_version       integer NOT NULL,
    token_expires_at  timestamptz,
    last_error        text,
    created_at        timestamptz NOT NULL DEFAULT now(),
    updated_at        timestamptz NOT NULL DEFAULT now(),
    deleted_at        timestamptz
);
CREATE UNIQUE INDEX email_accounts_email_provider_uq
    ON email_accounts (email, provider) WHERE deleted_at IS NULL;

-- one row only. engine_paused is the global pause flag (shared by all server copies).
CREATE TABLE sending_defaults (
    id            integer PRIMARY KEY CHECK (id = 1),
    settings      jsonb NOT NULL DEFAULT '{}',
    engine_paused boolean NOT NULL DEFAULT false,
    updated_at    timestamptz NOT NULL DEFAULT now()
);
INSERT INTO sending_defaults (id, settings) VALUES (1, '{}');

CREATE TABLE account_sending_settings (
    account_id uuid PRIMARY KEY REFERENCES email_accounts (id),
    settings   jsonb NOT NULL DEFAULT '{}',
    updated_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE account_send_state (
    account_id           uuid PRIMARY KEY REFERENCES email_accounts (id),
    next_send_at         timestamptz,
    local_day            date,
    sent_today           integer NOT NULL DEFAULT 0,
    today_limit          integer,
    warmup_day           integer NOT NULL DEFAULT 0,
    consecutive_failures integer NOT NULL DEFAULT 0,
    paused               boolean NOT NULL DEFAULT false,
    paused_reason        text,
    paused_at            timestamptz,
    last_sent_at         timestamptz
);

CREATE TABLE send_jobs (
    id                  uuid PRIMARY KEY,
    account_id          uuid NOT NULL REFERENCES email_accounts (id),
    idempotency_key     text UNIQUE,
    to_email            citext NOT NULL,
    to_name             text,
    recipient_domain    text NOT NULL,
    subject             text NOT NULL,
    body_html           text,
    body_text           text,
    headers             jsonb NOT NULL DEFAULT '{}',
    in_reply_to         text,
    "references"        text,
    lead_id             uuid,
    campaign_id         uuid,
    metadata            jsonb NOT NULL DEFAULT '{}',
    priority            smallint NOT NULL DEFAULT 0,
    send_after          timestamptz,
    status              job_status NOT NULL DEFAULT 'queued',
    attempts            integer NOT NULL DEFAULT 0,
    last_error          text,
    message_id_header   text,
    provider_message_id text,
    provider_thread_id  text,
    created_at          timestamptz NOT NULL DEFAULT now(),
    claimed_at          timestamptz,
    sent_at             timestamptz,
    updated_at          timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX send_jobs_queue_idx ON send_jobs (account_id, priority DESC, created_at) WHERE status = 'queued';
CREATE INDEX send_jobs_account_sent_idx ON send_jobs (account_id, sent_at);
CREATE INDEX send_jobs_account_domain_sent_idx ON send_jobs (account_id, recipient_domain, sent_at);
-- needed for the "same recipient cooldown", which looks across all accounts
CREATE INDEX send_jobs_recipient_sent_idx ON send_jobs (to_email, sent_at);
CREATE INDEX send_jobs_status_idx ON send_jobs (status, claimed_at) WHERE status = 'sending';

CREATE TABLE suppressions (
    value      citext NOT NULL,
    kind       suppression_kind NOT NULL,
    reason     suppression_reason NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (value, kind)
);

CREATE TABLE send_events (
    id         bigserial PRIMARY KEY,
    job_id     uuid,
    account_id uuid NOT NULL,
    event      text NOT NULL,
    detail     jsonb NOT NULL DEFAULT '{}',
    at         timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX send_events_account_at_idx ON send_events (account_id, at);
CREATE INDEX send_events_job_idx ON send_events (job_id);
