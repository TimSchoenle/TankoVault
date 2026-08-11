-- Postgres has no `ALTER TYPE ... DROP VALUE`. Removing the label means rebuilding the enum and
-- every column that uses it, which would break any `providers` row already onboarded as this
-- family. Rows must be re-pointed at another adapter kind first; this file deliberately leaves
-- the label in place rather than destroying data to satisfy a rollback.
SELECT 1;
