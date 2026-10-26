-- Automations, phase 5 (production): triggers, rate limits, and a record of campaign status changes.
-- Build map: AIKIDO_EMAIL_Automations_Build_Map.md, sections 9 and 18 (item 4).

-- Campaign status changes. `trigger.campaign_status_changed` reads this table with a cursor.
-- One row per change: written in the same transaction as the change.
CREATE TABLE campaign_events (
  id          bigserial PRIMARY KEY,
  campaign_id uuid NOT NULL REFERENCES campaigns(id) ON DELETE CASCADE,
  from_status text,
  to_status   text NOT NULL,
  at          timestamptz NOT NULL DEFAULT now()
);

-- The node type of the trigger (`trigger.lead_bounced`), so the tick knows how to read and filter.
ALTER TABLE automation_triggers
  ADD COLUMN trigger_type    text NOT NULL DEFAULT '',
  -- events skipped because the lead was deleted before the run was made
  ADD COLUMN skipped_deleted bigint NOT NULL DEFAULT 0,
  -- set while `max_runs_per_hour` holds the trigger back; cleared when it fires again
  ADD COLUMN rate_limited_at timestamptz;

-- The rate and back-pressure checks count production runs of one workflow.
CREATE INDEX automation_runs_production_idx ON automation_runs (workflow_id, queued_at) WHERE mode = 'production';
