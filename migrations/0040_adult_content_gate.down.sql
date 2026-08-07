ALTER TABLE users DROP CONSTRAINT IF EXISTS users_adult_opt_in_requires_attestation;
ALTER TABLE users
  DROP COLUMN IF EXISTS age_attested_at,
  DROP COLUMN IF EXISTS adult_opt_in;

DROP INDEX IF EXISTS series_adult_gated_idx;
ALTER TABLE series
  DROP COLUMN IF EXISTS adult_gated,
  DROP COLUMN IF EXISTS adult_inferred;
