//! Build bookkeeping: what generation is live, what stage a run is in, and what needs redoing.

use crate::error::DbResult;
use sqlx::{FromRow, PgExecutor};
use tankovault_domain::SeriesId;
use time::OffsetDateTime;
use uuid::Uuid;

/// The single row of `rec_build_state`.
#[derive(Debug, Clone, FromRow)]
pub struct BuildState {
    pub generation: i32,
    pub stage: String,
    pub started_at: Option<OffsetDateTime>,
    pub finished_at: Option<OffsetDateTime>,
    pub series_built: i32,
    pub vocabulary: i32,
    pub dense_dims: i32,
    pub error: Option<String>,
}

/// Read the current build state.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only; the row is created by migration 0028 and cannot be absent.
pub async fn read_build_state<'e, E: PgExecutor<'e>>(exec: E) -> DbResult<BuildState> {
    let state = sqlx::query_as!(
        BuildState,
        "SELECT generation, stage, started_at, finished_at, series_built, vocabulary, \
                dense_dims, error \
         FROM rec_build_state WHERE id",
    )
    .fetch_one(exec)
    .await?;
    Ok(state)
}

/// How much of the catalogue the live model actually covers.
///
/// The three counts are read together because the *gaps* between them are the diagnosis, not the
/// absolute numbers: a large drop from extracted to embedded means a full build never finished,
/// and a large drop from embedded to recommendable means `build.min_features` is excluding more
/// than intended.
#[derive(Debug, Clone, Copy, FromRow)]
pub struct ModelCoverage {
    /// Series in the catalogue, the denominator for everything below.
    pub series_total: i64,
    /// Series with an extracted feature vector.
    pub with_features: i64,
    /// Series with a projected embedding, and therefore reachable by neighbour retrieval.
    pub with_embedding: i64,
    /// Series the model is willing to recommend at all.
    pub recommendable: i64,
}

/// Read the model's coverage of the catalogue.
///
/// One statement rather than four round trips, so the numbers the console compares against each
/// other are read at one point in time — mid-build they would otherwise disagree by however long
/// the calls took.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only.
pub async fn read_model_coverage<'e, E: PgExecutor<'e>>(exec: E) -> DbResult<ModelCoverage> {
    let coverage = sqlx::query_as!(
        ModelCoverage,
        "SELECT (SELECT count(*) FROM series)                                AS \"series_total!\", \
                (SELECT count(*) FROM series_features)                      AS \"with_features!\", \
                (SELECT count(*) FROM series_embedding)                     AS \"with_embedding!\", \
                (SELECT count(*) FROM series_prior WHERE recommendable)     AS \"recommendable!\"",
    )
    .fetch_one(exec)
    .await?;
    Ok(coverage)
}

/// Claim the build, advancing the generation, and return the generation claimed.
///
/// The `WHERE stage = 'idle'` is the mutual exclusion: a build is a singleton, and two workers
/// that both decided to start would interleave writes under two generations and leave a model
/// that is neither. A caller that gets `None` was beaten to it and must do nothing — not wait,
/// not retry, because the other build is already doing this one's work.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only.
pub async fn start_build<'e, E: PgExecutor<'e>>(exec: E, full: bool) -> DbResult<Option<i32>> {
    // A full build takes the next generation so its rows can be swapped in wholesale; an
    // incremental one writes under the live generation, because it is patching that model
    // rather than replacing it.
    let generation = sqlx::query_scalar!(
        "UPDATE rec_build_state \
            SET generation  = CASE WHEN $1 THEN generation + 1 ELSE generation END, \
                stage       = CASE WHEN $1 THEN 'full:features' ELSE 'incremental' END, \
                started_at  = now(), \
                finished_at = NULL, \
                error       = NULL \
          WHERE id AND stage = 'idle' \
      RETURNING generation",
        full,
    )
    .fetch_optional(exec)
    .await?;
    Ok(generation)
}

