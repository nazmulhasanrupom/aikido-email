-- Campaigns, and the links from enrollments and sequences to leads and campaigns.

CREATE TYPE campaign_status AS ENUM ('draft', 'active', 'paused', 'completed', 'archived');
CREATE TYPE campaign_account_pick AS ENUM ('least_loaded', 'round_robin');
CREATE TYPE campaign_lead_state AS ENUM ('pending', 'enrolled', 'skipped');
CREATE TYPE campaign_skip_reason AS ENUM (
    'suppressed', 'unverified', 'active_elsewhere', 'no_healthy_account', 'lead_deleted', 'already_in_campaign', 'missing_variable', 'company_replied', 'campaign_archived'
);

CREATE TABLE campaigns (
    id                    uuid PRIMARY KEY,
    name                  text NOT NULL,
    status                campaign_status NOT NULL DEFAULT 'draft',
    -- one campaign, one sequence. `sequences.campaign_id` is the same link seen from the other side.
    sequence_id           uuid NOT NULL UNIQUE REFERENCES sequences (id),
    daily_new_leads       integer NOT NULL DEFAULT 50 CHECK (daily_new_leads BETWEEN 1 AND 5000),
    account_pick          campaign_account_pick NOT NULL DEFAULT 'least_loaded',
    stop_company_on_reply boolean NOT NULL DEFAULT false,
    allow_unverified      boolean NOT NULL DEFAULT false,
    -- the day boundary of `daily_new_leads`
    timezone              text NOT NULL DEFAULT 'UTC',
    -- the last account the feeder used (for round_robin)
    last_account_id       uuid,
    -- why the feeder is stuck, if it is (for example: no healthy account)
    warning               text,
    launched_at           timestamptz,
    completed_at          timestamptz,
    -- feeder lease
    claimed_at            timestamptz,
    created_at            timestamptz NOT NULL DEFAULT now(),
    updated_at            timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX campaigns_status_idx ON campaigns (status, claimed_at);

CREATE TABLE campaign_accounts (
    campaign_id uuid NOT NULL REFERENCES campaigns (id) ON DELETE CASCADE,
    account_id  uuid NOT NULL REFERENCES email_accounts (id),
    PRIMARY KEY (campaign_id, account_id)
);
CREATE INDEX campaign_accounts_account_idx ON campaign_accounts (account_id);

CREATE TABLE campaign_leads (
    campaign_id   uuid NOT NULL REFERENCES campaigns (id) ON DELETE CASCADE,
    lead_id       uuid NOT NULL REFERENCES leads (id) ON DELETE CASCADE,
    state         campaign_lead_state NOT NULL DEFAULT 'pending',
    skip_reason   campaign_skip_reason,
    enrollment_id uuid REFERENCES enrollments (id),
    added_at      timestamptz NOT NULL DEFAULT now(),
    enrolled_at   timestamptz,
    PRIMARY KEY (campaign_id, lead_id)
);
-- the feeder: oldest pending first
CREATE INDEX campaign_leads_feed_idx ON campaign_leads (campaign_id, state, added_at, lead_id);
CREATE INDEX campaign_leads_lead_idx ON campaign_leads (lead_id);
CREATE INDEX campaign_leads_enrollment_idx ON campaign_leads (enrollment_id) WHERE enrollment_id IS NOT NULL;

-- ---------- changes to existing tables (section 12 of the lead map, changes 1 and 2) ----------

-- No leads or campaigns existed before this step, so an id in these columns pointed at nothing.
UPDATE enrollments SET lead_id = NULL WHERE lead_id IS NOT NULL;
UPDATE sequences SET campaign_id = NULL WHERE campaign_id IS NOT NULL;

-- Change 1: an enrollment points to a lead. A deleted lead leaves the enrollment in place.
ALTER TABLE enrollments ADD CONSTRAINT enrollments_lead_fk FOREIGN KEY (lead_id) REFERENCES leads (id) ON DELETE SET NULL;
CREATE INDEX enrollments_lead_idx ON enrollments (lead_id) WHERE lead_id IS NOT NULL;

-- Change 2: a sequence belongs to one campaign at most.
ALTER TABLE sequences ADD CONSTRAINT sequences_campaign_fk FOREIGN KEY (campaign_id) REFERENCES campaigns (id) DEFERRABLE INITIALLY DEFERRED;
CREATE UNIQUE INDEX sequences_campaign_unique_idx ON sequences (campaign_id) WHERE campaign_id IS NOT NULL;

-- The lead_imports table already has a campaign_id column from 0004.
ALTER TABLE lead_imports ADD CONSTRAINT lead_imports_campaign_fk FOREIGN KEY (campaign_id) REFERENCES campaigns (id) ON DELETE SET NULL;
