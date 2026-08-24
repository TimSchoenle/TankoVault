//! Build bookkeeping: what generation is live, what stage a run is in, and what needs redoing.

use crate::error::DbResult;
use sqlx::{FromRow, PgExecutor};
use tankovault_domain::SeriesId;
use time::OffsetDateTime;
use uuid::Uuid;

/// The single row of `rec_build_state`.
#[derive(Debug, Clone, FromRow)]
pub struct BuildState {
    /// The generation currently being written, or the last one completed.
    pub generation: i32,
    /// What the build is doing, or the stage it stopped in.
    pub stage: String,
    /// When the current or last build claimed the row.
    pub started_at: Option<OffsetDateTime>,
    /// When the last build released it, `None` while one is running.
    pub finished_at: Option<OffsetDateTime>,
    /// Series the running stage has finished.
    pub series_built: i32,
    /// What the running stage is counting towards. Display only — nothing branches on it, and a
    /// stage that cannot cheaply know its own size leaves it at zero.
    pub stage_total: i32,
    /// Distinct features interned in this generation.
    pub vocabulary: i32,
    /// Width of the embedding this generation was projected into.
    pub dense_dims: i32,
    /// Why the last build failed, `None` when it did not.
    pub error: Option<String>,
}

/// A granted claim on the build: what generation to write under, and the token that proves the
/// claim is still this run's.
///
/// Every write that advances or releases the claim carries it, so a build whose lease expired
/// while it was still running cannot touch the state of the run that replaced it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BuildClaim {
    /// The generation this run writes under.
    pub generation: i32,
    /// The token. Every advance and release is conditioned on it.
    pub claim_id: Uuid,
}

/// Read the current build state.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only; the row is created by migration 0028 and cannot be absent.
pub async fn read_build_state<'e, E: PgExecutor<'e>>(exec: E) -> DbResult<BuildState> {
    let state = sqlx::query_as!(
        BuildState,
        "SELECT generation, stage, started_at, finished_at, series_built, stage_total, \
                vocabulary, dense_dims, error \
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

/// Claim the build, advancing the generation, and return the claim granted.
///
/// The claim is a **lease**, not a flag. `WHERE stage = 'idle'` alone was the mutual exclusion
/// and it was released in exactly one place — the `finish_build` at the end of a run — so a
/// build that died without reaching that line held the claim forever and every later run
/// declined to start, silently. A claim whose `heartbeat_at` is older than `lease_secs` (or
/// absent, which is a claim taken before the column existed) is therefore breakable.
///
/// `lease_secs` must be comfortably larger than the caller's heartbeat interval; the two are
/// defined together in `services/control-plane/src/recsys.rs` for that reason. Too short and a
/// live build gets its claim stolen mid-run; too long and a dead one blocks that much of the
/// schedule.
///
/// A caller that gets `None` was beaten to it by a *live* build and must do nothing — not wait,
/// not retry, because the other build is already doing this one's work.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only.
pub async fn start_build<'e, E: PgExecutor<'e>>(
    exec: E,
    full: bool,
    lease_secs: f64,
) -> DbResult<Option<BuildClaim>> {
    let claim_id = Uuid::new_v4();
    // A full build takes the next generation so its rows can be swapped in wholesale; an
    // incremental one writes under the live generation, because it is patching that model
    // rather than replacing it.
    let generation = sqlx::query_scalar!(
        "UPDATE rec_build_state \
            SET generation  = CASE WHEN $1 THEN generation + 1 ELSE generation END, \
                stage       = CASE WHEN $1 THEN 'full:features' ELSE 'incremental' END, \
                claim_id    = $2, \
                started_at  = now(), \
                heartbeat_at = now(), \
                finished_at = NULL, \
                series_built = 0, \
                stage_total = 0, \
                error       = NULL \
          WHERE id \
            AND (stage = 'idle' \
                 OR heartbeat_at IS NULL \
                 OR heartbeat_at < now() - make_interval(secs => $3::double precision)) \
      RETURNING generation",
        full,
        claim_id,
        lease_secs,
    )
    .fetch_optional(exec)
    .await?;
    Ok(generation.map(|generation| BuildClaim {
        generation,
        claim_id,
    }))
}

/// Record progress within a running build.
///
/// `stage_total` is what `series_built` is counting towards; pass `0` from a stage whose size is
/// not known without doing the work twice. The console shows a bare count in that case rather
/// than a bar it would have to invent a denominator for.
///
/// Fenced by `claim_id`: a build whose lease expired while it was still running would otherwise
/// keep writing its own progress over the run that replaced it, and the console would show two
/// runs interleaved as one going backwards.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only.
pub async fn update_build_stage<'e, E: PgExecutor<'e>>(
    exec: E,
    claim: BuildClaim,
    stage: &str,
    series_built: i32,
    stage_total: i32,
) -> DbResult<()> {
    sqlx::query!(
        "UPDATE rec_build_state SET stage = $1, series_built = $2, stage_total = $3 \
          WHERE id AND claim_id = $4",
        stage,
        series_built,
        stage_total,
        claim.claim_id,
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// Stamp the running build's lease so it is not reclaimed under it.
///
/// Driven by a timer for the life of the build rather than by its progress: `full:basis` and
/// `full:index` are single database statements that can run for minutes with nothing to report,
/// and a heartbeat that only rode along with progress writes would expire during them. The
/// stamp therefore means "the process running this build is alive", which is exactly what the
/// lease needs to know.
///
/// `claim_id` makes a superseded build's heartbeat a no-op, so it cannot keep a claim it no
/// longer holds alive.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only.
pub async fn touch_build<'e, E: PgExecutor<'e>>(exec: E, claim: BuildClaim) -> DbResult<()> {
    sqlx::query!(
        "UPDATE rec_build_state SET heartbeat_at = now() WHERE id AND claim_id = $1",
        claim.claim_id,
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// Release the build, recording how it ended.
///
/// **Must run on the failure path too.** This is where a run's own claim is released, so a build
/// that dies without calling it leaves the claim held until its lease expires — which is what
/// the lease exists for, but the operator still sees a run that is going nowhere until then.
///
/// Fenced by `claim_id`, and that fence is the load-bearing half: a build whose lease was
/// broken while it was still running would otherwise release the claim of the run that replaced
/// it, and write that run's `stage = 'idle'` over a build still in progress.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only.
pub async fn finish_build<'e, E: PgExecutor<'e>>(
    exec: E,
    claim: BuildClaim,
    series_built: i32,
    vocabulary: i32,
    dense_dims: i32,
    error: Option<&str>,
) -> DbResult<()> {
    sqlx::query!(
        "UPDATE rec_build_state \
            SET stage        = 'idle', \
                claim_id     = NULL, \
                finished_at  = now(), \
                stage_total  = 0, \
                series_built = $1, \
                vocabulary   = $2, \
                dense_dims   = $3, \
                error        = $4 \
          WHERE id AND claim_id = $5",
        series_built,
        vocabulary,
        dense_dims,
        error,
        claim.claim_id,
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
