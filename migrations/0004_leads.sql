-- Lead management: leads, custom fields, lists, timeline, imports, MX cache.
-- Campaign tables and the changes to existing tables come later (0005), after approval.

CREATE EXTENSION IF NOT EXISTS pg_trgm;

-- Lead deletion adds the address to `suppressions` with this reason (decision B).
ALTER TYPE suppression_reason ADD VALUE IF NOT EXISTS 'erased';

CREATE TYPE lead_contact_state AS ENUM ('new', 'in_sequence', 'contacted', 'replied', 'bounced', 'unsubscribed', 'completed');
CREATE TYPE lead_interest AS ENUM ('interested', 'meeting_booked', 'won', 'not_interested', 'wrong_person', 'lost');
CREATE TYPE lead_verification AS ENUM ('unknown', 'syntax_ok', 'mx_ok', 'no_mx', 'invalid');
CREATE TYPE lead_source AS ENUM ('api', 'csv', 'enrollment', 'auto_database', 'manual');
CREATE TYPE lead_field_kind AS ENUM ('text', 'number', 'url', 'date');
CREATE TYPE lead_dedupe_mode AS ENUM ('skip', 'fill_empty', 'overwrite');
CREATE TYPE lead_import_status AS ENUM ('pending', 'running', 'done', 'failed', 'cancelled');

CREATE TABLE leads (
    id                uuid PRIMARY KEY,
    -- trimmed and lower case
    email             text NOT NULL CHECK (email = lower(email)),
    email_domain      text NOT NULL,
    first_name        text,
    last_name         text,
    company_name      text,
    job_title         text,
    phone             text,
    website           text,
    linkedin_url      text,
    location          text,
    -- IANA name. Stored only. Nothing reads it yet.
    timezone          text,
    -- keys must exist in lead_fields. Values are text.
    custom_fields     jsonb NOT NULL DEFAULT '{}',
    tags              text[] NOT NULL DEFAULT '{}' CHECK (cardinality(tags) <= 20),
    contact_state     lead_contact_state NOT NULL DEFAULT 'new',
    interest          lead_interest,
    verification      lead_verification NOT NULL DEFAULT 'unknown',
    source            lead_source NOT NULL DEFAULT 'api',
    source_ref        text,
    notes             text,
    last_contacted_at timestamptz,
    last_replied_at   timestamptz,
    created_at        timestamptz NOT NULL DEFAULT now(),
    updated_at        timestamptz NOT NULL DEFAULT now(),
    UNIQUE (email)
);
CREATE INDEX leads_domain_idx ON leads (email_domain);
CREATE INDEX leads_state_idx ON leads (contact_state);
CREATE INDEX leads_interest_idx ON leads (interest) WHERE interest IS NOT NULL;
CREATE INDEX leads_tags_idx ON leads USING gin (tags);
CREATE INDEX leads_email_trgm_idx ON leads USING gin (email gin_trgm_ops);
CREATE INDEX leads_first_name_trgm_idx ON leads USING gin (first_name gin_trgm_ops);
CREATE INDEX leads_last_name_trgm_idx ON leads USING gin (last_name gin_trgm_ops);
CREATE INDEX leads_company_trgm_idx ON leads USING gin (company_name gin_trgm_ops);
-- keyset paging: newest first
CREATE INDEX leads_created_idx ON leads (created_at DESC, id DESC);

CREATE TABLE lead_fields (
    key        text PRIMARY KEY CHECK (key ~ '^[a-z][a-z0-9_]{0,39}$'),
    label      text NOT NULL,
    kind       lead_field_kind NOT NULL DEFAULT 'text',
    -- used in templates when the lead has no value
    fallback   text,
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE lead_lists (
    id          uuid PRIMARY KEY,
    name        text NOT NULL UNIQUE,
    description text,
    created_at  timestamptz NOT NULL DEFAULT now(),
    updated_at  timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE lead_list_members (
    list_id  uuid NOT NULL REFERENCES lead_lists (id) ON DELETE CASCADE,
    lead_id  uuid NOT NULL REFERENCES leads (id) ON DELETE CASCADE,
    added_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (list_id, lead_id)
);
CREATE INDEX lead_list_members_lead_idx ON lead_list_members (lead_id);

-- the timeline
CREATE TABLE lead_events (
    id      bigserial PRIMARY KEY,
    lead_id uuid NOT NULL REFERENCES leads (id) ON DELETE CASCADE,
    -- created, updated, imported, list_added, list_removed, campaign_added, enrolled, step_sent, replied,
    -- auto_replied, bounced, unsubscribed, complained, interest_changed, stopped, note
    kind    text NOT NULL,
    -- the job, enrollment, campaign or import it is about
    ref_id  uuid,
    detail  jsonb NOT NULL DEFAULT '{}',
    at      timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX lead_events_lead_idx ON lead_events (lead_id, id DESC);

CREATE TABLE lead_imports (
    id           uuid PRIMARY KEY,
    kind         text NOT NULL DEFAULT 'csv' CHECK (kind IN ('csv', 'json')),
    file_name    text,
    status       lead_import_status NOT NULL DEFAULT 'pending',
    -- CSV column name -> lead field, `custom:<key>`, or `ignore`
    mapping      jsonb NOT NULL DEFAULT '{}',
    dedupe_mode  lead_dedupe_mode NOT NULL DEFAULT 'fill_empty',
    list_id      uuid REFERENCES lead_lists (id) ON DELETE SET NULL,
    -- the foreign key comes with the campaign tables
    campaign_id  uuid,
    tags         text[] NOT NULL DEFAULT '{}',
    total_rows   integer NOT NULL DEFAULT 0,
    -- data rows already written (with the counters, in one transaction). A restart skips them.
    rows_done    integer NOT NULL DEFAULT 0,
    created      integer NOT NULL DEFAULT 0,
    updated      integer NOT NULL DEFAULT 0,
    skipped      integer NOT NULL DEFAULT 0,
    invalid      integer NOT NULL DEFAULT 0,
    -- the first 1000 bad rows: { row, reason }
    errors       jsonb NOT NULL DEFAULT '[]',
    error        text,
    -- worker lease. `claim_id` is the token of the worker that holds it: only that worker may write the counters.
    claimed_at   timestamptz,
    claim_id     uuid,
    -- how many times a worker took it. After 5 the import fails (a bad file must not loop for ever).
    attempts     integer NOT NULL DEFAULT 0,
    created_at   timestamptz NOT NULL DEFAULT now(),
    finished_at  timestamptz
);
CREATE INDEX lead_imports_status_idx ON lead_imports (status, claimed_at);

-- the uploaded CSV. Deleted when the import ends.
CREATE TABLE lead_import_files (
    import_id uuid PRIMARY KEY REFERENCES lead_imports (id) ON DELETE CASCADE,
    data      bytea NOT NULL
);

-- one lookup per domain, not per lead
CREATE TABLE email_domains (
    domain     text PRIMARY KEY,
    has_mx     boolean NOT NULL,
    checked_at timestamptz NOT NULL
);

-- one row only
CREATE TABLE lead_defaults (
    id            integer PRIMARY KEY CHECK (id = 1),
    settings      jsonb NOT NULL DEFAULT '{}',
    -- the campaign feeder (a later step) reads these two
    feeder_paused boolean NOT NULL DEFAULT false,
    last_tick_at  timestamptz,
    updated_at    timestamptz NOT NULL DEFAULT now()
);
INSERT INTO lead_defaults (id, settings) VALUES (1, '{}');
