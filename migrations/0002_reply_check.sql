-- Reply Check: read the inboxes, find replies and bounces, act on them.

CREATE TYPE job_kind AS ENUM ('automated', 'manual');
CREATE TYPE bounce_kind AS ENUM ('hard', 'soft', 'block');
CREATE TYPE inbound_label AS ENUM (
    'reply', 'auto_reply', 'unsubscribe', 'bounce_hard', 'bounce_soft',
    'bounce_block', 'complaint', 'delayed', 'unknown'
);
CREATE TYPE label_source AS ENUM ('auto', 'manual');
CREATE TYPE match_method AS ENUM (
    'dsn_original', 'in_reply_to', 'references', 'thread',
    'bounce_recipient', 'sender_address', 'none'
);

-- ---------- changes to existing tables ----------

-- Old accounts have no read scope: can_read stays false until the poller checks them
-- (read_status NULL = not checked yet) or the user connects them again.
ALTER TABLE email_accounts
    ADD COLUMN granted_scopes text[] NOT NULL DEFAULT '{}',
    ADD COLUMN can_read       boolean NOT NULL DEFAULT false,
    ADD COLUMN read_status    text;

ALTER TABLE send_jobs
    ADD COLUMN kind        job_kind NOT NULL DEFAULT 'automated',
    ADD COLUMN replied_at  timestamptz,
    ADD COLUMN bounced_at  timestamptz,
    ADD COLUMN bounce_kind bounce_kind;

-- the inbound email that caused the suppression
ALTER TABLE suppressions ADD COLUMN source_message_id uuid;

CREATE INDEX send_jobs_message_id_idx ON send_jobs (message_id_header) WHERE message_id_header IS NOT NULL;
CREATE INDEX send_jobs_thread_idx ON send_jobs (account_id, provider_thread_id) WHERE provider_thread_id IS NOT NULL;
CREATE INDEX send_jobs_account_to_sent_idx ON send_jobs (account_id, to_email, sent_at);

-- ---------- new tables ----------

-- one row only
CREATE TABLE reply_defaults (
    id            integer PRIMARY KEY CHECK (id = 1),
    settings      jsonb NOT NULL DEFAULT '{}',
    engine_paused boolean NOT NULL DEFAULT false,
    updated_at    timestamptz NOT NULL DEFAULT now()
);
INSERT INTO reply_defaults (id, settings) VALUES (1, '{}');

CREATE TABLE account_reply_settings (
    account_id uuid PRIMARY KEY REFERENCES email_accounts (id),
    settings   jsonb NOT NULL DEFAULT '{}',
    updated_at timestamptz NOT NULL DEFAULT now()
);

-- one row per account per folder: 'inbox' | 'spam' | 'all' (Gmail)
CREATE TABLE inbox_sync_state (
    account_id         uuid NOT NULL REFERENCES email_accounts (id),
    folder             text NOT NULL,
    cursor             jsonb,
    paused             boolean NOT NULL DEFAULT false,
    next_sync_at       timestamptz,
    sync_started_at    timestamptz,
    last_success_at    timestamptz,
    last_error         text,
    consecutive_errors integer NOT NULL DEFAULT 0,
    PRIMARY KEY (account_id, folder)
);
CREATE INDEX inbox_sync_state_next_idx ON inbox_sync_state (next_sync_at);

CREATE TABLE inbound_messages (
    id                  uuid PRIMARY KEY,
    account_id          uuid NOT NULL REFERENCES email_accounts (id),
    folder              text NOT NULL,
    provider_message_id text NOT NULL,
    message_id_header   text,
    in_reply_to         text,
    references_ids      text[] NOT NULL DEFAULT '{}',
    provider_thread_id  text,
    from_email          citext NOT NULL,
    from_name           text,
    subject             text,
    received_at         timestamptz NOT NULL,
    label               inbound_label NOT NULL,
    label_reason        text,
    label_source        label_source NOT NULL DEFAULT 'auto',
    match_method        match_method NOT NULL,
    matched_job_id      uuid REFERENCES send_jobs (id),
    lead_id             uuid,
    campaign_id         uuid,
    bounce_recipient    citext,
    bounce_status       text,
    bounce_diagnostic   text,
    reply_text          text,
    truncated           boolean NOT NULL DEFAULT false,
    actions             jsonb NOT NULL DEFAULT '[]',
    created_at          timestamptz NOT NULL DEFAULT now(),
    updated_at          timestamptz NOT NULL DEFAULT now(),
    UNIQUE (account_id, provider_message_id)
);
CREATE INDEX inbound_messages_account_received_idx ON inbound_messages (account_id, received_at);
CREATE INDEX inbound_messages_label_received_idx ON inbound_messages (label, received_at);
CREATE INDEX inbound_messages_job_idx ON inbound_messages (matched_job_id);
CREATE INDEX inbound_messages_lead_idx ON inbound_messages (lead_id);
