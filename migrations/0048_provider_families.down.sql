-- Postgres has no `ALTER TYPE ... DROP VALUE`. Removing the two labels means rebuilding the
-- enum and every column that uses it, which would break any `providers` row already onboarded
-- as one of these families. Rows must be re-pointed at another adapter kind first; this file
-- deliberately leaves the labels in place rather than destroying data to satisfy a rollback.
SELECT 1;
