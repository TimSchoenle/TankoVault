-- A username may not contain `@`, so it can never collide with another account's email
-- address (audit: SECURITY §9, the database half).
--
-- ---------------------------------------------------------------------------------------
-- What this fixes
-- ---------------------------------------------------------------------------------------
-- `users` carries two SEPARATE unique constraints, one on `email` and one on `username`.
-- Nothing stopped user B's *username* from being equal to user A's *email address* — the two
-- indexes never see each other. Login resolves a credential with
--
--     WHERE email = $1 OR username = $1        -- repo::users::find_credentials
--
-- through `fetch_optional`, which does **not** error when the predicate matches more than one
-- row: it silently returns whichever row the executor produced first. So registering
-- `victim@example.com` as a username made the victim's own correct password return a bare
-- `401` — intermittently, depending on the plan — with nothing in the response, the logs or
-- the schema to explain why. The application-level guard (`validate_username`, `[A-Za-z0-9_.-]`)
-- closes the path new registrations take; this closes the invariant itself, for every writer
-- including a hand-run `UPDATE`.
--
-- ---------------------------------------------------------------------------------------
-- Why the constraint is VALIDATED rather than added `NOT VALID`
-- ---------------------------------------------------------------------------------------
-- `NOT VALID` would let this migration always succeed and leave the pre-existing violations —
-- exactly the accounts that are already breaking someone's login — permanently unchecked
-- behind a `VALIDATE CONSTRAINT` follow-up that nobody schedules. A validating `ADD CONSTRAINT`
-- scans `users` under an `ACCESS EXCLUSIVE` lock, which is the reason to avoid it on a large
-- table; `users` holds one row per account and is the smallest table in the schema, so the
-- scan is milliseconds and the trade does not apply.
--
-- **If this migration fails, it has found the bug.** Identify and rename the offending
-- accounts, then re-run — the migration is idempotent and safe to retry:
--
--   SELECT id, username, email FROM users WHERE position('@' in username::text) > 0;
--   UPDATE users SET username = '<new-name>' WHERE id = '<id>';
--
-- Renaming is the correct remedy rather than widening the constraint: the account whose
-- username is someone else's address is the one that must move.
--
-- ---------------------------------------------------------------------------------------
-- Conventions
-- ---------------------------------------------------------------------------------------
-- - `username::text` because the column is `citext`, for which `position(unknown in citext)`
--   is ambiguous. The cast is binary and costs nothing.
-- - Wrapped in `DO` because Postgres has no `ALTER TABLE ... ADD CONSTRAINT IF NOT EXISTS`,
--   and every new migration should be replayable (audit: OPS §6.5).
-- - No `CONCURRENTLY` anywhere in this file, and none is possible: see the header of
--   `0020_performance_indexes.sql` for why this migrator cannot run it at all.
DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
        WHERE conrelid = 'users'::regclass AND conname = 'username_not_an_email'
    ) THEN
        ALTER TABLE users
            ADD CONSTRAINT username_not_an_email
            CHECK (position('@' in username::text) = 0);
    END IF;
END
$$;
