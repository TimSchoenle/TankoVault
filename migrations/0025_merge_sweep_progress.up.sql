-- The duplicate sweep could not reach a duplicate, and the shortlist it worked from was noise.
--
-- ---------------------------------------------------------------------------------------
-- What was wrong
-- ---------------------------------------------------------------------------------------
-- Two KunManga listings of one work — identical japanese, romaji and english alternative
-- titles, 18 chapters each — sat in the catalogue as separate series with no queue row at all.
-- `find_duplicate_pairs` did shortlist them, on its `by_shared_alias` branch. It shortlisted
-- them at position 8 914 953 of 15 176 110, and the sweep takes the first 500. Two defects
-- compound to produce that number:
--
--   1. Nothing bounded a blocking key's fan-out. A provider adapter briefly scraped a Madara
--      summary block's *labels* into `series_titles`, so 5 509 series answered to the title
--      `Status`, 5 508 to `Alternative`, and so on for `Genres`, `View`, `Rating`, `Release`.
--      Each such key is an all-pairs clique: six of them are 15.2 million of the 15.2 million
--      shortlisted pairs. With them excluded the whole catalogue blocks to 4 352.
--   2. The shortlist had no progress guarantee. It is ordered by `(lo, hi)` with a `LIMIT`, and
--      excluded only pairs an operator had *resolved* — so a pair queued for review came back
--      next run, and a pair the scorer judged distinct had its row deleted and also came back.
--      The prefix was therefore static, and the only pairs ever leaving it were the ~31 per run
--      that got auto-merged. Reaching position 8.9M that way takes decades.
--
-- The fan-out cap and the "already recorded" exclusion live in `find_duplicate_pairs`. This
-- migration supplies the two things they need from the schema, plus the data repair.

-- ---------------------------------------------------------------------------------------
-- `distinct` is an outcome
-- ---------------------------------------------------------------------------------------
-- Deleting the row was the reason a distinct verdict could not stick. It was deliberate — a
-- scorer's "these are different works" is weaker evidence than an operator's, and must not
-- suppress the pair forever on evidence that may change — but the two requirements are not in
-- conflict. Recorded as its own outcome, the verdict is durable enough to keep the pair out of
-- the new-pair shortlist and revisitable on its own budget (`merge_sweep_recheck`,
-- least-recently-scored first), which is what `dismissed` deliberately is not.
ALTER TABLE merge_candidates DROP CONSTRAINT merge_candidates_outcome_check;
ALTER TABLE merge_candidates
  ADD CONSTRAINT merge_candidates_outcome_check
  CHECK (outcome IS NULL OR outcome IN ('merged', 'auto_merged', 'dismissed', 'distinct'));

-- The recheck queue is drained least-recently-scored first, and re-scoring bumps `updated_at`,
-- so the scan is a round-robin over a table that is mostly *not* this outcome. Partial, on the
-- ordering column, so it is an index scan of exactly the rows the budget will consume.
CREATE INDEX merge_candidates_distinct_recheck ON merge_candidates (updated_at ASC)
  WHERE outcome = 'distinct';

-- ---------------------------------------------------------------------------------------
-- Drop the alternative titles that are not titles
-- ---------------------------------------------------------------------------------------
-- The adapter no longer produces these, but nothing removes what it already wrote: alternative
-- titles are only ever inserted (`add_series_titles` is an upsert), so a rescan cannot retract
-- one. They are not merely inert — they are searched by `find_candidates`' trigram lookup and
-- scored by `best_title_match` as names the series answers to, so an incoming source titled
-- `Status` alias-matches five thousand series.
--
-- The threshold is the one `find_duplicate_pairs` blocks with (`MAX_KEY_FANOUT`), and the
-- argument is the same in both places: a title key held by more than a handful of distinct
-- series is not identifying, whatever produced it. Deleting is safe in the direction that
-- matters — a legitimately shared title is re-inserted by the next scan of each series, whereas
-- a label never is.
--
-- Only `series_titles`. A series whose *canonical* title lands on such a key keeps it; the cap
-- in `find_duplicate_pairs` stops that key blocking, which is the part that mattered.
DELETE FROM series_titles st
 WHERE replace(st.normalized, ' ', '') IN (
   SELECT key FROM (
     SELECT replace(normalized_title, ' ', '') AS key, id AS series_id
       FROM series WHERE normalized_title <> ''
     UNION
     SELECT replace(normalized, ' ', '') AS key, series_id
       FROM series_titles WHERE normalized <> ''
   ) k
   GROUP BY key
   HAVING count(*) > 16
 );
