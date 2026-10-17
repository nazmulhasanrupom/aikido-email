-- Follow-up automation: sequences of emails, one enrollment per recipient.

CREATE TYPE sequence_status AS ENUM ('active', 'paused', 'archived');
CREATE TYPE delay_type AS ENUM ('business_days', 'calendar_days');
CREATE TYPE enrollment_status AS ENUM ('active', 'waiting_send', 'paused', 'finished', 'stopped', 'failed');

CREATE TABLE sequences (
    id                    uuid PRIMARY KEY,
    name                  text NOT NULL,
    status                sequence_status NOT NULL DEFAULT 'active',
    stop_on_auto_reply    boolean NOT NULL DEFAULT false,
    auto_reply_pause_days integer NOT NULL DEFAULT 3,
    thread_followups      boolean NOT NULL DEFAULT true,
    campaign_id           uuid,
    created_at            timestamptz NOT NULL DEFAULT now(),
    updated_at            timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX sequences_created_idx ON sequences (id DESC);

CREATE TABLE sequence_steps (
    id           uuid PRIMARY KEY,
    sequence_id  uuid NOT NULL REFERENCES sequences (id),
    step_number  integer NOT NULL CHECK (step_number BETWEEN 1 AND 10),
    delay_amount integer NOT NULL DEFAULT 0 CHECK (delay_amount >= 0),
    delay_type   delay_type NOT NULL DEFAULT 'business_days',
    subject      text NOT NULL DEFAULT '',
    body_text    text,
    body_html    text,
    updated_at   timestamptz NOT NULL DEFAULT now(),
    UNIQUE (sequence_id, step_number)
);

CREATE TABLE enrollments (
    id              uuid PRIMARY KEY,
    sequence_id     uuid NOT NULL REFERENCES sequences (id),
    account_id      uuid NOT NULL REFERENCES email_accounts (id),
    to_email        citext NOT NULL,
    variables       jsonb NOT NULL DEFAULT '{}',
    status          enrollment_status NOT NULL DEFAULT 'active',
    next_step       integer NOT NULL DEFAULT 1,
    next_due_at     timestamptz,
    -- the send job of the step that is queued or being sent (no foreign key: see below)
    current_job_id  uuid,
    -- when the current step became a job. The stall check reads it.
    step_queued_at  timestamptz,
    -- why the enrollment ended (`stopped` and `failed`): reply, bounce_hard, unsubscribe, manual, stalled, ...
    stop_reason     text,
    pause_until     timestamptz,
    idempotency_key text UNIQUE,
    lead_id         uuid,
    created_at      timestamptz NOT NULL DEFAULT now(),
    updated_at      timestamptz NOT NULL DEFAULT now()
);
-- the scheduler
CREATE INDEX enrollments_due_idx ON enrollments (status, next_due_at);
-- "one open enrollment per address" and the stop lookups
CREATE INDEX enrollments_open_email_idx ON enrollments (to_email) WHERE status IN ('active', 'waiting_send', 'paused');
CREATE INDEX enrollments_sequence_idx ON enrollments (sequence_id, status);
CREATE INDEX enrollments_id_desc_idx ON enrollments (id DESC);

CREATE TABLE enrollment_events (
    id            bigserial PRIMARY KEY,
    enrollment_id uuid NOT NULL REFERENCES enrollments (id),
    kind          text NOT NULL,
    step_number   integer,
    detail        jsonb NOT NULL DEFAULT '{}',
    at            timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX enrollment_events_enrollment_idx ON enrollment_events (enrollment_id, id);

-- one row only. engine_paused is the global pause flag (shared by all server copies).
CREATE TABLE followup_defaults (
    id            integer PRIMARY KEY CHECK (id = 1),
    settings      jsonb NOT NULL DEFAULT '{}',
    engine_paused boolean NOT NULL DEFAULT false,
    last_tick_at  timestamptz,
    updated_at    timestamptz NOT NULL DEFAULT now()
);
INSERT INTO followup_defaults (id, settings) VALUES (1, '{}');

-- ---------- changes to existing tables ----------

-- `in_reply_to` and "references" already exist on send_jobs (0001): the follow-up step uses them.
ALTER TABLE send_jobs
    ADD COLUMN enrollment_id uuid REFERENCES enrollments (id),
    ADD COLUMN step_number   integer;
CREATE INDEX send_jobs_enrollment_idx ON send_jobs (enrollment_id, step_number) WHERE enrollment_id IS NOT NULL;
ALTER TABLE enrollments ADD CONSTRAINT enrollments_current_job_fk FOREIGN KEY (current_job_id) REFERENCES send_jobs (id);
