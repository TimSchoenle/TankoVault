-- Remove the series the Manganato family's advertisement card registered.
--
-- ---------------------------------------------------------------------------------------
-- What was wrong
-- ---------------------------------------------------------------------------------------
-- `natomanga.com`, `mangakakalot.gg` and `nelomanga.net` render a sponsored card in the same
-- `div.list-comic-item-wrap` container as every real row of their listings, and it is the first
-- card on both the catalogue and the latest-updates feed. Its link is a rotating `bit.ly` short
-- URL — and `relativize` flattens a foreign host to its path on purpose, so
-- `https://bit.ly/scrailadi` arrived as `/scrailadi`, which reads exactly like a series slug.
-- Every scan registered it: a `series` row titled after whatever the campaign was selling, a
-- `series_sources` row under it, and from then on a fetch that 404s on every fast scan forever.
-- The rotation whose href the banner script had not rewritten yet resolved to the listing page
-- itself, which answers `200` and fails on the missing series title instead.
--
-- `generic::is_series_link` now drops both shapes at parse time. This removes what the scans
-- that ran without it wrote.

-- ---------------------------------------------------------------------------------------
-- What counts as sponsored
-- ---------------------------------------------------------------------------------------
-- Every series on all three domains lives under `/manga/`; the application serves nothing else
-- that a listing card can link to. Anything else under one of these providers was written by the
-- ad card, so the path prefix is the discriminator rather than a frozen list of the campaigns
-- seen so far — the short link rotates, and a list would be stale before this ships.
CREATE TEMP TABLE sponsored_sources (id uuid PRIMARY KEY, series_id uuid NOT NULL) ON COMMIT DROP;
INSERT INTO sponsored_sources (id, series_id)
SELECT ss.id, ss.series_id
  FROM series_sources ss
  JOIN providers p ON p.id = ss.provider_id
 WHERE p.adapter = 'manganato'
   AND ss.source_path NOT LIKE '/manga/%';

-- Chapters cascade from the source row.
DELETE FROM series_sources ss USING sponsored_sources s WHERE ss.id = s.id;

-- ---------------------------------------------------------------------------------------
-- The series left behind
-- ---------------------------------------------------------------------------------------
-- An advertisement matches no real work, so its series row has no other source and is now
-- unreachable — it would sit in Discover under the campaign's name. Only the series the
-- statement above just orphaned are considered: a series left without sources by anything else
-- is not this migration's to judge. Deleting one cascades into the reader-owned tables, so a
-- series any reader has touched is left standing — an empty shelf entry is a far worse outcome
-- than a stale row, and no correct catalogue state is worth taking someone's progress with it.
DELETE FROM series s
 WHERE s.id IN (SELECT series_id FROM sponsored_sources)
   AND NOT EXISTS (SELECT 1 FROM series_sources ss WHERE ss.series_id = s.id)
   AND NOT EXISTS (SELECT 1 FROM watchlist_entries w WHERE w.series_id = s.id)
   AND NOT EXISTS (SELECT 1 FROM read_progress r WHERE r.series_id = s.id);
