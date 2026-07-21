-- Snapshot of every entry a pull fetches from an external provider (design §15, admin Sync
-- console). Persisting the whole remote list — not just the ones the auto-matcher linked —
-- lets an operator review and hand-assign the leftovers, so *all* loaded entries can be
-- reconciled to a local series rather than silently dropped.
--
-- `series_id` is the canonical series the entry resolved to, or NULL when the auto-matcher
-- was not confident (the "unmatched" queue). The stored `status`/`progress` mirror the
-- provider snapshot so an assignment can import the entry without a fresh pull.
CREATE TABLE sync_remote_entries (
  user_id      uuid NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  provider     text NOT NULL,
  external_id  text NOT NULL,
  title        text NOT NULL,
  status       text NOT NULL,                                   -- local WatchStatus token
  progress     double precision NOT NULL DEFAULT 0,
  content_type text NOT NULL DEFAULT 'unknown',
  start_year   int,
  updated_at   timestamptz NOT NULL,                            -- provider's last-change time
  series_id    uuid REFERENCES series(id) ON DELETE SET NULL,   -- matched series, NULL if not
  fetched_at   timestamptz NOT NULL DEFAULT now(),
  PRIMARY KEY (user_id, provider, external_id)
);

-- The assign queue reads unmatched rows per provider; a partial index keeps it cheap.
CREATE INDEX sync_remote_entries_unmatched_idx
  ON sync_remote_entries (provider)
  WHERE series_id IS NULL;
