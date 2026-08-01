-- The duplicate-series merge queue: a usable queue, and the keys that stop filling it.
--
-- ---------------------------------------------------------------------------------------
-- What was wrong
-- ---------------------------------------------------------------------------------------
-- On a 26 418-series catalogue this queue held 2 676 open rows, every one of them carrying
-- the literal reason `ambiguous title match`, ordered by insertion time, with no way to tell a
-- certain duplicate from a coincidence of wording. Three separate defects produced that:
--
--   1. `normalize_title` treated an apostrophe as a word boundary, so `Sorry but I’m not Yuri`
--      and `Sorry But Im Not Yuri` — the same series on two providers — produced the keys
--      `sorry but i m not yuri` and `sorry but im not yuri`. Every possessive and contraction
--      in the catalogue split the same way. That rule has changed, which invalidates every
--      stored key; the backfill below is what re-derives them.
--   2. Nothing deduplicated the queue. `record_merge_candidate` was a bare `INSERT`, so the
--      same ambiguity could be recorded repeatedly, and `(A,B)` and `(B,A)` were two different
--      rows describing one pair. Worse, an operator's dismissal was not durable against a
--      re-scan: nothing stopped the pair being inserted again as a fresh open row.
--   3. There was no way to find a duplicate that the scan did not create. The queue was a
--      side effect of `resolve_canonical_series`, so two series that only became obviously
--      identical *later* — once enrichment gave them authors, a year, or an alternative title —
--      were never reconsidered. 59 pairs with byte-identical whitespace-stripped titles sat in
--      the catalogue without ever reaching the queue.
--
-- ---------------------------------------------------------------------------------------
-- `tv_normalize_title` is a bootstrap twin, not a second implementation
-- ---------------------------------------------------------------------------------------
-- `tankovault_domain::normalize_title` is the authority for this key and always will be; this
-- function exists because a migration cannot call it, and 26k rows cannot be re-derived from
-- application code that only runs on the next scan. It is pinned against the Rust version by
-- `crates/db/tests/repo_matching.rs::the_sql_normalizer_agrees_with_the_rust_one`, and
-- `POST /v1/admin/matching/rebuild-keys` re-derives every key through the Rust function, which
-- is the supported way to repair any divergence this one introduces.
CREATE OR REPLACE FUNCTION tv_normalize_title(t text)
RETURNS text
LANGUAGE sql
IMMUTABLE
STRICT
PARALLEL SAFE
AS $$
  WITH lowered AS (
    SELECT lower(t) AS s
  ), widened AS (
    -- Full-width forms (U+FF01..U+FF5E) are ASCII shifted by 0xFEE0, plus the ideographic
    -- space. Folded first so the apostrophe and ampersand rules below see `＇` and `＆`.
    -- The upper-case half is listed even though `lower()` has already run, so the fold does
    -- not silently depend on the database's case-mapping covering full-width Latin.
    SELECT translate(
             s,
             '！＂＃＄％＆＇（）＊＋，－．／０１２３４５６７８９：；＜＝＞？＠'
             || 'ＡＢＣＤＥＦＧＨＩＪＫＬＭＮＯＰＱＲＳＴＵＶＷＸＹＺ'
             || '［＼］＾＿｀'
             || 'ａｂｃｄｅｆｇｈｉｊｋｌｍｎｏｐｑｒｓｔｕｖｗｘｙｚ'
             || '｛｜｝～　',
             '!"#$%&''()*+,-./0123456789:;<=>?@'
             || 'abcdefghijklmnopqrstuvwxyz'
             || '[\]^_`'
             || 'abcdefghijklmnopqrstuvwxyz'
             || '{|}~ '
           ) AS s
    FROM lowered
  ), elided AS (
    -- Apostrophes in every spelling a provider emits, and combining marks (U+0300..U+036F,
    -- U+1AB0..U+1AFF, U+20D0..U+20F0, U+FE20..U+FE2F). Both sit *inside* a word: removing them
    -- is what makes `witch's` and `witchs` one key, and what keeps `İstanbul` — which
    -- lowercases to `i` + U+0307 — one word instead of two.
    SELECT regexp_replace(
             s,
             '[''‘’ʼʹ`´′'
             || chr(768)   || '-' || chr(879)
             || chr(6832)  || '-' || chr(6911)
             || chr(8400)  || '-' || chr(8432)
             || chr(65056) || '-' || chr(65071)
             || ']',
             '',
             'g'
           ) AS s
    FROM widened
  ), expanded AS (
    -- One character that is really several letters. `ß` is `ss`, not `s`, or `Straße` and
    -- `Strasse` land on different keys; `&` is the word "and", which providers spell both ways.
    SELECT replace(replace(replace(replace(replace(replace(replace(replace(replace(replace(replace(
             s, '&', ' and '),
             'ß', 'ss'), 'æ', 'ae'), 'œ', 'oe'), 'ĳ', 'ij'), 'þ', 'th'),
             'ﬀ', 'ff'), 'ﬁ', 'fi'), 'ﬂ', 'fl'), 'ﬃ', 'ffi'), 'ﬄ', 'ffl') AS s
    FROM elided
  ), folded AS (
    -- An accented letter is its base letter. Written as one concatenated pair per base letter
    -- rather than two long opaque strings, because `translate` silently *deletes* any `from`
    -- character without a `to` partner — a miscount here would not error, it would quietly
    -- drop letters out of every affected title.
    SELECT translate(
             s,
             'àáâãäåāăąǎȧ' || 'çćĉċč' || 'ďđð' || 'èéêëēĕėęě' || 'ĝğġģ' || 'ĥħ'
             || 'ìíîïĩīĭįı' || 'ĵ' || 'ķ' || 'ĺļľŀł' || 'ñńņňŋ' || 'òóôõöøōŏő'
             || 'ŕŗř' || 'śŝşš' || 'ţťŧ' || 'ùúûüũūŭůűų' || 'ŵ' || 'ýÿŷ' || 'źżž',
             'aaaaaaaaaaa' || 'ccccc' || 'ddd' || 'eeeeeeeee' || 'gggg' || 'hh'
             || 'iiiiiiiii' || 'j' || 'k' || 'lllll' || 'nnnnn' || 'ooooooooo'
             || 'rrr' || 'ssss' || 'ttt' || 'uuuuuuuuuu' || 'w' || 'yyy' || 'zzz'
           ) AS s
    FROM expanded
  ), cleaned AS (
    -- Everything that is not alphanumeric is a word boundary. `[:alnum:]` is `iswalnum()`
    -- under this database's UTF-8 ctype, so CJK and Hangul titles survive intact.
    SELECT btrim(regexp_replace(s, '[^[:alnum:]]+', ' ', 'g')) AS s
    FROM folded
  ), toks AS (
    SELECT tok, ord
    FROM cleaned, LATERAL unnest(string_to_array(s, ' ')) WITH ORDINALITY AS u(tok, ord)
    WHERE tok <> ''
  ), kept AS (
    SELECT tok, ord FROM toks
    WHERE tok <> ALL (ARRAY[
      'manga','mangas','manhwa','manhwas','manhua','manhuas','webtoon','webtoons',
      'webcomic','webcomics','comic','comics','official','raw','raws','scan','scans','scanlation'
    ])
  )
  -- A title made entirely of noise words keeps them, because the alternative is an empty key
  -- and every empty key collides with every other one.
  SELECT COALESCE(
    NULLIF((SELECT string_agg(tok, ' ' ORDER BY ord) FROM kept), ''),
    (SELECT string_agg(tok, ' ' ORDER BY ord) FROM toks),
    ''
  )
