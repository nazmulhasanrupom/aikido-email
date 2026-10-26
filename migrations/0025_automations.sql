-- Automations (node editor for email work). Build map: AIKIDO_EMAIL_Automations_Build_Map.md, section 12.
-- One migration for all phases: the engine, the triggers, the secrets and the copilot use these tables later.
-- The user ids are set to NULL when a user is deleted (a deleted user must not block `DELETE /v1/users/{id}`).

CREATE TABLE automation_workflows (
  id                   uuid PRIMARY KEY,
  name                 text NOT NULL,
  description          text NOT NULL DEFAULT '',
  draft                jsonb NOT NULL,             -- WorkflowDefinition
  draft_hash           text NOT NULL,              -- sha256 of canonical draft JSON
  draft_valid          boolean NOT NULL DEFAULT false,
  published_version_id uuid,                       -- FK added below
  active               boolean NOT NULL DEFAULT false,
  run_as_user_id       uuid REFERENCES users(id) ON DELETE SET NULL,
  created_by           uuid REFERENCES users(id) ON DELETE SET NULL,
  tags                 text[] NOT NULL DEFAULT '{}',
  archived_at          timestamptz,
  created_at           timestamptz NOT NULL DEFAULT now(),
  updated_at           timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE automation_versions (
  id           uuid PRIMARY KEY,
  workflow_id  uuid NOT NULL REFERENCES automation_workflows(id) ON DELETE CASCADE,
  version      int  NOT NULL,
  definition   jsonb NOT NULL,
  note         text NOT NULL DEFAULT '',
  published_by uuid REFERENCES users(id) ON DELETE SET NULL,
  published_at timestamptz NOT NULL DEFAULT now(),
  UNIQUE (workflow_id, version)
);
ALTER TABLE automation_workflows
  ADD CONSTRAINT automation_workflows_published_fk
  FOREIGN KEY (published_version_id) REFERENCES automation_versions(id);

CREATE TABLE automation_draft_history (
  id          bigserial PRIMARY KEY,
  workflow_id uuid NOT NULL REFERENCES automation_workflows(id) ON DELETE CASCADE,
  draft       jsonb NOT NULL,
  draft_hash  text NOT NULL,
  source      text NOT NULL,           -- 'editor', 'copilot', 'import', 'restore'
  created_by  uuid,
  created_at  timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX ON automation_draft_history (workflow_id, id DESC);

CREATE TABLE automation_triggers (
  id           uuid PRIMARY KEY,
  workflow_id  uuid NOT NULL REFERENCES automation_workflows(id) ON DELETE CASCADE,
  version_id   uuid NOT NULL REFERENCES automation_versions(id),
  node_id      text NOT NULL,
  kind         text NOT NULL,          -- 'schedule','cron','webhook','event'
  event_types  text[] NOT NULL DEFAULT '{}',
  config       jsonb NOT NULL,         -- params with vars resolved
  next_run_at  timestamptz,            -- time triggers
  cursor       jsonb,                  -- event triggers: { id, created_at }
  webhook_path text UNIQUE,            -- webhooks
  last_fired_at timestamptz,
  last_error   text,
  claimed_by   text,
  lease_until  timestamptz,
  UNIQUE (workflow_id, node_id)
);
CREATE INDEX ON automation_triggers (kind, next_run_at);

CREATE TABLE automation_runs (
  id              uuid PRIMARY KEY,
  workflow_id     uuid NOT NULL REFERENCES automation_workflows(id) ON DELETE CASCADE,
  version_id      uuid REFERENCES automation_versions(id),  -- null for test runs of the draft
  definition      jsonb,                                    -- test runs: the draft snapshot used
  mode            text NOT NULL,      -- 'test','production'
  status          text NOT NULL,      -- 'queued','running','waiting','success','error','cancelled'
  trigger_node_id text NOT NULL,
  trigger_item    jsonb NOT NULL,
  options         jsonb NOT NULL DEFAULT '{}',  -- { real_writes, only_node, from_node }
  state           bytea,              -- compressed checkpoint (zstd), see 8.4
  error           jsonb,
  idempotency_key text UNIQUE,        -- 'trigger:<id>:event:<id>' etc.
  started_by      uuid,
  run_as_user_id  uuid,
  attempt         int NOT NULL DEFAULT 0,
  resume_at       timestamptz,
  cancel_requested boolean NOT NULL DEFAULT false,
  claimed_by      text,
  lease_until     timestamptz,
  queued_at       timestamptz NOT NULL DEFAULT now(),
  started_at      timestamptz,
  finished_at     timestamptz,
  active_ms       bigint NOT NULL DEFAULT 0
);
CREATE INDEX ON automation_runs (status, mode, queued_at);
CREATE INDEX ON automation_runs (workflow_id, queued_at DESC);
CREATE INDEX ON automation_runs (status, resume_at) WHERE status = 'waiting';

CREATE TABLE automation_run_nodes (
  run_id       uuid NOT NULL REFERENCES automation_runs(id) ON DELETE CASCADE,
  node_id      text NOT NULL,
  seq          int  NOT NULL,          -- order it ran (a node can run more than once, 8.3 rule 5)
  status       text NOT NULL,          -- 'running','success','error','skipped','dry'
  input_count  int  NOT NULL DEFAULT 0,
  output_count jsonb NOT NULL DEFAULT '{}',  -- per port: { "main": 3, "false": 1 }
  output       jsonb,                  -- per port, cut at 512 KB
  truncated    boolean NOT NULL DEFAULT false,
  error        jsonb,
  started_at   timestamptz NOT NULL,
  finished_at  timestamptz,
  PRIMARY KEY (run_id, node_id, seq)
);

CREATE TABLE automation_run_events (
  id      bigserial PRIMARY KEY,
  run_id  uuid NOT NULL REFERENCES automation_runs(id) ON DELETE CASCADE,
  at      timestamptz NOT NULL DEFAULT now(),
  kind    text NOT NULL,   -- 'node_started','node_finished','node_error','waiting','resumed','node_restarted','log'
  node_id text,
  detail  jsonb NOT NULL DEFAULT '{}'
);
CREATE INDEX ON automation_run_events (run_id, id);

CREATE TABLE automation_secrets (
  name       text PRIMARY KEY,
  value_enc  bytea NOT NULL,
  last4      text NOT NULL,
  created_by uuid,
  created_at timestamptz NOT NULL DEFAULT now(),
  updated_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE automation_test_listens (
  webhook_path text PRIMARY KEY,
  workflow_id  uuid NOT NULL REFERENCES automation_workflows(id) ON DELETE CASCADE,
  node_id      text NOT NULL,
  user_id      uuid,
  until        timestamptz NOT NULL
);

CREATE TABLE automation_copilot_threads (
  id          uuid PRIMARY KEY,
  workflow_id uuid REFERENCES automation_workflows(id) ON DELETE SET NULL,
  user_id     uuid NOT NULL,
  title       text NOT NULL DEFAULT '',
  summary     text NOT NULL DEFAULT '',   -- rolling summary of old turns (16.4)
  created_at  timestamptz NOT NULL DEFAULT now(),
  updated_at  timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE automation_copilot_messages (
  id           bigserial PRIMARY KEY,
  thread_id    uuid NOT NULL REFERENCES automation_copilot_threads(id) ON DELETE CASCADE,
  role         text NOT NULL,          -- 'user','assistant'
  content      text NOT NULL,
  workflow_json jsonb,                 -- what the assistant produced (if any)
  applied      jsonb,                  -- { workflow_id, mode, diff, draft_history_id } or { error }
  tokens_in    int, tokens_out int,
  created_at   timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX ON automation_copilot_messages (thread_id, id);

CREATE TABLE automation_settings (
  id       boolean PRIMARY KEY DEFAULT true CHECK (id),
  settings jsonb NOT NULL DEFAULT '{}',
  paused   boolean NOT NULL DEFAULT false
);
INSERT INTO automation_settings DEFAULT VALUES;
