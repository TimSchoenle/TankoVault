-- Password reset tokens.
--
-- Like refresh tokens, we store only a SHA-256 hash of the opaque token the user receives
-- by email; the plaintext never touches the database. A token is single-use (`used_at`)
-- and short-lived (`expires_at`). Rows cascade away with the owning user.
CREATE TABLE password_reset_tokens (
  id         uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  user_id    uuid NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  token_hash text NOT NULL UNIQUE,                  -- SHA-256 of the opaque token
  expires_at timestamptz NOT NULL,
  used_at    timestamptz,
  created_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX password_reset_user_idx ON password_reset_tokens (user_id);
