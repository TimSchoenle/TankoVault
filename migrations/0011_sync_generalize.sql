-- Generalize external sync for multiple providers + admin visibility (design: sync tab).
-- `last_error` records the most recent sync failure for a linked account (cleared on the next
-- successful sync); `sync_mappings.updated_at` lets the admin console show mapping freshness.
ALTER TABLE external_accounts
  ADD COLUMN last_error text;

ALTER TABLE sync_mappings
  ADD COLUMN updated_at timestamptz NOT NULL DEFAULT now();
