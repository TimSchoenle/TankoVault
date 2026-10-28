//! The dense half: storing embeddings, owning the HNSW index, and searching it.

use crate::error::DbResult;
use sqlx::{FromRow, PgExecutor, PgPool};
use std::fmt::Write as _;
use tankovault_domain::SeriesId;
use uuid::Uuid;

/// Renders a vector as a pgvector literal.
///
/// A string, because `sqlx` has no native `halfvec` codec and this workspace touches the type in
/// two statements. The literal is cast server-side, so malformed input fails the statement
/// instead of storing something unreadable.
fn to_literal(values: &[f32]) -> String {
    let mut out = String::with_capacity(values.len() * 8 + 2);
    out.push('[');
    for (i, value) in values.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        // Finite-guarded: pgvector rejects `NaN`, failing the batch that carried it. Zero is safe
        // because a zero component contributes nothing to a cosine.
        let safe = if value.is_finite() { *value } else { 0.0 };
        // `write!` into the buffer, not `push_str(&format!(..))`: this runs per component per
        // series, a million allocations per build.
        let _ = write!(out, "{safe:.6}");
    }
    out.push(']');
    out
}

/// Stores one batch of embeddings.
///
/// A vector of the wrong width fails, never padded: the column is `halfvec(128)`, and a
/// mismatch means the basis and the schema disagree.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only.
pub async fn write_embeddings<'e, E: PgExecutor<'e>>(
    exec: E,
    series_ids: &[SeriesId],
    vectors: &[Vec<f32>],
    generation: i32,
) -> DbResult<u64> {
    let ids: Vec<Uuid> = series_ids.iter().copied().map(SeriesId::as_uuid).collect();
    let literals: Vec<String> = vectors.iter().map(|v| to_literal(v)).collect();

    let affected = sqlx::query!(
        "INSERT INTO series_embedding (series_id, embedding, generation) \
         SELECT * FROM unnest($1::uuid[], $2::text[]::halfvec(128)[], $3::int[]) \
         ON CONFLICT (series_id) DO UPDATE \
            SET embedding  = EXCLUDED.embedding, \
                generation = EXCLUDED.generation, \
                built_at   = now()",
        &ids,
        &literals,
        &vec![generation; ids.len()],
    )
    .execute(exec)
    .await?
    .rows_affected();
    Ok(affected)
}

/// Builds the HNSW index if it is not already there.
///
/// **Owned by the builder, not a migration.** `CREATE INDEX CONCURRENTLY` cannot run inside the
/// migrator's implicit transaction (see migration 0020), and a blocking build over a million rows
/// is minutes of `ACCESS EXCLUSIVE` on a table the API reads on every request. Takes a pool, since
/// an `Executor` may be a transaction.
///
/// An interrupted concurrent build leaves an `INVALID` index that the planner ignores, so search
/// works slowly, and the next run's `IF NOT EXISTS` finds it present. `REINDEX` is an operator
/// action, reported by the build state.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only.
pub async fn create_embedding_index(pool: &PgPool, m: i32, ef_construction: i32) -> DbResult<()> {
    // Not bindable: index storage parameters are part of the utility statement's grammar. Both
    // are clamped to the range pgvector accepts before interpolation.
    let m = m.clamp(2, 100);
    let ef_construction = ef_construction.clamp(4, 1000);
    // `AssertSqlSafe` because the statement is interpolated. The audit it demands is the clamp
    // above: both values are `i32` narrowed to the range pgvector accepts, so nothing reaches
    // the format string that is not a small integer.
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "CREATE INDEX CONCURRENTLY IF NOT EXISTS series_embedding_hnsw \
         ON series_embedding USING hnsw (embedding halfvec_cosine_ops) \
         WITH (m = {m}, ef_construction = {ef_construction})"
    )))
    .execute(pool)
    .await?;
    Ok(())
}

/// Sets `hnsw.ef_search` for the searches that follow on this connection.
///
/// Takes a `&mut PgConnection`, not a pool: `SET` is per session, so a pool would configure
/// whichever connection served the call while the searches run on another. A request that wants
/// a recall setting holds the connection it searches on.
///
/// The value is clamped to what pgvector accepts before interpolation; see
/// [`create_embedding_index`] for why it cannot be a bind parameter.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only.
pub async fn set_ef_search(conn: &mut sqlx::PgConnection, ef_search: i64) -> DbResult<()> {
    let ef_search = ef_search.clamp(1, 1000);
    // `AssertSqlSafe` because `SET` takes no bind parameters. The audit it demands is the clamp
    // above: nothing reaches the format string but a small integer.
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "SET hnsw.ef_search = {ef_search}"
    )))
    .execute(conn)
    .await?;
    Ok(())
}

/// The row both neighbour searches read.
#[derive(FromRow)]
struct NeighbourRow {
    series_id: Uuid,
    score: f64,
}

/// A retrieved neighbour and how close it is.
pub struct Neighbour {
    /// The retrieved series.
    pub series_id: SeriesId,
    /// Cosine similarity in `[0, 1]`, not the distance the index ranks by.
    pub score: f32,
}

