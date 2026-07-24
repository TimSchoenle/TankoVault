-- Email confirmation on sign-up.
--
-- A newly registered account must confirm its email address before it can sign in — but only
-- when the deployment can actually send mail. `email_verified_at` records when (and whether)
-- the address was confirmed; it stays NULL until the user clicks the emailed link.
--
-- Existing accounts predate this feature, so they are backfilled as already verified: they
-- were created before confirmation was required and must keep working after the upgrade.
ALTER TABLE users ADD COLUMN email_verified_at timestamptz;
UPDATE users SET email_verified_at = now();

-- Single-use, short-lived confirmation tokens. Mirrors `password_reset_tokens` (migration
-- 0015): only a SHA-256 hash of the opaque token is stored — the plaintext lives only in the
-- email — and rows cascade away with the owning user.
CREATE TABLE email_verification_tokens (
  id         uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  user_id    uuid NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  token_hash text NOT NULL UNIQUE,                  -- SHA-256 of the opaque token
  expires_at timestamptz NOT NULL,
  used_at    timestamptz,
  created_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX email_verification_user_idx ON email_verification_tokens (user_id);