$$;

-- ---------------------------------------------------------------------------------------
-- Re-derive every stored key
-- ---------------------------------------------------------------------------------------
-- `series_titles` first, and destructively: its primary key is `(series_id, normalized)`, so
-- two alternative titles that normalized apart under the old rules and together under the new
-- ones would collide on `UPDATE`. One of each such pair is dropped — they are by definition the
-- same key, so nothing is lost but a duplicate row.
DELETE FROM series_titles a
 WHERE EXISTS (
   SELECT 1 FROM series_titles b
    WHERE b.series_id = a.series_id
      AND tv_normalize_title(b.title) = tv_normalize_title(a.title)
      AND (b.normalized, b.title) < (a.normalized, a.title)
 );

UPDATE series_titles
   SET normalized = tv_normalize_title(title)
 WHERE normalized IS DISTINCT FROM tv_normalize_title(title);

UPDATE series
   SET normalized_title = tv_normalize_title(canonical_title)
 WHERE normalized_title IS DISTINCT FROM tv_normalize_title(canonical_title);

-- ---------------------------------------------------------------------------------------
-- The compact key, indexed
-- ---------------------------------------------------------------------------------------
-- The normalized key with its spaces removed. Providers scrape titles out of HTML and a missing
-- space between two inline elements is the commonest way one listing differs from another for
-- the same work: `Spy X Family` against `Spyxfamily`, `Hana Kimi` against `Hanakimi`. Trigram
-- similarity scores those pairs 0.37–0.58, so they never reached the queue.
--
-- Indexed because the duplicate sweep's blocking step is `GROUP BY` this expression over the
-- whole `series` table; without the index that is a sequential scan plus a sort on every sweep.
CREATE INDEX series_compact_title_idx ON series (replace(normalized_title, ' ', ''));
CREATE INDEX series_titles_compact_idx ON series_titles (replace(normalized, ' ', ''));