/// Returns one series' embedding, as the literal the search binds.
///
/// `None` for a series whose embedding has not been built yet.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only.
pub async fn embedding_of<'e, E: PgExecutor<'e>>(
    exec: E,
    series_id: SeriesId,
) -> DbResult<Option<String>> {
    let literal = sqlx::query_scalar!(
        "SELECT embedding::text FROM series_embedding WHERE series_id = $1",
        series_id.as_uuid(),
    )
    .fetch_optional(exec)
    .await?;
    Ok(literal.flatten())
}

/// Returns the nearest recommendable neighbours of a vector.
///
/// The ANN scan is a bare `ORDER BY … LIMIT` in a subquery, with every filter applied *outside*
/// it. Joins pushed inside would let the planner abandon the index and sort the table, turning a
/// two-millisecond lookup into a full scan of a million embeddings; `repo_query_plans` catches
/// that. The subquery over-fetches (`limit * overfetch`) so filtering has candidates to remove;
/// too small, and a deployment with many unrecommendable series silently gets short results.
///
/// `include_adult` is the caller's resolved answer for *this* reader. This backs a route that
/// answers unauthenticated callers, where the only correct value is `false`.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only.
pub async fn nearest_neighbours<'e, E: PgExecutor<'e>>(
    exec: E,
    embedding: &str,
    exclude: SeriesId,
    include_adult: bool,
    limit: i64,
    overfetch: i64,
) -> DbResult<Vec<Neighbour>> {
    let rows = sqlx::query_as!(
        NeighbourRow,
        "SELECT c.series_id, (1 - c.distance)::float8 AS \"score!\" \
         FROM ( \
           SELECT series_id, embedding <=> $1::text::halfvec(128) AS distance \
           FROM series_embedding \
           ORDER BY embedding <=> $1::text::halfvec(128) \
           LIMIT $5 \
         ) c \
         JOIN series_prior p ON p.series_id = c.series_id AND p.recommendable \
         JOIN series s ON s.id = c.series_id AND (NOT s.adult_gated OR $3) \
         WHERE c.series_id <> $2 \
         ORDER BY c.distance \
         LIMIT $4",
        embedding,
        exclude.as_uuid(),
        include_adult,
        limit,
        overfetch,
    )
    .fetch_all(exec)
    .await?;

    #[expect(
        clippy::cast_possible_truncation,
        reason = "a cosine in [0,1] loses nothing meaningful as f32, and the score is a ranking \
                  key rather than a measurement"
    )]
    Ok(rows
        .into_iter()
        .map(|r| Neighbour {
            series_id: SeriesId::from_uuid(r.series_id),
            score: r.score as f32,
        })
        .collect())
}

/// Returns the nearest recommendable neighbours of a vector, excluding a whole set.
///
/// The shape and constraint of [`nearest_neighbours`]: a bare `ORDER BY … LIMIT` with every
/// filter outside it.
///
/// The exclusion set is the reader's: everything tracked, plus everything refused. It applies
/// after the index ranks, so `overfetch` matters more than on the public endpoint: a
/// thousand-entry watchlist removes a thousand candidates.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only.
pub async fn nearest_excluding<'e, E: PgExecutor<'e>>(
    exec: E,
    embedding: &str,
    exclude: &[SeriesId],
    include_adult: bool,
    limit: i64,
    overfetch: i64,
) -> DbResult<Vec<Neighbour>> {
    let excluded: Vec<Uuid> = exclude.iter().copied().map(SeriesId::as_uuid).collect();

    let rows = sqlx::query_as!(
        NeighbourRow,
        "SELECT c.series_id, (1 - c.distance)::float8 AS \"score!\" \
         FROM ( \
           SELECT series_id, embedding <=> $1::text::halfvec(128) AS distance \
           FROM series_embedding \
           ORDER BY embedding <=> $1::text::halfvec(128) \
           LIMIT $5 \
         ) c \
         JOIN series_prior p ON p.series_id = c.series_id AND p.recommendable \
         JOIN series s ON s.id = c.series_id AND (NOT s.adult_gated OR $3) \
         WHERE NOT (c.series_id = ANY($2)) \
         ORDER BY c.distance \
         LIMIT $4",
        embedding,
        &excluded,
        include_adult,
        limit,
        overfetch,
    )
    .fetch_all(exec)
    .await?;

    #[expect(
        clippy::cast_possible_truncation,
        reason = "a cosine in [0,1] loses nothing meaningful as f32; the score is a ranking key"
    )]
    Ok(rows
        .into_iter()
        .map(|r| Neighbour {
            series_id: SeriesId::from_uuid(r.series_id),
            score: r.score as f32,
        })
        .collect())
}

/// Returns the mean embedding of a set of series, the reader's centre of gravity.
///
/// Unweighted, over seeds that are already the top of the affinity ordering. pgvector has no
/// scalar multiply, so weighting would mean parsing every vector back out of text in the request
/// path; a weighted mean belongs in the builder. `None` when no member is embedded, so
/// profile-vector retrieval has nothing to search with.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only.
pub async fn mean_embedding<'e, E: PgExecutor<'e>>(
    exec: E,
    series_ids: &[SeriesId],
) -> DbResult<Option<String>> {
    let ids: Vec<Uuid> = series_ids.iter().copied().map(SeriesId::as_uuid).collect();
    let literal = sqlx::query_scalar!(
        "SELECT avg(embedding)::halfvec(128)::text \
         FROM series_embedding WHERE series_id = ANY($1)",
        &ids,
    )
    .fetch_one(exec)
    .await?;
    Ok(literal)
}
