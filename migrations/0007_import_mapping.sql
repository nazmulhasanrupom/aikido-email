-- Auto Database part 1: AI column mapping for CSV import.

-- An import that is stored but not confirmed. The worker does not claim it.
ALTER TYPE lead_import_status ADD VALUE IF NOT EXISTS 'uploaded';

-- The last mapping proposal (the confirmed mapping stays in `mapping`).
ALTER TABLE lead_imports ADD COLUMN proposal jsonb;

-- The mapper writes `llm_calls` rows with this purpose.
ALTER TABLE llm_calls DROP CONSTRAINT llm_calls_purpose_check;
ALTER TABLE llm_calls ADD CONSTRAINT llm_calls_purpose_check CHECK (purpose IN ('slot', 'draft', 'preview', 'test', 'import_mapping'));

-- A saved mapping for the same headers: no AI call on the second upload.
CREATE TABLE import_mapping_templates (
    id           uuid PRIMARY KEY,
    -- SHA-256 of the normalized headers, in file order
    header_hash  text NOT NULL UNIQUE,
    headers      jsonb NOT NULL,
    -- [{ index, target, transform }]
    mapping      jsonb NOT NULL,
    used_count   integer NOT NULL DEFAULT 0,
    last_used_at timestamptz,
    created_at   timestamptz NOT NULL DEFAULT now()
);
