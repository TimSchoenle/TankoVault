-- The intake vocabulary guard skipped credits, and the recommender's strongest axis paid for it.
--
-- ---------------------------------------------------------------------------------------
-- What was wrong
-- ---------------------------------------------------------------------------------------
-- An aggregator's summary block renders `Genres: Updating` and, from the row directly below it,
-- `Author: Updating`. `add_series_tags` refused the first — that guard has been in place since
-- the tag vocabulary existed, which is why Discover's facet panel is clean. `add_series_authors`
-- had no `blocked` parameter at all, so the second was interned into `authors`, linked, rendered
-- as a credit on the series page, and extracted as an `author` feature.
--
-- `author` is the recommender's heaviest axis (base weight 1.0, against 0.25 for a status) and
-- the one path that retrieves *exactly* rather than approximately, on the argument that two works
-- by one creator is close to a certain recommendation. A placeholder credit shared by a large
-- part of the catalogue therefore did not merely add noise: it went to the top of every reader's
-- taste profile and pulled unrelated series onto every shelf.
--
-- The guard now runs at both writers (`add_series_authors` takes the same `TermBlocklist`). This
-- migration removes what the missing one already wrote.

-- ---------------------------------------------------------------------------------------
-- A pruned vocabulary is a reason to re-derive the recommender's rows
-- ---------------------------------------------------------------------------------------
-- `rec_repair_queue.reason` is a closed set, so the new cause has to be admitted here or the
-- enqueue below fails on the check constraint.
ALTER TABLE rec_repair_queue DROP CONSTRAINT rec_repair_queue_reason_check;
ALTER TABLE rec_repair_queue
  ADD CONSTRAINT rec_repair_queue_reason_check
  CHECK (reason IN ('merged', 'features_changed', 'merge_reverted', 'vocabulary_pruned'));

-- ---------------------------------------------------------------------------------------
-- The terms to prune
-- ---------------------------------------------------------------------------------------
-- A frozen snapshot of `tankovault_domain::DEFAULT_BLOCKED_TERMS`, deliberately not kept in sync
-- with it: the live guard is the Rust constant, and this statement runs exactly once against the
-- catalogue as it stands today. An operator's own additions are not pruned here — they were
-- never written by a build that had them configured.
--
-- Deleting is safe in the direction that matters, the same way it was for `series_titles` in
-- 0025: both writers are idempotent upserts, so a term a deployment's own guard *allows* is
-- re-inserted by the next scan of each series, whereas a template label never is.
CREATE TEMP TABLE pruned_terms (slug text PRIMARY KEY) ON COMMIT DROP;
INSERT INTO pruned_terms (slug) VALUES
  -- Placeholders.
  ('updating'), ('update'), ('updated'), ('unknown'), ('none'), ('null'), ('n-a'), ('na'),
  ('tbd'), ('coming-soon'), ('no-genre'), ('no-genres'),
  -- Template labels.
  ('status'), ('genre'), ('genres'), ('alternative'), ('alternative-name'),
  ('alternative-names'), ('author'), ('authors'), ('artist'), ('artists'), ('type'),
  ('release'), ('released'), ('rating'), ('view'), ('views'), ('summary'), ('description'),
  ('tags'), ('chapter'), ('chapters'),
  -- Medium, not genre.
  ('manga'), ('manhwa'), ('manhua'), ('webtoon'), ('webtoons'), ('webcomic'), ('comic'),
  ('comics'), ('raw'), ('scan'), ('scans'), ('scanlation');

-- ---------------------------------------------------------------------------------------
-- Drop the links, then the vocabulary rows they were the last reference to
-- ---------------------------------------------------------------------------------------
-- Which series lose a *tag* is recorded separately from which lose a credit, because only the
-- first needs a re-projection: `FeatureKind::is_dense_eligible` excludes authors from the dense
-- space, so a pruned credit changes a series' sparse vector — repaired in place below — and
-- nothing about its embedding.
CREATE TEMP TABLE retagged_series (series_id uuid PRIMARY KEY) ON COMMIT DROP;

WITH doomed AS (
  DELETE FROM series_tags st
   USING tags t, pruned_terms p
   WHERE t.id = st.tag_id AND t.slug = p.slug
   RETURNING st.series_id
)
INSERT INTO retagged_series (series_id)
SELECT DISTINCT series_id FROM doomed
ON CONFLICT (series_id) DO NOTHING;

