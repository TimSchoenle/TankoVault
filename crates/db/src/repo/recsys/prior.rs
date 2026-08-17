//! Appeal priors: what to recommend when nothing else has an opinion, and what not to recommend
//! at all.

use crate::error::DbResult;
use sqlx::{FromRow, PgExecutor};
use tankovault_domain::{ContentType, SeriesId, SeriesStatus};
use uuid::Uuid;

/// The catalogue row behind [`summaries_in_order`].
#[derive(FromRow)]
struct SummaryRow {
    id: Uuid,
    canonical_title: String,
    normalized_title: String,
    description: Option<String>,
    cover_url: Option<String>,
    content_type: ContentType,
    status: SeriesStatus,
    release_year: Option<i32>,
    created_at: time::OffsetDateTime,
    updated_at: time::OffsetDateTime,
    source_count: i64,
}

/// The row [`prior_inputs_for`] reads.
#[derive(FromRow)]
struct InputsRow {
    id: Uuid,
    watchers: i64,
    sources: i64,
    chapters: i64,
    external_score: Option<f32>,
    external_popularity: Option<i32>,
    descriptive_features: i64,
    adult_gated: bool,
    has_active_source: bool,
}

/// The raw signals a prior is computed from, before blending.
pub struct PriorInputs {
    pub series_id: SeriesId,
    pub watchers: i64,
    pub sources: i64,
    pub chapters: i64,
    pub external_score: Option<f32>,
    pub external_popularity: Option<i32>,
    /// Tags and authors only — **not** every feature.
    ///
    /// Status, decade and length are derived from columns every series has, so counting them
    /// would let a completely unenriched series clear a "has enough metadata" gate on the
    /// strength of facts that describe nothing. Only a tag or an author distinguishes one series
    /// from another.
    pub descriptive_features: i64,
    /// Whether the adult gate is closed on this series.
    ///
    /// Reported to the build, **not acted on by it**. The exclusion belongs at read time, where
    /// the reader's own opt-in is known. A build that also excluded these would make that opt-in
    /// unreachable: every retrieval path joins `series_prior.recommendable`, so a series the
    /// build refused can never be recovered by a read-time filter, however permissive.
    pub adult_gated: bool,
    pub has_active_source: bool,
}

/// One page of series ids, for a caller that will then ask for something about them.
///
/// Deliberately separate from [`prior_inputs_for`]. Paging *and* aggregating in one statement
/// puts three correlated aggregates on every row the planner thinks it might scan, and since a
/// generic plan cannot see the `LIMIT`, it costs them against the whole catalogue —
/// `repo_query_plans` measured 1.8x the ceiling. Splitting gives the planner a bounded id set to
/// aggregate over, which is what it actually gets at run time.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only; an exhausted walk is an empty `Vec`.
pub async fn page_series_ids<'e, E: PgExecutor<'e>>(
    exec: E,
    after: Option<SeriesId>,
    limit: i64,
) -> DbResult<Vec<SeriesId>> {
    let rows: Vec<Uuid> = sqlx::query_scalar!(
        "SELECT id FROM series WHERE id > $1 ORDER BY id LIMIT $2",
        after.map_or_else(Uuid::nil, SeriesId::as_uuid),
        limit,
    )
    .fetch_all(exec)
    .await?;
    Ok(rows.into_iter().map(SeriesId::from_uuid).collect())
}

/// The prior inputs for a bounded set of series.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only; an id with no live series is simply absent from the result.
pub async fn prior_inputs_for<'e, E: PgExecutor<'e>>(
    exec: E,
    series_ids: &[SeriesId],
) -> DbResult<Vec<PriorInputs>> {
    let ids: Vec<Uuid> = series_ids.iter().copied().map(SeriesId::as_uuid).collect();
    let rows = sqlx::query_as!(
        InputsRow,
        // `id!` and `adult_gated!`: both are `NOT NULL` on `series`, but the three `LEFT JOIN
        // LATERAL`s make sqlx treat the whole row as nullable, so the overrides restore what the
        // schema already guarantees.
        "SELECT s.id AS \"id!\", \
                COALESCE(w.n, 0) AS \"watchers!\", \
                COALESCE(src.n, 0) AS \"sources!\", \
                COALESCE(ch.n, 0) AS \"chapters!\", \
                s.external_score, \
                s.external_popularity, \
                COALESCE(( \
                  SELECT count(*) FROM rec_features rf \
                  WHERE rf.id = ANY(f.feature_ids) AND rf.kind IN ('tag', 'author') \
                ), 0) AS \"descriptive_features!\", \
                s.adult_gated AS \"adult_gated!\", \
                COALESCE(src.active, false) AS \"has_active_source!\" \
         FROM series s \
         LEFT JOIN series_features f ON f.series_id = s.id \
         LEFT JOIN LATERAL ( \
           SELECT count(*) AS n FROM watchlist_entries w WHERE w.series_id = s.id \
         ) w ON true \
         LEFT JOIN LATERAL ( \
           SELECT count(*) AS n, bool_or(ss.state = 'active') AS active \
           FROM series_sources ss WHERE ss.series_id = s.id \
         ) src ON true \
         LEFT JOIN LATERAL ( \
           SELECT count(DISTINCT c.number_milli / 10000) AS n \
           FROM series_sources ss JOIN chapters c ON c.series_source_id = ss.id \
           WHERE ss.series_id = s.id \
         ) ch ON true \
         WHERE s.id = ANY($1)",
        &ids,
    )
    .fetch_all(exec)
    .await?;

    Ok(rows
        .into_iter()
        .map(|r| PriorInputs {
            series_id: SeriesId::from_uuid(r.id),
            watchers: r.watchers,
            sources: r.sources,
            chapters: r.chapters,
            external_score: r.external_score,
            external_popularity: r.external_popularity,
            descriptive_features: r.descriptive_features,
            adult_gated: r.adult_gated,
            has_active_source: r.has_active_source,
        })
        .collect())
}

