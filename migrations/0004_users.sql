-- Users & tracking.
CREATE TABLE users (
  id            uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  email         citext NOT NULL UNIQUE,
  username      citext NOT NULL UNIQUE,
  password_hash text NOT NULL,                     -- argon2id
  role          user_role NOT NULL DEFAULT 'user',
  created_at    timestamptz NOT NULL DEFAULT now()
);

-- Rotating refresh tokens. We store only a hash; a reused token revokes its family.
CREATE TABLE refresh_tokens (
  id         uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  user_id    uuid NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  family_id  uuid NOT NULL,                         -- rotation lineage for reuse detection
  token_hash text NOT NULL UNIQUE,                  -- SHA-256 of the opaque token
  expires_at timestamptz NOT NULL,
  revoked_at timestamptz,
  created_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX refresh_user_idx   ON refresh_tokens (user_id);
CREATE INDEX refresh_family_idx ON refresh_tokens (family_id);

CREATE TABLE watchlist_entries (
  user_id   uuid NOT NULL REFERENCES users(id)  ON DELETE CASCADE,
  series_id uuid NOT NULL REFERENCES series(id) ON DELETE CASCADE,
  status    watch_status NOT NULL DEFAULT 'reading',
  notify    boolean NOT NULL DEFAULT true,
  added_at  timestamptz NOT NULL DEFAULT now(),
  PRIMARY KEY (user_id, series_id)
);
CREATE INDEX watchlist_series_notify ON watchlist_entries (series_id) WHERE notify;

CREATE TABLE read_progress (
  user_id          uuid NOT NULL REFERENCES users(id)  ON DELETE CASCADE,
  series_id        uuid NOT NULL REFERENCES series(id) ON DELETE CASCADE,
  last_read_number numeric(10,4) NOT NULL,
  updated_at       timestamptz NOT NULL DEFAULT now(),
  PRIMARY KEY (user_id, series_id)
);

CREATE TABLE notifications (
  id         uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  user_id    uuid NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  kind       text NOT NULL,                         -- 'new_chapter', 'series_completed', ...
  payload    jsonb NOT NULL,
  read_at    timestamptz,
  created_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX notifications_user_unread ON notifications (user_id, created_at DESC)
  WHERE read_at IS NULL;

-- Idempotency guard so overlapping providers never double-notify a user for the
-- same (series, chapter). Enforced by the notifier before inserting a row.
CREATE TABLE notification_dedup (
  user_id        uuid NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  series_id      uuid NOT NULL REFERENCES series(id) ON DELETE CASCADE,
  chapter_number numeric(10,4) NOT NULL,
  created_at     timestamptz NOT NULL DEFAULT now(),
  PRIMARY KEY (user_id, series_id, chapter_number)
);
