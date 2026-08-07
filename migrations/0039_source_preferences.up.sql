-- Reader-owned source preferences: a global provider order, and a per-series pin that beats it.
--
-- Until now "which source does this series open on" was derived from chapter counts alone, and
-- the only way to override it was a browser-local pin that did not survive a second device.

-- The global half: the reader's provider ranking, densely positioned from 0. Only ranked
-- providers appear here — an absent provider is not "last", it is "no opinion", and resolution
-- falls back to the objective richest-source order for those.
CREATE TABLE user_provider_priority (
  user_id     uuid NOT NULL REFERENCES users(id)     ON DELETE CASCADE,
  provider_id uuid NOT NULL REFERENCES providers(id) ON DELETE CASCADE,
  position    int  NOT NULL,
  PRIMARY KEY (user_id, provider_id)
);

-- Positions are unique per reader so the order can never be ambiguous. Writes replace the whole
-- list inside one transaction, which is what keeps this satisfiable without a deferred check.
CREATE UNIQUE INDEX user_provider_priority_order
  ON user_provider_priority (user_id, position);

-- The per-series half. `ON DELETE SET NULL` is the fallback the reader would want: a merge that
-- retires the pinned `series_sources` row drops the pin and resolution returns to the global
-- order, rather than the entry vanishing or the pin pointing at nothing.
ALTER TABLE watchlist_entries
  ADD COLUMN pinned_source_id uuid REFERENCES series_sources(id) ON DELETE SET NULL;
