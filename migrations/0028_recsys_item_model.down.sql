-- Reverse of `0028_recsys_item_model.up.sql`.
--
-- Genuinely reversible, unlike 0026: every table here is derived from `series` and its link
-- tables, so re-applying and rebuilding restores the same model. The only loss is the time the
-- rebuild takes.

DROP INDEX IF EXISTS series_embedding_hnsw;

DROP TABLE IF EXISTS rec_repair_queue;
DROP TABLE IF EXISTS rec_build_state;
DROP TABLE IF EXISTS series_prior;
DROP TABLE IF EXISTS series_cooccurrence;
DROP TABLE IF EXISTS series_embedding;
DROP TABLE IF EXISTS series_features;
DROP TABLE IF EXISTS rec_features;
