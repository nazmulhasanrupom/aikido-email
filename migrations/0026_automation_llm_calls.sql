-- Automations, phase 4: the AI Agent node writes `llm_calls` rows (build map 7.3 and 18.5).
-- New purposes, and three nullable columns that say which run, workflow and node made the call.
-- No foreign keys: `llm_calls` is history and must outlive a deleted run.

ALTER TABLE llm_calls DROP CONSTRAINT llm_calls_purpose_check;
ALTER TABLE llm_calls ADD CONSTRAINT llm_calls_purpose_check CHECK (purpose IN (
    'slot', 'draft', 'preview', 'test', 'import_mapping', 'lead_email', 'lead_email_test', 'sender_profile',
    'automation', 'automation_copilot', 'automation_copilot_summary'
));

ALTER TABLE llm_calls
    ADD COLUMN automation_run_id uuid,
    ADD COLUMN workflow_id       uuid,
    ADD COLUMN node_id           text;

CREATE INDEX llm_calls_automation_run_idx ON llm_calls (automation_run_id) WHERE automation_run_id IS NOT NULL;
CREATE INDEX llm_calls_workflow_idx ON llm_calls (workflow_id, created_at) WHERE workflow_id IS NOT NULL;
