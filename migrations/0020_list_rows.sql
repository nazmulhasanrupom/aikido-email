-- A list can hold its own rows: a CSV imported as it is, with any columns.
-- The rows are not leads. Editing them changes only the list. They become leads when they go to a campaign
-- (through the normal column mapping of an import).

-- the column names of the list, in order
ALTER TABLE lead_lists ADD COLUMN columns jsonb NOT NULL DEFAULT '[]';

CREATE TABLE lead_list_rows (
    id         uuid PRIMARY KEY,
    list_id    uuid NOT NULL REFERENCES lead_lists (id) ON DELETE CASCADE,
    -- column name -> text
    data       jsonb NOT NULL DEFAULT '{}',
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now()
);
-- ids are v7, so this is also the file order
CREATE INDEX lead_list_rows_list_idx ON lead_list_rows (list_id, id);