/// Record progress within a running build.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only.
pub async fn update_build_stage<'e, E: PgExecutor<'e>>(
    exec: E,
    stage: &str,
    series_built: i32,
) -> DbResult<()> {
    sqlx::query!(
        "UPDATE rec_build_state SET stage = $1, series_built = $2 WHERE id",
        stage,
        series_built,
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// Release the build, recording how it ended.
///
/// **Must run on the failure path too.** The claim in [`start_build`] is only released here, so
/// a build that dies without calling this leaves `stage` stuck and every subsequent run
/// declining to start — a recommender that silently stops updating and reports nothing wrong.
/// The operator-visible symptom is `recsys_model_age_seconds` climbing.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only.
pub async fn finish_build<'e, E: PgExecutor<'e>>(
    exec: E,
    series_built: i32,
    vocabulary: i32,
    dense_dims: i32,
    error: Option<&str>,
) -> DbResult<()> {
    sqlx::query!(
        "UPDATE rec_build_state \
            SET stage        = 'idle', \
                finished_at  = now(), \
                series_built = $1, \
                vocabulary   = $2, \
                dense_dims   = $3, \
                error        = $4 \
          WHERE id",
        series_built,
        vocabulary,
        dense_dims,
        error,
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// Persist the projection basis solved by a full build.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only.
pub async fn write_basis<'e, E: PgExecutor<'e>>(
    exec: E,
    coefficients: &[u8],
    input_dim: i32,
    dense_dims: i32,
) -> DbResult<()> {
    sqlx::query!(
        "UPDATE rec_build_state \
            SET basis = $1, basis_input_dim = $2, dense_dims = $3 WHERE id",
        coefficients,
        input_dim,
        dense_dims,
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// The stored projection basis, if a full build has ever solved one.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only; a deployment that has never run a full build gets `None`,
/// which is the signal an incremental build must not proceed — there is no space to project
/// into yet.
pub async fn read_basis<'e, E: PgExecutor<'e>>(exec: E) -> DbResult<Option<(Vec<u8>, i32, i32)>> {
    #[derive(FromRow)]
    struct Row {
        basis: Option<Vec<u8>>,
        basis_input_dim: i32,
        dense_dims: i32,
    }
    let row = sqlx::query_as!(
        Row,
        "SELECT basis, basis_input_dim, dense_dims FROM rec_build_state WHERE id",
    )
    .fetch_one(exec)
    .await?;
    Ok(row
        .basis
        .map(|basis| (basis, row.basis_input_dim, row.dense_dims)))
}

/// Drop model rows left behind by an earlier generation.
///
/// The generation counter is what makes a full rebuild atomic-ish without a table swap: rows are
/// upserted under the new one while readers keep querying the old, and this is the sweep that
/// removes whatever the new build did not touch — series deleted since, most often.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only.
pub async fn delete_stale_generations<'e, E: PgExecutor<'e> + Copy>(
    exec: E,
    generation: i32,
) -> DbResult<()> {
    sqlx::query!(
        "DELETE FROM series_embedding WHERE generation < $1",
        generation
    )
    .execute(exec)
    .await?;
    sqlx::query!(
        "DELETE FROM series_features WHERE generation < $1",
        generation
    )
    .execute(exec)
    .await?;
    sqlx::query!("DELETE FROM series_prior WHERE generation < $1", generation)
        .execute(exec)
        .await?;
    Ok(())
}

/// Mark a series' model as stale.
///
/// Idempotent by primary key, which is what makes it safe to call from the merge path: a popular
/// series absorbed forty times in an hour is one row, not forty.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only.
pub async fn enqueue_repair<'e, E: PgExecutor<'e>>(
    exec: E,
    series_id: SeriesId,
    reason: &str,
) -> DbResult<()> {
    sqlx::query!(
        "INSERT INTO rec_repair_queue (series_id, reason) VALUES ($1, $2) \
         ON CONFLICT (series_id) DO NOTHING",
        series_id.as_uuid(),
        reason,
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// How many series are waiting to be re-embedded.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only.
pub async fn repair_depth<'e, E: PgExecutor<'e>>(exec: E) -> DbResult<i64> {
    let depth = sqlx::query_scalar!("SELECT count(*) AS \"n!\" FROM rec_repair_queue")
        .fetch_one(exec)
        .await?;
    Ok(depth)
}

/// Take a batch off the repair queue, deleting the rows in the same statement.
///
/// Delete-on-claim, not claim-then-delete: the work is idempotent (re-extracting a series is a
/// pure function of its facts), so losing a row to a crash costs one stale embedding until the
/// next full build, while leaving rows behind would spin the incremental pass on them forever.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only.
pub async fn claim_repair_batch<'e, E: PgExecutor<'e>>(
    exec: E,
    limit: i64,
) -> DbResult<Vec<SeriesId>> {
    let rows = sqlx::query_scalar!(
        "DELETE FROM rec_repair_queue \
          WHERE series_id IN (SELECT series_id FROM rec_repair_queue \
                              ORDER BY enqueued_at LIMIT $1 FOR UPDATE SKIP LOCKED) \
      RETURNING series_id",
        limit,
    )
    .fetch_all(exec)
    .await?;
    Ok(rows.into_iter().map(SeriesId::from_uuid).collect())
}

/// Series whose stored model predates the live generation, or which have none at all.
///
/// The incremental build's work list after the repair queue is drained. `series_features` is
/// left-joined rather than filtered, so a series that has never been extracted is included —
/// that is the case a `WHERE generation < $1` alone would silently skip forever.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only.
pub async fn list_stale_series<'e, E: PgExecutor<'e>>(
    exec: E,
    generation: i32,
    limit: i64,
) -> DbResult<Vec<SeriesId>> {
    let rows: Vec<Uuid> = sqlx::query_scalar!(
        "SELECT s.id FROM series s \
         LEFT JOIN series_features f ON f.series_id = s.id \
         WHERE f.series_id IS NULL OR f.generation < $1 \
         ORDER BY s.id LIMIT $2",
        generation,
        limit,
    )
    .fetch_all(exec)
    .await?;
    Ok(rows.into_iter().map(SeriesId::from_uuid).collect())
}
