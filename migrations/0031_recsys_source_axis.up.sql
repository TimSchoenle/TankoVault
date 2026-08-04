-- The adaptation-source axis (docs/RECOMMENDATIONS.md §6.1).
--
-- `AniList`'s `source` — ORIGINAL, LIGHT_NOVEL, WEB_NOVEL, VIDEO_GAME and so on — is the one
-- signal in the widened media selection with nowhere to live: the other four land on columns the
-- recsys-signals migration already added. Light-novel and web-novel adaptations cluster hard,
-- and nothing else in the vocabulary separates them from an original work.
ALTER TABLE series ADD COLUMN IF NOT EXISTS external_source text;

-- Deliberately unconstrained text: the value is upstream's enum, upstream extends it, and a
-- CHECK here would turn a new `AniList` variant into a failed enrichment sweep. The feature
-- extractor lower-cases it and treats an unknown value as just another term.

-- The feature vocabulary gains the matching kind. `rec_features.kind` is part of a feature's
-- identity, so this is an axis, not a value on an existing one.
ALTER TABLE rec_features DROP CONSTRAINT IF EXISTS rec_features_kind_check;
ALTER TABLE rec_features
  ADD CONSTRAINT rec_features_kind_check
  CHECK (kind IN ('tag', 'author', 'content_type', 'country', 'status', 'decade', 'length',
                  'source'));
