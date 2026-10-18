-- Verify on import: runs made by the server itself for new leads.
ALTER TABLE verification_runs ADD COLUMN auto boolean NOT NULL DEFAULT false;