-- ---------------------------------------------------------------------------------------
-- Make the queue a set of pairs rather than a log of observations
-- ---------------------------------------------------------------------------------------
-- Canonical ordering first. Which id landed in `series_id` and which in `candidate_id` recorded
-- nothing but the accident of which one the scan happened to create second, and it made `(A,B)`
-- and `(B,A)` two rows for one pair — so neither the unique index below nor a dismissal could
-- be relied on. Storage order and *merge direction* are now separate concerns: the survivor is
-- chosen when the merge happens, from which series actually carries more of the work.
UPDATE merge_candidates
   SET series_id = candidate_id, candidate_id = series_id
 WHERE series_id > candidate_id;

-- Collapse the duplicates that ordering has now made visible, keeping the most decided row:
-- resolved beats open (an operator's dismissal must survive), then the highest score, then the
-- newest. `false < true` in Postgres, so the tuple comparison keeps the resolved row.
DELETE FROM merge_candidates a
 USING merge_candidates b
 WHERE a.series_id = b.series_id
   AND a.candidate_id = b.candidate_id
   AND (a.resolved, a.score, a.created_at, a.id) < (b.resolved, b.score, b.created_at, b.id);

-- One row per pair, forever. This is what makes `record_merge_candidate` idempotent and, more
-- importantly, what makes a dismissal *durable*: the upsert's `WHERE NOT resolved` guard has
-- something to conflict with, so a re-scan can no longer resurrect a pair an operator has
-- already judged distinct.
CREATE UNIQUE INDEX merge_candidates_pair_key ON merge_candidates (series_id, candidate_id);

-- Which scoring rules fired, as the stable slugs `tankovault_domain::matching::MatchSignals`
-- emits. `reason` stays as the human sentence; this is the machine-readable half, and it is
-- what lets the console render badges and the sweep re-judge a row without re-scoring it.
ALTER TABLE merge_candidates ADD COLUMN signals text[] NOT NULL DEFAULT '{}';

-- When the pair was last re-scored. A candidate recorded at ingest is scored without the tags,
-- authors and alternative titles that enrichment adds afterwards, so its score is a floor, not
-- a verdict; the sweep revisits it and this is how it knows what it has already looked at.
ALTER TABLE merge_candidates ADD COLUMN updated_at timestamptz NOT NULL DEFAULT now();

-- How a resolved row was resolved. `resolved` alone could not distinguish "an operator said
-- these are different works" from "these were merged", which are opposite facts: the first must
-- suppress the pair forever, the second is already enforced by the row's series ceasing to
-- exist. NULL while the row is open.
ALTER TABLE merge_candidates ADD COLUMN outcome text;
ALTER TABLE merge_candidates
  ADD CONSTRAINT merge_candidates_outcome_check
  CHECK (outcome IS NULL OR outcome IN ('merged', 'auto_merged', 'dismissed'));

-- Existing resolved rows predate the column and cannot be attributed; `dismissed` is the safe
-- reading, because it is the one that suppresses rather than the one that acts.
UPDATE merge_candidates SET outcome = 'dismissed' WHERE resolved AND outcome IS NULL;

-- The review queue is read highest-confidence-first — an operator working a 2 600-row queue
-- needs the certain duplicates at the top, not the most recently observed ones.
CREATE INDEX merge_candidates_open_score ON merge_candidates (score DESC, created_at DESC)
  WHERE NOT resolved;
