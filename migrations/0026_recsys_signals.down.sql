-- Reverse of `0026_recsys_signals.up.sql`, as far as reversing it means anything.
--
-- Dropping `series_merges` discards where every merged series went. That history cannot be
-- recomputed — the absorbed rows are gone — so a re-application starts from an empty map and
-- every id merged before the rollback stays a 404. Reversible in schema, not in fact.

DROP INDEX IF EXISTS series_merges_survivor_idx;
DROP TABLE IF EXISTS series_merges;

ALTER TABLE series DROP CONSTRAINT IF EXISTS series_external_score_check;
ALTER TABLE series
  DROP COLUMN IF EXISTS external_popularity,
  DROP COLUMN IF EXISTS external_score,
  DROP COLUMN IF EXISTS is_adult;

ALTER TABLE series_tags DROP CONSTRAINT IF EXISTS series_tags_weight_check;
ALTER TABLE series_tags
  DROP COLUMN IF EXISTS source,
  DROP COLUMN IF EXISTS weight;

ALTER TABLE tags DROP CONSTRAINT IF EXISTS tags_kind_check;
ALTER TABLE tags
  DROP COLUMN IF EXISTS series_count,
  DROP COLUMN IF EXISTS kind;

-- `CREATE EXTENSION vector` is deliberately **not** dropped. Another schema in the same database
-- may depend on it, and dropping an extension cascades into every column typed by it — which,
-- after a partial rollback, is a far worse outcome than an unused extension.
