//! The reader half: affinity, the taste profile, feedback, and the shelf cache.
//!
//! **Everything in this module is personal data.** It is derived from what someone reads and it
//! is a profile in the GDPR sense, so every table cascades from `users(id)` and every one of them
//! appears in `repo::privacy`'s export. A new table here that skips either is a subject access
//! request that lies.

use crate::error::DbResult;
use sqlx::{FromRow, PgExecutor};
use tankovault_domain::{SeriesId, UserId, WatchStatus};
use tankovault_recsys::Interaction;
use time::OffsetDateTime;
use uuid::Uuid;

/// What the reader has done with one series, as affinity needs it.
pub struct ReaderInteraction {
    /// The series the reader acted on.
    pub series_id: SeriesId,
    /// What they did, and how far through they got.
    pub interaction: Interaction,
    /// When, which is what the recency decay is measured from.
    pub observed_at: OffsetDateTime,
}

/// Every series the reader has any relationship with, with the depth and recency of it.
///
/// The whole list, not a page: affinity is recomputed wholesale because it is cheap (one indexed
/// read of the reader's own rows) and because a partial recompute would leave the profile built
/// from a mixture of two policies.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only; a reader tracking nothing gets an empty `Vec`.
pub async fn reader_interactions<'e, E: PgExecutor<'e>>(
    exec: E,
    user_id: UserId,
) -> DbResult<Vec<ReaderInteraction>> {
    #[derive(FromRow)]
    struct Row {
        series_id: Uuid,
        status: WatchStatus,
        chapters_read: i64,
        observed_at: OffsetDateTime,
        age_days: f64,
    }
    let rows = sqlx::query_as!(
        Row,
        "SELECT w.series_id, \
                w.status AS \"status: WatchStatus\", \
                COALESCE(floor(rp.last_read_whole_number), 0)::int8 AS \"chapters_read!\", \
                GREATEST(w.updated_at, COALESCE(rp.updated_at, w.updated_at)) AS \"observed_at!\", \
                (EXTRACT(EPOCH FROM (now() - GREATEST(w.updated_at, \
                        COALESCE(rp.updated_at, w.updated_at)))) / 86400.0)::float8 \
                  AS \"age_days!\" \
         FROM watchlist_entries w \
         LEFT JOIN read_progress rp ON rp.user_id = w.user_id AND rp.series_id = w.series_id \
         WHERE w.user_id = $1",
        user_id.as_uuid(),
    )
    .fetch_all(exec)
    .await?;

    #[expect(
        clippy::cast_possible_truncation,
        reason = "an age in days is a decay input, not a measurement; f32 is ample"
    )]
    Ok(rows
        .into_iter()
        .map(|r| ReaderInteraction {
            series_id: SeriesId::from_uuid(r.series_id),
            interaction: Interaction {
                status: r.status,
                chapters_read: r.chapters_read,
                age_days: r.age_days as f32,
            },
            observed_at: r.observed_at,
        })
        .collect())
}

