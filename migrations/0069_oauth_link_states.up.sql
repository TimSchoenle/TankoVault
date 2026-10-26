-- The `state` of an in-flight OAuth link (RFC 6749 §10.12), bound to the account and provider
-- that started it. Server-side so it is single-use: the callback deletes the row it matches,
-- and a replayed callback URL finds nothing. Only the digest of the handle is stored.
CREATE TABLE oauth_link_states (
  token_hash text PRIMARY KEY,
  user_id    uuid NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  provider   text NOT NULL,
  expires_at timestamptz NOT NULL,
  created_at timestamptz NOT NULL DEFAULT now()
);

-- Drives the expiry sweep: an abandoned consent page leaves its row behind.
CREATE INDEX oauth_link_states_expiry_idx ON oauth_link_states (expires_at);

-- Serves the `ON DELETE CASCADE` from `users`: without it, account erasure scans this table once
-- per deleted user (`schema_foreign_key_indexes`).
CREATE INDEX oauth_link_states_user_idx ON oauth_link_states (user_id);