/// Store one batch of computed priors.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only.
pub async fn write_priors<'e, E: PgExecutor<'e>>(
    exec: E,
    series_ids: &[SeriesId],
    priors: &[f32],
    watchers: &[i32],
    velocities: &[f32],
    recommendable: &[bool],
    generation: i32,
) -> DbResult<u64> {
    let ids: Vec<Uuid> = series_ids.iter().copied().map(SeriesId::as_uuid).collect();
    let affected = sqlx::query!(
        "INSERT INTO series_prior \
            (series_id, prior, watchers, velocity, recommendable, generation) \
         SELECT * FROM unnest($1::uuid[], $2::real[], $3::int[], $4::real[], $5::bool[], $6::int[]) \
         ON CONFLICT (series_id) DO UPDATE \
            SET prior         = EXCLUDED.prior, \
                watchers      = EXCLUDED.watchers, \
                velocity      = EXCLUDED.velocity, \
                recommendable = EXCLUDED.recommendable, \
                generation    = EXCLUDED.generation, \
                built_at      = now()",
        &ids,
        priors,
        watchers,
        velocities,
        recommendable,
        &vec![generation; ids.len()],
    )
    .execute(exec)
    .await?
    .rows_affected();
    Ok(affected)
}

/// The most broadly appealing recommendable series — cold start, and shelf backfill.
///
/// `include_adult` is the caller's resolved answer for this reader. It exists because this path
/// serves two callers with different context: the shelf, which knows exactly who is asking, and
/// the public similarity fallback, which may not. Passing `false` is always safe; passing it
/// where the reader *has* opted in makes their shelf quietly narrow as the other paths run dry.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only.
pub async fn top_by_prior<'e, E: PgExecutor<'e>>(
    exec: E,
    include_adult: bool,
    limit: i64,
) -> DbResult<Vec<SeriesId>> {
    let rows: Vec<Uuid> = sqlx::query_scalar!(
        "SELECT p.series_id FROM series_prior p \
         JOIN series s ON s.id = p.series_id AND (NOT s.adult_gated OR $2) \
         WHERE p.recommendable \
         ORDER BY p.prior DESC, p.series_id \
         LIMIT $1",
        limit,
        include_adult,
    )
    .fetch_all(exec)
    .await?;
    Ok(rows.into_iter().map(SeriesId::from_uuid).collect())
}

/// Summary rows for a named set of series, in the order the caller asked for them.
///
/// Retrieval hands back ids in *relevance* order, and a `= ANY` returns them in whatever order
/// the heap does. Re-sorting here rather than at the call site keeps the ranking the retrieval
/// produced from being silently replaced by physical row order — which looks like a working
/// endpoint returning slightly wrong answers.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only; an id with no live series is absent from the result rather
/// than an error, which is what makes this safe to call with ids from a stale model.
pub async fn summaries_in_order<'e, E: PgExecutor<'e>>(
    exec: E,
    order: &[SeriesId],
) -> DbResult<Vec<crate::repo::catalog::SeriesListItem>> {
    use tankovault_domain::Series;

    let ids: Vec<Uuid> = order.iter().copied().map(SeriesId::as_uuid).collect();

    let rows = sqlx::query_as!(
        SummaryRow,
        "SELECT s.id, s.canonical_title, s.normalized_title, s.description, s.cover_url, \
                s.content_type AS \"content_type: ContentType\", \
                s.status AS \"status: SeriesStatus\", \
                s.release_year, s.created_at, s.updated_at, \
                (SELECT count(*) FROM series_sources ss WHERE ss.series_id = s.id) \
                  AS \"source_count!\" \
         FROM series s WHERE s.id = ANY($1)",
        &ids,
    )
    .fetch_all(exec)
    .await?;

    let mut by_id: std::collections::HashMap<Uuid, crate::repo::catalog::SeriesListItem> = rows
        .into_iter()
        .map(|r| {
            (
                r.id,
                crate::repo::catalog::SeriesListItem {
                    series: Series {
                        id: SeriesId::from_uuid(r.id),
                        canonical_title: r.canonical_title,
                        normalized_title: r.normalized_title,
                        description: r.description,
                        cover_url: r.cover_url,
                        content_type: r.content_type,
                        status: r.status,
                        release_year: r.release_year,
                        created_at: r.created_at,
                        updated_at: r.updated_at,
                    },
                    source_count: r.source_count,
                },
            )
        })
        .collect();

    Ok(order
        .iter()
        .filter_map(|id| by_id.remove(&id.as_uuid()))
        .collect())
}