/// Replace the reader's affinity rows wholesale.
///
/// Upsert-then-prune, in one transaction, and both halves of that are load-bearing.
///
/// *Prune*, because a series the reader removed from their watchlist has no row to update, and
/// leaving the stale one behind would keep it seeding recommendations after they said they were
/// done with it.
///
/// *Upsert first, and never delete-then-insert*, because the SPA opens several recommendation
/// surfaces at once and each one rebuilds a stale profile: two rebuilds for the same reader run
/// concurrently as a matter of course. Deleting first would give the second one a primary-key
/// collision on every row it re-inserted — and, in the window before it committed, would show a
/// concurrent reader an empty affinity table, which is indistinguishable from a reader who tracks
/// nothing and caches as an empty shelf.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only; a failure rolls back, so the reader keeps the affinity they had
/// rather than losing it to a half-applied rebuild.
pub async fn replace_affinity(
    pool: &sqlx::PgPool,
    user_id: UserId,
    series_ids: &[SeriesId],
    affinities: &[f32],
    engagements: &[f32],
    observed: &[OffsetDateTime],
) -> DbResult<()> {
    let ids: Vec<Uuid> = series_ids.iter().copied().map(SeriesId::as_uuid).collect();
    let mut tx = pool.begin().await?;

    // `ORDER BY t.series_id` is the deadlock guard, not cosmetics: `reader_interactions` has no
    // ordering of its own, so two concurrent rebuilds would otherwise take the same row locks in
    // whatever order the planner handed them back, and each could hold what the other wants.
    sqlx::query!(
        "INSERT INTO user_series_affinity \
            (user_id, series_id, affinity, engagement, observed_at) \
         SELECT $1, * FROM unnest($2::uuid[], $3::real[], $4::real[], $5::timestamptz[]) \
              AS t(series_id, affinity, engagement, observed_at) \
         ORDER BY t.series_id \
         ON CONFLICT (user_id, series_id) DO UPDATE \
            SET affinity    = EXCLUDED.affinity, \
                engagement  = EXCLUDED.engagement, \
                observed_at = EXCLUDED.observed_at",
        user_id.as_uuid(),
        &ids,
        affinities,
        engagements,
        observed,
    )
    .execute(&mut *tx)
    .await?;

    sqlx::query!(
        "DELETE FROM user_series_affinity WHERE user_id = $1 AND series_id <> ALL($2)",
        user_id.as_uuid(),
        &ids,
    )
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;
    Ok(())
}

/// The reader's strongest relationships, by absolute affinity.
///
/// Absolute, not signed: the series someone rejected shape the profile as much as the ones they
/// finished, and taking only the positives would discard the whole negative vector.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only.
pub async fn top_affinity<'e, E: PgExecutor<'e>>(
    exec: E,
    user_id: UserId,
    limit: i64,
) -> DbResult<Vec<(SeriesId, f32)>> {
    #[derive(FromRow)]
    struct Row {
        series_id: Uuid,
        affinity: f32,
    }
    let rows = sqlx::query_as!(
        Row,
        "SELECT series_id, affinity FROM user_series_affinity \
         WHERE user_id = $1 ORDER BY abs(affinity) DESC, series_id LIMIT $2",
        user_id.as_uuid(),
        limit,
    )
    .fetch_all(exec)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| (SeriesId::from_uuid(r.series_id), r.affinity))
        .collect())
}

/// The reader's taste profile, as stored.
pub struct TasteProfile {
    /// Interned feature ids the reader is drawn to, parallel to `weights`.
    pub feature_ids: Vec<i32>,
    /// How strongly, one per id above.
    pub weights: Vec<f32>,
    /// Interned feature ids the reader avoids, parallel to `neg_weights`.
    pub neg_feature_ids: Vec<i32>,
    /// How strongly, one per id above.
    pub neg_weights: Vec<f32>,
    /// The series this profile was built from.
    pub seeds: Vec<SeriesId>,
    /// The reader's centre of gravity, as a pgvector literal. `None` until a seed is embedded.
    pub embedding: Option<String>,
    /// Whether an interaction has landed since `built_at`, so the next build has work.
    pub stale: bool,
    /// When the profile was last rebuilt.
    pub built_at: OffsetDateTime,
}

/// Read the reader's profile.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only; a reader who has never had one gets `None`.
pub async fn read_profile<'e, E: PgExecutor<'e>>(
    exec: E,
    user_id: UserId,
) -> DbResult<Option<TasteProfile>> {
    #[derive(FromRow)]
    struct Row {
        feature_ids: Vec<i32>,
        weights: Vec<f32>,
        neg_feature_ids: Vec<i32>,
        neg_weights: Vec<f32>,
        seeds: Vec<Uuid>,
        embedding: Option<String>,
        stale: bool,
        built_at: OffsetDateTime,
    }
    let row = sqlx::query_as!(
        Row,
        "SELECT feature_ids, weights, neg_feature_ids, neg_weights, seeds, \
                embedding::text AS embedding, stale, built_at \
         FROM user_taste_profile WHERE user_id = $1",
        user_id.as_uuid(),
    )
    .fetch_optional(exec)
    .await?;

    Ok(row.map(|r| TasteProfile {
        feature_ids: r.feature_ids,
        weights: r.weights,
        neg_feature_ids: r.neg_feature_ids,
        neg_weights: r.neg_weights,
        seeds: r.seeds.into_iter().map(SeriesId::from_uuid).collect(),
        embedding: r.embedding,
        stale: r.stale,
        built_at: r.built_at,
    }))
}