DELETE FROM series_authors sa
 USING authors a, pruned_terms p
 WHERE a.id = sa.author_id AND a.slug = p.slug;

-- The vocabulary rows themselves, so neither term can be offered as a facet chip or a credit
-- before the next scan re-decides. The `NOT EXISTS` is redundant with the deletes above and kept
-- anyway: both link tables cascade, so a surviving link would be deleted silently rather than
-- keep its vocabulary row.
DELETE FROM tags t
 USING pruned_terms p
 WHERE t.slug = p.slug
   AND NOT EXISTS (SELECT 1 FROM series_tags st WHERE st.tag_id = t.id);

DELETE FROM authors a
 USING pruned_terms p
 WHERE a.slug = p.slug
   AND NOT EXISTS (SELECT 1 FROM series_authors sa WHERE sa.author_id = a.id);

-- ---------------------------------------------------------------------------------------
-- Repair the derived vectors in place
-- ---------------------------------------------------------------------------------------
-- The item model is derived and could simply be dropped, but "dropped" here means every
-- affected series contributes nothing to any profile and appears on no shelf until the
-- incremental build's per-run budget reaches it — potentially many runs, for a term this
-- widespread. Removing the element from the two parallel arrays and re-normalising is the same
-- answer the next extraction will produce: idf is a property of a feature's own `doc_count`,
-- which pruning a *different* feature does not change.
-- Restricted to the two axes intake interns from a template row. The restriction is load-bearing
-- rather than tidiness: `manga` and `original` are legitimate *values* on the `source` axis
-- (`AniList`'s "adapted from"), and `unknown` would be one on `status` if that axis emitted it.
CREATE TEMP TABLE pruned_features (id int PRIMARY KEY) ON COMMIT DROP;
INSERT INTO pruned_features (id)
SELECT f.id FROM rec_features f JOIN pruned_terms p ON p.slug = f.value
 WHERE f.kind IN ('tag', 'author');

UPDATE series_features sf
   SET feature_ids = k.ids, weights = k.ws
  FROM (
    SELECT e.series_id,
           array_agg(e.fid ORDER BY e.fid) AS ids,
           -- Cast back explicitly: the norm is computed in double precision, and `real[]` is not
           -- an assignment cast away from `double precision[]`.
           array_agg((CASE WHEN e.norm > 0 THEN e.w / e.norm ELSE e.w END)::real ORDER BY e.fid)
             AS ws
      FROM (
        SELECT s.series_id,
               u.fid,
               u.w,
               sqrt(sum(u.w::double precision * u.w) OVER (PARTITION BY s.series_id)) AS norm
          FROM series_features s
          CROSS JOIN LATERAL unnest(s.feature_ids, s.weights) AS u(fid, w)
         WHERE EXISTS (SELECT 1 FROM pruned_features q WHERE q.id = ANY(s.feature_ids))
           AND NOT EXISTS (SELECT 1 FROM pruned_features q WHERE q.id = u.fid)
      ) e
     GROUP BY e.series_id
  ) k
 WHERE sf.series_id = k.series_id;

-- A series whose *whole* vector was the pruned term produces no group above, so it is emptied
-- here rather than left holding the array the previous statement could not rewrite.
UPDATE series_features sf
   SET feature_ids = '{}', weights = '{}'
 WHERE EXISTS (SELECT 1 FROM pruned_features q WHERE q.id = ANY(sf.feature_ids))
   AND NOT EXISTS (
     SELECT 1 FROM unnest(sf.feature_ids) AS fid
      WHERE NOT EXISTS (SELECT 1 FROM pruned_features q WHERE q.id = fid)
   );

-- Every reader whose stored profile names a pruned feature is rebuilt on their next read. The
-- vectors it rebuilds from are correct as of the statements above, so this does not wait on the
-- build.
UPDATE user_taste_profile p
   SET stale = true
 WHERE EXISTS (
   SELECT 1 FROM pruned_features q
    WHERE q.id = ANY(p.feature_ids) OR q.id = ANY(p.neg_feature_ids)
 );

DELETE FROM rec_features f USING pruned_features q WHERE f.id = q.id;

-- Only the series that lost a tag: see `retagged_series` above.
INSERT INTO rec_repair_queue (series_id, reason)
SELECT series_id, 'vocabulary_pruned' FROM retagged_series
ON CONFLICT (series_id) DO NOTHING;
