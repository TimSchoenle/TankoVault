-- Reverses 0031. Any `rec_features` row on the retired axis has to go first, or the narrowed
-- CHECK cannot be validated.
DELETE FROM rec_features WHERE kind = 'source';

ALTER TABLE rec_features DROP CONSTRAINT IF EXISTS rec_features_kind_check;
ALTER TABLE rec_features
  ADD CONSTRAINT rec_features_kind_check
  CHECK (kind IN ('tag', 'author', 'content_type', 'country', 'status', 'decade', 'length'));

ALTER TABLE series DROP COLUMN IF EXISTS external_source;
