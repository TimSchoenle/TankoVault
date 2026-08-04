//! The dense half: storing embeddings, owning the HNSW index, and searching it.

use crate::error::DbResult;
use sqlx::{FromRow, PgExecutor, PgPool};
use std::fmt::Write as _;
use tankovault_domain::SeriesId;
use uuid::Uuid;

/// Render a vector as a pgvector literal.
///
/// A string, because `sqlx` has no native `halfvec` codec and adding one would mean a
/// `sqlx::Type` implementation for a type this workspace touches in exactly two statements. The
/// literal is cast server-side; malformed input fails the statement rather than storing
/// something unreadable.
fn to_literal(values: &[f32]) -> String {
    let mut out = String::with_capacity(values.len() * 8 + 2);
    out.push('[');
    for (i, value) in values.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        // Finite-guarded: a NaN would serialise as `NaN`, which pgvector rejects — but only at
        // write time, and only for the batch that carried it. Zero is the safe substitute
        // because a zero component contributes nothing to a cosine.
        let safe = if value.is_finite() { *value } else { 0.0 };
        // `write!` into the buffer rather than `push_str(&format!(..))`: this runs once per
        // component per series, so a per-component allocation is a million of them per build.
        let _ = write!(out, "{safe:.6}");
    }
    out.push(']');
    out
}

/// Store one batch of embeddings.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only. A vector of the wrong width fails here rather than being
/// silently padded — the column is `halfvec(128)` and a mismatch means the basis and the schema
/// disagree, which no row should survive.
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

/// Build the HNSW index if it is not already there.
///
/// **Owned by the builder, not by a migration.** `CREATE INDEX CONCURRENTLY` cannot run inside
/// the migrator's implicit transaction (migration 0020 documents that trap at length), and a
/// blocking build over a million rows is minutes of `ACCESS EXCLUSIVE` on a table the API reads
/// on every request. Here it can be concurrent, retried, and reported on.
///
/// Takes a pool rather than an executor because `CONCURRENTLY` refuses to run inside a
/// transaction, and an `Executor` may be one.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only. An interrupted concurrent build leaves an `INVALID` index that
/// the planner ignores — the search still works, slowly — and the next run's `IF NOT EXISTS`
/// finds it present, so `REINDEX` is an operator action. Reported by the build state, not
/// silently retried.
pub async fn create_embedding_index(pool: &PgPool, m: i32, ef_construction: i32) -> DbResult<()> {
    // Not bindable: index storage parameters are part of the utility statement's grammar, not
    // expressions, so they cannot be parameters. Both are clamped to the range pgvector accepts
    // rather than interpolated as given.
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

/// A retrieved neighbour and how close it is.
pub struct Neighbour {
    pub series_id: SeriesId,
    /// Cosine similarity in `[0, 1]`, not the distance the index ranks by.
    pub score: f32,
}

/// One series' embedding, as the literal the search binds.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only; a series with no embedding is `Ok(None)` — it has not been
/// built yet, which is not an error.
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

/// The nearest recommendable neighbours of a vector.
///
/// # The shape of this query is load-bearing
///
/// The ANN scan is a bare `ORDER BY … LIMIT` in a subquery, and every filter is applied
/// *outside* it. Pushing the joins inside would let the planner decide the predicate is
/// selective enough to abandon the index and sort the table — which produces the same rows,
/// slightly better ones in fact, and turns a two-millisecond lookup into a full scan of a
/// million embeddings. That is the exact failure mode `repo_query_plans` exists to catch, and
/// pgvector cannot express "filtered ANN" any other way.
///
/// The subquery therefore over-fetches (`limit * overfetch`) so that filtering has candidates
/// left to remove. An over-fetch that is too small silently returns fewer rows than asked for
/// once a deployment marks many series unrecommendable.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only.
pub async fn nearest_neighbours<'e, E: PgExecutor<'e>>(
    exec: E,
    embedding: &str,
    exclude: SeriesId,
    limit: i64,
    overfetch: i64,
) -> DbResult<Vec<Neighbour>> {
    #[derive(FromRow)]
    struct Row {
        series_id: Uuid,
        score: f64,
    }
    let rows = sqlx::query_as!(
        Row,
        "SELECT c.series_id, (1 - c.distance)::float8 AS \"score!\" \
         FROM ( \
           SELECT series_id, embedding <=> $1::text::halfvec(128) AS distance \
           FROM series_embedding \
           ORDER BY embedding <=> $1::text::halfvec(128) \
           LIMIT $4 \
         ) c \
         JOIN series_prior p ON p.series_id = c.series_id AND p.recommendable \
         JOIN series s ON s.id = c.series_id AND NOT s.is_adult \
         WHERE c.series_id <> $2 \
         ORDER BY c.distance \
         LIMIT $3",
        embedding,
        exclude.as_uuid(),
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
