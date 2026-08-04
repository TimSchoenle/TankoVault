-- Reverse of `0029_recsys_user_model.up.sql`.
--
-- Affinity and the taste profile are derived and rebuild themselves from the watchlist and read
-- progress. `recommendation_feedback` does **not**: a reader's "never show me this" exists
-- nowhere else, and dropping this table discards those decisions permanently.

DROP TRIGGER IF EXISTS progress_marks_profile_stale ON read_progress;
DROP TRIGGER IF EXISTS watchlist_marks_profile_stale ON watchlist_entries;
DROP FUNCTION IF EXISTS recsys_mark_profile_stale();

DROP TABLE IF EXISTS user_recommendations;
DROP TABLE IF EXISTS recommendation_feedback;
DROP TABLE IF EXISTS user_taste_profile;
DROP TABLE IF EXISTS user_series_affinity;