/// Store a freshly built profile, clearing the stale flag.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only.
#[expect(
    clippy::too_many_arguments,
    reason = "the profile's five parallel vectors are one value; grouping them into a struct here \
              would only move the flattening to the call site"
)]
pub async fn write_profile<'e, E: PgExecutor<'e>>(
    exec: E,
    user_id: UserId,
    feature_ids: &[i32],
    weights: &[f32],
    neg_feature_ids: &[i32],
    neg_weights: &[f32],
    seeds: &[SeriesId],
    embedding: Option<&str>,
) -> DbResult<OffsetDateTime> {
    let seed_ids: Vec<Uuid> = seeds.iter().copied().map(SeriesId::as_uuid).collect();
    let built_at = sqlx::query_scalar!(
        "INSERT INTO user_taste_profile \
            (user_id, feature_ids, weights, neg_feature_ids, neg_weights, seeds, embedding, \
             stale, built_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7::text::halfvec(128), false, now()) \
         ON CONFLICT (user_id) DO UPDATE \
            SET feature_ids     = EXCLUDED.feature_ids, \
                weights         = EXCLUDED.weights, \
                neg_feature_ids = EXCLUDED.neg_feature_ids, \
                neg_weights     = EXCLUDED.neg_weights, \
                seeds           = EXCLUDED.seeds, \
                embedding       = EXCLUDED.embedding, \
                stale           = false, \
                built_at        = now() \
      RETURNING built_at",
        user_id.as_uuid(),
        feature_ids,
        weights,
        neg_feature_ids,
        neg_weights,
        &seed_ids,
        embedding,
    )
    .fetch_one(exec)
    .await?;
    Ok(built_at)
}

