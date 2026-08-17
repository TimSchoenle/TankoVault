-- Rebuild the pre-0055 `chapters`: uuid primary key, `numeric(10,4)` number, whole paths, the
-- `volume` column, and the six indexes.
--
-- Lossy in one direction that cannot be helped: `volume` was never populated by any adapter, so it
-- comes back NULL, and rows dropped by the up-migration for being outside the storable range are
-- gone. Everything else round-trips.

CREATE TABLE chapters_old (
  id               uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  series_source_id uuid NOT NULL REFERENCES series_sources(id) ON DELETE CASCADE,
  number           numeric(10,4) NOT NULL,
  volume           int,
  title            text,
  path             text NOT NULL,
  published_at     timestamptz,
  discovered_at    timestamptz NOT NULL DEFAULT now(),
  access           chapter_access NOT NULL DEFAULT 'free',
  unlocks_at       timestamptz,
  UNIQUE (series_source_id, number)
);

INSERT INTO chapters_old
  (series_source_id, number, volume, title, path, published_at, discovered_at, access, unlocks_at)
SELECT c.series_source_id,
       (c.number_milli::numeric / 10000)::numeric(10,4),
       NULL,
       c.title,
       chapter_url_path(ss.source_path, c.path),
       c.published_at,
       c.discovered_at,
       c.access,
       c.unlocks_at
FROM chapters c
JOIN series_sources ss ON ss.id = c.series_source_id;

DROP TABLE chapters;
ALTER TABLE chapters_old RENAME TO chapters;

ALTER TABLE chapters
  ADD CONSTRAINT chapters_unlocks_at_requires_early_access
  CHECK (access = 'early_access' OR unlocks_at IS NULL);

CREATE INDEX chapters_source_idx ON chapters (series_source_id, number DESC);
CREATE INDEX chapters_discovered ON chapters (discovered_at DESC);
CREATE INDEX chapters_source_floor_num_access_idx
    ON chapters (series_source_id, (floor(number)), number) INCLUDE (access, unlocks_at);
CREATE INDEX chapters_source_disc_access_idx
    ON chapters (series_source_id, discovered_at DESC) INCLUDE (access, unlocks_at);

DROP FUNCTION IF EXISTS chapter_url_path(text, text);

ANALYZE chapters;
