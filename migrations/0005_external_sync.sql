-- External sync (AniList). Access tokens are encrypted at rest by the sync service
-- (envelope encryption, AES-GCM) before insertion; the column holds ciphertext.
CREATE TABLE external_accounts (
  user_id       uuid NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  provider      text NOT NULL,                      -- 'anilist'
  access_token  bytea NOT NULL,                     -- encrypted at rest
  refresh_token bytea,
  expires_at    timestamptz,
  created_at    timestamptz NOT NULL DEFAULT now(),
  PRIMARY KEY (user_id, provider)
);

CREATE TABLE sync_mappings (
  series_id   uuid NOT NULL REFERENCES series(id) ON DELETE CASCADE,
  provider    text NOT NULL,
  external_id text NOT NULL,
  PRIMARY KEY (series_id, provider)
);
CREATE INDEX sync_mappings_external_idx ON sync_mappings (provider, external_id);