/// Mark a reader's profile as needing a rebuild.
///
/// Called from the watchlist and progress write paths. Deliberately a flag rather than a
/// recompute: the write path must stay fast, and the profile is only needed when a shelf is
/// actually asked for.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only. A reader with no profile row yet needs no flag — the absence
/// already means "build one".
pub async fn mark_profile_stale<'e, E: PgExecutor<'e>>(exec: E, user_id: UserId) -> DbResult<()> {
    sqlx::query!(
        "UPDATE user_taste_profile SET stale = true WHERE user_id = $1",
        user_id.as_uuid()
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// Mark every profile that tracks any of these series as stale.
///
/// Called by the incremental build once it has re-extracted a set: those readers' profiles are
/// weighted against feature ids the series may no longer carry. The merge and undo paths do the
/// same thing inline instead, because they must do it inside their own transaction and key it on
/// `user_series_affinity` rather than the watchlist — the loser's watchlist rows have already
/// been folded into the survivor's by the time they get there, while the affinity rows naming it
/// have not.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only.
pub async fn mark_profiles_stale_for_series<'e, E: PgExecutor<'e>>(
    exec: E,
    series: &[SeriesId],
) -> DbResult<()> {
    let ids: Vec<Uuid> = series.iter().copied().map(SeriesId::as_uuid).collect();
    sqlx::query!(
        "UPDATE user_taste_profile p SET stale = true \
          WHERE EXISTS (SELECT 1 FROM watchlist_entries w \
                        WHERE w.user_id = p.user_id AND w.series_id = ANY($1))",
        &ids,
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// Record a reader's verdict on a recommendation.
///
/// `hide_forever` outranks `not_interested`: a stronger refusal must not be softened by a later
/// weaker one, which is what a plain overwrite would allow.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only.
pub async fn record_feedback<'e, E: PgExecutor<'e>>(
    exec: E,
    user_id: UserId,
    series_id: SeriesId,
    verdict: &str,
) -> DbResult<()> {
    sqlx::query!(
        "INSERT INTO recommendation_feedback (user_id, series_id, verdict) VALUES ($1, $2, $3) \
         ON CONFLICT (user_id, series_id) DO UPDATE \
            SET verdict = CASE \
                  WHEN recommendation_feedback.verdict = 'hide_forever' \
                    OR EXCLUDED.verdict = 'hide_forever' THEN 'hide_forever' \
                  ELSE EXCLUDED.verdict END, \
                created_at = now()",
        user_id.as_uuid(),
        series_id.as_uuid(),
        verdict,
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// Series the reader must not be shown: already tracked, or refused.
///
/// `not_interested` expires after `decay_days` (whole days — `make_interval` takes an integer,
/// and a fractional suppression window is not a policy anyone needs); `hide_forever` does not. One statement rather
/// than two so the request path has a single set to subtract.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only.
pub async fn suppressed_series<'e, E: PgExecutor<'e>>(
    exec: E,
    user_id: UserId,
    decay_days: i32,
) -> DbResult<Vec<SeriesId>> {
    let rows: Vec<Uuid> = sqlx::query_scalar!(
        // `series_id!`: both branches select a `NOT NULL` column, but sqlx cannot see through a
        // `UNION` and infers the result as nullable.
        "SELECT series_id AS \"series_id!\" FROM watchlist_entries WHERE user_id = $1 \
         UNION \
         SELECT series_id FROM recommendation_feedback \
          WHERE user_id = $1 \
            AND (verdict = 'hide_forever' \
                 OR created_at > now() - make_interval(days => $2::int))",
        user_id.as_uuid(),
        decay_days,
    )
    .fetch_all(exec)
    .await?;
    Ok(rows.into_iter().map(SeriesId::from_uuid).collect())
}

/// The cached shelf, if it was built from the profile the caller holds.
///
/// The freshness key is `profile_at`, not age: a shelf is only valid for the profile it was
/// computed from, and a rebuild must invalidate it even if the clock has barely moved.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only.
pub async fn read_shelf<'e, E: PgExecutor<'e>>(
    exec: E,
    user_id: UserId,
    profile_at: OffsetDateTime,
    max_age_secs: f64,
) -> DbResult<Option<serde_json::Value>> {
    let items = sqlx::query_scalar!(
        "SELECT items FROM user_recommendations \
          WHERE user_id = $1 AND profile_at = $2 \
            AND built_at > now() - make_interval(secs => $3)",
        user_id.as_uuid(),
        profile_at,
        max_age_secs,
    )
    .fetch_optional(exec)
    .await?;
    Ok(items)
}

/// Drop a reader's cached shelf, forcing the next request to recompute it.
///
/// The cache is keyed on `(user_id, profile_at)` and expires on age, so nothing about it
/// notices a change to *who the reader is allowed to see*. Their taste profile has not moved,
/// the entry has not aged out, and the shelf they get back is the one built under the old
/// answer. That is how an opt-out keeps serving adult recommendations until the TTL runs down.
///
/// Called by whatever changes the adult gate for this reader. Idempotent; a reader with no
/// cached shelf is `Ok(())`.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only.
pub async fn clear_shelf<'e, E: PgExecutor<'e>>(exec: E, user_id: UserId) -> DbResult<()> {
    sqlx::query!(
        "DELETE FROM user_recommendations WHERE user_id = $1",
        user_id.as_uuid(),
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// Cache a freshly computed shelf.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only.
pub async fn write_shelf<'e, E: PgExecutor<'e>>(
    exec: E,
    user_id: UserId,
    items: &serde_json::Value,
    profile_at: OffsetDateTime,
) -> DbResult<()> {
    sqlx::query!(
        "INSERT INTO user_recommendations (user_id, items, profile_at) VALUES ($1, $2, $3) \
         ON CONFLICT (user_id) DO UPDATE \
            SET items = EXCLUDED.items, profile_at = EXCLUDED.profile_at, built_at = now()",
        user_id.as_uuid(),
        items,
        profile_at,
    )
    .execute(exec)
    .await?;
    Ok(())
}
