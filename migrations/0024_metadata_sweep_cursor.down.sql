-- Restore the ascending keyset index the old `updated_at` enrichment cursor needed before
-- dropping what replaced it, so a rolled-back deployment is never left without either.
CREATE INDEX IF NOT EXISTS series_enrichment_cursor_idx
    ON series (updated_at ASC, id ASC);

DROP INDEX IF EXISTS series_metadata_sweep_idx;
ALTER TABLE series DROP COLUMN IF EXISTS metadata_checked_at;
