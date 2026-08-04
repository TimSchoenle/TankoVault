-- The reader half of the recommendation model (docs/RECOMMENDATIONS.md §5.3).
--
-- Unlike the item model, **this is personal data**: it is derived from what someone reads, and
-- it is a profile in the GDPR sense. Three consequences the item model does not have:
--
--   * every table cascades from `users(id)`, so erasure reaches it;
--   * `crates/db/src/repo/privacy.rs` exports all four, or the subject access request is a lie;
--   * `deploy/legal/privacy.en.md` has to disclose the profiling (Art. 13(2)(f)).

-- ---------------------------------------------------------------------------------------
-- Affinity
-- ---------------------------------------------------------------------------------------
-- One number per (reader, series) in [-1, 1], derived from watchlist status, reading depth and
-- recency. Materialised rather than computed per request because the taste profile is built from
-- the whole list and a reader can have thousands of entries.
CREATE TABLE user_series_affinity (
  user_id     uuid NOT NULL REFERENCES users(id)  ON DELETE CASCADE,
  series_id   uuid NOT NULL REFERENCES series(id) ON DELETE CASCADE,
  affinity    real NOT NULL,
  engagement  real NOT NULL,
  observed_at timestamptz NOT NULL,
  PRIMARY KEY (user_id, series_id),
  CHECK (affinity >= -1 AND affinity <= 1),
  CHECK (engagement >= 0 AND engagement <= 1)
);

-- Seeds are the top of this ordering, so it is the one index the profile build needs.
CREATE INDEX user_affinity_top_idx ON user_series_affinity (user_id, affinity DESC);

-- ---------------------------------------------------------------------------------------
-- Taste profile
-- ---------------------------------------------------------------------------------------
-- What a reader likes, as a sparse vector — and, separately, what they have rejected.
--
-- The negative vector is what makes "I dropped every isekai I ever opened" mean something.
-- Without it a dropped series contributes nothing but a filter on itself, and the system keeps
-- recommending the genre the reader has already turned down four times.
CREATE TABLE user_taste_profile (
  user_id         uuid PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
  feature_ids     int[]  NOT NULL DEFAULT '{}',
  weights         real[] NOT NULL DEFAULT '{}',
  neg_feature_ids int[]  NOT NULL DEFAULT '{}',
  neg_weights     real[] NOT NULL DEFAULT '{}',
  -- The series the profile was built from, in affinity order. Cached so the request path does
  -- not re-rank a five-thousand-entry watchlist to find twenty-five seeds.
  seeds           uuid[] NOT NULL DEFAULT '{}',
  -- The reader's centre of gravity in the dense space: the affinity-weighted mean of the seeds'
  -- embeddings. NULL until at least one seed has been embedded, which is also the signal that
  -- profile-vector retrieval has nothing to search with yet.
  embedding       halfvec(128),
  stale           boolean NOT NULL DEFAULT true,
  built_at        timestamptz NOT NULL DEFAULT now(),
  CHECK (cardinality(feature_ids) = cardinality(weights)),
  CHECK (cardinality(neg_feature_ids) = cardinality(neg_weights))
);

-- ---------------------------------------------------------------------------------------
-- Feedback
-- ---------------------------------------------------------------------------------------
-- A reader's decision about a recommendation, and the only explicit signal the product has.
--
-- **Folded on merge, not cascaded.** This is the one recommendation table that holds a decision
-- rather than a derivation: if the catalogue merges two series, a reader's "never show me this"
-- must survive it. Same rule, and the same reasoning, as `series_sync_overrides`.
CREATE TABLE recommendation_feedback (
  user_id    uuid NOT NULL REFERENCES users(id)  ON DELETE CASCADE,
  series_id  uuid NOT NULL REFERENCES series(id) ON DELETE CASCADE,
  verdict    text NOT NULL,
  created_at timestamptz NOT NULL DEFAULT now(),
  PRIMARY KEY (user_id, series_id),
  CONSTRAINT recommendation_feedback_verdict_check
    CHECK (verdict IN ('not_interested', 'hide_forever'))
);

-- ---------------------------------------------------------------------------------------
-- The shelf cache
-- ---------------------------------------------------------------------------------------
-- The last computed shelf, kept so a repeat visit is one primary-key read.
--
-- `profile_at` is the freshness key, not `built_at`: the shelf is only valid for the profile it
-- was computed from, and a profile rebuild must invalidate it even if the clock has barely moved.
CREATE TABLE user_recommendations (
  user_id    uuid PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
  items      jsonb NOT NULL,
  profile_at timestamptz NOT NULL,
  built_at   timestamptz NOT NULL DEFAULT now()
);

-- ---------------------------------------------------------------------------------------
-- Exact retrieval over rare features
-- ---------------------------------------------------------------------------------------
-- GIN over the feature array, so "which series carry any of these features" is an index lookup.
--
-- This is what makes the exact retrieval path affordable, and that path is what recovers the
-- precision the dense projection destroys: authors are excluded from the embedding entirely
-- (a rank-128 approximation cannot represent a feature with a document frequency of three), and
-- sharing an author is close to a certain recommendation. Without this index the only way to use
-- that signal is a sequential scan of every feature vector in the catalogue.
CREATE INDEX series_features_gin ON series_features USING gin (feature_ids);

-- ---------------------------------------------------------------------------------------
-- Keeping the profile honest
-- ---------------------------------------------------------------------------------------
-- A trigger, not a call at each write site.
--
-- The taste profile is derived from `watchlist_entries` and `read_progress`, and those are
-- written from a dozen places: the watchlist upsert, remove, bulk update, bulk remove and status
-- setter; progress set, mark-read-to, bulk mark-read; the external-sync reconciler; and
-- `merge_series`. A convention that every one of them must remember to invalidate the profile is
-- a convention that will be broken by the thirteenth — and the symptom is not an error, it is a
-- reader whose recommendations silently stop reflecting what they read.
--
-- Marking stale is idempotent and touches at most one row, so the per-row firing costs nothing
-- worth measuring even on a bulk update.
CREATE FUNCTION recsys_mark_profile_stale() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
  UPDATE user_taste_profile
     SET stale = true
   WHERE user_id = COALESCE(NEW.user_id, OLD.user_id);
  RETURN NULL;
END;
$$;

CREATE TRIGGER watchlist_marks_profile_stale
  AFTER INSERT OR UPDATE OR DELETE ON watchlist_entries
  FOR EACH ROW EXECUTE FUNCTION recsys_mark_profile_stale();

CREATE TRIGGER progress_marks_profile_stale
  AFTER INSERT OR UPDATE OR DELETE ON read_progress
  FOR EACH ROW EXECUTE FUNCTION recsys_mark_profile_stale();
