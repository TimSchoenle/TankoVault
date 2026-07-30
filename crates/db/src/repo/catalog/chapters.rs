//! Chapters: idempotent upserts that report which rows were genuinely new, plus the counts
//! and listings the reading surfaces read back.

use crate::error::DbResult;
use sqlx::{FromRow, PgExecutor};
use tankovault_domain::{Chapter, ChapterId, ProviderId, SeriesId, SeriesSourceId};
use time::OffsetDateTime;
use uuid::Uuid;

/// One chapter to upsert (from an adapter's `fetch_chapters`).
pub struct ChapterUpsert {
    pub number: f64,
    pub volume: Option<i32>,
    pub title: Option<String>,
    /// RELATIVE link to the chapter page.
    pub path: String,
    pub published_at: Option<OffsetDateTime>,
}

/// Outcome of a single chapter upsert.
pub struct ChapterUpsertResult {
    pub number: f64,
    /// True when this row was newly inserted (a genuine discovery), false on update.
    pub inserted: bool,
}

#[derive(FromRow)]
struct ChapterRow {
    id: Uuid,
    series_source_id: Uuid,
    number: f64,
    volume: Option<i32>,
    title: Option<String>,
    path: String,
    published_at: Option<OffsetDateTime>,
    discovered_at: OffsetDateTime,
}

impl From<ChapterRow> for Chapter {
    fn from(r: ChapterRow) -> Self {
        Self {
            id: ChapterId::from_uuid(r.id),
            series_source_id: SeriesSourceId::from_uuid(r.series_source_id),
            number: r.number,
            volume: r.volume,
            title: r.title,
            path: r.path,
            published_at: r.published_at,
            discovered_at: r.discovered_at,
        }
    }
}

/// Upsert one chapter and report whether it was newly discovered.
///
/// The `xmax = 0` predicate distinguishes an inserted row (xmax 0) from an updated
/// row, giving both idempotent upsert and new-chapter detection in one round trip.
pub async fn upsert_chapter<'e, E: PgExecutor<'e>>(
    exec: E,
    source_id: SeriesSourceId,
    ch: &ChapterUpsert,
) -> DbResult<ChapterUpsertResult> {
    let inserted = sqlx::query_scalar!(
        "INSERT INTO chapters (id, series_source_id, number, volume, title, path, published_at) \
         VALUES ($1,$2,$3::float8::numeric(10,4),$4,$5,$6,$7) \
         ON CONFLICT (series_source_id, number) DO UPDATE \
            SET title = EXCLUDED.title, path = EXCLUDED.path, \
                published_at = COALESCE(EXCLUDED.published_at, chapters.published_at) \
         RETURNING (xmax = 0) AS \"inserted!\"",
        ChapterId::new().as_uuid(),
        source_id.as_uuid(),
        ch.number,
        ch.volume,
        ch.title.as_deref(),
        &ch.path,
        ch.published_at,
    )
    .fetch_one(exec)
    .await?;
    Ok(ChapterUpsertResult {
        number: ch.number,
        inserted,
    })
}

/// Upsert a whole chapter list in one statement, returning the numbers that were **new**.
///
/// The per-chapter [`upsert_chapter`] is kept for the single-chapter callers, but the ingest
/// path must not use it: a series with two thousand chapters meant two thousand sequential
/// round trips, each one holding open the transaction that also holds row locks on the shared
/// `tags` and `authors` rows — so one slow series blocked every other provider's ingest.
///
/// Same `xmax = 0` trick as the single-row version: an inserted row has `xmax` 0, an updated
/// one does not, so one statement gives both the idempotent upsert and the new-chapter
/// detection that drives `chapter.discovered`.
///
/// `DISTINCT ON` is load-bearing. `ON CONFLICT DO UPDATE` cannot touch the same row twice in
/// one statement (Postgres raises `21000`), and a provider listing the same chapter number
/// twice on one page is a real and recurring occurrence — the last spelling wins, matching
/// the row-at-a-time loop this replaces.
pub async fn upsert_chapters<'e, E: PgExecutor<'e>>(
    exec: E,
    source_id: SeriesSourceId,
    chapters: &[ChapterUpsert],
) -> DbResult<Vec<f64>> {
    if chapters.is_empty() {
        return Ok(Vec::new());
    }

    let ids: Vec<Uuid> = chapters
        .iter()
        .map(|_| ChapterId::new().as_uuid())
        .collect();
    let numbers: Vec<f64> = chapters.iter().map(|c| c.number).collect();
    let volumes: Vec<Option<i32>> = chapters.iter().map(|c| c.volume).collect();
    let titles: Vec<Option<String>> = chapters.iter().map(|c| c.title.clone()).collect();
    let paths: Vec<String> = chapters.iter().map(|c| c.path.clone()).collect();
    let published: Vec<Option<OffsetDateTime>> = chapters.iter().map(|c| c.published_at).collect();

    let rows = sqlx::query!(
        "INSERT INTO chapters (id, series_source_id, number, volume, title, path, published_at) \
         SELECT DISTINCT ON (u.number) \
                u.id, $2, u.number::float8::numeric(10,4), u.volume, u.title, u.path, u.published_at \
           FROM UNNEST($1::uuid[], $3::float8[], $4::int[], $5::text[], $6::text[], \
                       $7::timestamptz[]) \
                WITH ORDINALITY AS u(id, number, volume, title, path, published_at, ord) \
          ORDER BY u.number, u.ord DESC \
         ON CONFLICT (series_source_id, number) DO UPDATE \
            SET title = EXCLUDED.title, path = EXCLUDED.path, \
                published_at = COALESCE(EXCLUDED.published_at, chapters.published_at) \
         RETURNING number::float8 AS \"number!\", (xmax = 0) AS \"inserted!\"",
        &ids,
        source_id.as_uuid(),
        &numbers,
        &volumes as &[Option<i32>],
        &titles as &[Option<String>],
        &paths,
        &published as &[Option<OffsetDateTime>],
    )
    .fetch_all(exec)
    .await?;

    Ok(rows
        .into_iter()
        .filter(|r| r.inserted)
        .map(|r| r.number)
        .collect())
}

/// The highest chapter number stored for a source (fast-scan comparison key).
pub async fn max_chapter_number<'e, E: PgExecutor<'e>>(
    exec: E,
    source_id: SeriesSourceId,
) -> DbResult<Option<f64>> {
    let max = sqlx::query_scalar!(
        "SELECT MAX(number)::float8 AS \"max?\" FROM chapters WHERE series_source_id = $1",
        source_id.as_uuid(),
    )
    .fetch_one(exec)
    .await?;
    Ok(max)
}

/// The number of distinct **whole** chapters a source has — sub-chapter part releases
/// (e.g. `152.1`..`152.6`, fractional `number`) collapse into the one whole chapter they
/// belong to instead of each counting as its own chapter (frontend §9.2 "Read on" +
/// hero stat; mirrors the `floor(number)` tracking-count convention in
/// `repo::tracking`). Deliberately distinct from `series_sources.chapter_count`, which
/// stays a raw scanned-row count used for scan/sync bookkeeping.
pub async fn count_full_chapters<'e, E: PgExecutor<'e>>(
    exec: E,
    source_id: SeriesSourceId,
) -> DbResult<i32> {
    let count = sqlx::query_scalar!(
        "SELECT count(DISTINCT floor(number)) AS \"count!\" FROM chapters \
         WHERE series_source_id = $1",
        source_id.as_uuid(),
    )
    .fetch_one(exec)
    .await?;
    Ok(i32::try_from(count).unwrap_or(i32::MAX))
}

/// List chapters of a source, newest first (resolved to absolute links by the caller).
pub async fn list_chapters<'e, E: PgExecutor<'e>>(
    exec: E,
    source_id: SeriesSourceId,
) -> DbResult<Vec<Chapter>> {
    let rows = sqlx::query_as!(
        ChapterRow,
        "SELECT id, series_source_id, number::float8 AS \"number!\", volume, title, path, \
         published_at, discovered_at FROM chapters WHERE series_source_id = $1 \
         ORDER BY number DESC",
        source_id.as_uuid(),
    )
    .fetch_all(exec)
    .await?;
    Ok(rows.into_iter().map(Chapter::from).collect())
}

/// Distinct whole-chapter counts for a series, one row per provider.
///
/// The batched counterpart to [`count_full_chapters_across`]. `services/api`'s series-detail
/// handler folds a series' sources into one group per provider and needs a count for each; it
/// used to issue one `count_full_chapters_across` per group, which is an N+1 over a set the
/// database can group in a single pass.
///
/// # Errors
/// Propagates any database failure.
pub async fn count_full_chapters_by_provider<'e, E: PgExecutor<'e>>(
    exec: E,
    series_id: SeriesId,
) -> DbResult<Vec<(ProviderId, i32)>> {
    #[derive(FromRow)]
    struct Row {
        provider_id: Uuid,
        count: i64,
    }
    let rows = sqlx::query_as!(
        Row,
        "SELECT ss.provider_id, count(DISTINCT floor(c.number)) AS \"count!\"          FROM series_sources ss JOIN chapters c ON c.series_source_id = ss.id          WHERE ss.series_id = $1          GROUP BY ss.provider_id",
        series_id.as_uuid(),
    )
    .fetch_all(exec)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| {
            (
                ProviderId::from_uuid(r.provider_id),
                i32::try_from(r.count).unwrap_or(i32::MAX),
            )
        })
        .collect())
}

/// The number of distinct **whole** chapters across a *set* of sources — the merge-aware
/// counterpart to [`count_full_chapters`]. Used when several `series_sources` rows of the
/// same provider are folded into one reader-visible "completing" source (design §10 same-source
/// smart merge): counting each entry separately and summing would double-count a whole chapter
/// two entries happen to share, so the count is taken over the *union* of their chapters.
/// An empty slice yields `0`.
pub async fn count_full_chapters_across<'e, E: PgExecutor<'e>>(
    exec: E,
    source_ids: &[SeriesSourceId],
) -> DbResult<i32> {
    let ids: Vec<Uuid> = source_ids.iter().map(|s| s.as_uuid()).collect();
    let count = sqlx::query_scalar!(
        "SELECT count(DISTINCT floor(number)) AS \"count!\" FROM chapters \
         WHERE series_source_id = ANY($1)",
        &ids,
    )
    .fetch_one(exec)
    .await?;
    Ok(i32::try_from(count).unwrap_or(i32::MAX))
}

/// List the chapters spanning a *set* of sources as a single de-duplicated, newest-first
/// list — the merge-aware counterpart to [`list_chapters`]. When several `series_sources`
/// rows of the same provider are folded into one "completing" source, their chapter lists are
/// complementary (e.g. one entry carries the early chapters, another the later ones); this
/// unions them and, when two entries expose the *same* chapter number, keeps a single row
/// (the earliest-discovered) via `DISTINCT ON (number)` so the reader never sees a duplicate.
/// All rows belong to the same provider, so the caller resolves paths against one `base_url`.
pub async fn list_chapters_across<'e, E: PgExecutor<'e>>(
    exec: E,
    source_ids: &[SeriesSourceId],
) -> DbResult<Vec<Chapter>> {
    let ids: Vec<Uuid> = source_ids.iter().map(|s| s.as_uuid()).collect();
    let rows = sqlx::query_as!(
        ChapterRow,
        "SELECT DISTINCT ON (number) id, series_source_id, number::float8 AS \"number!\", \
         volume, title, path, published_at, discovered_at FROM chapters \
         WHERE series_source_id = ANY($1) \
         ORDER BY number DESC, discovered_at ASC",
        &ids,
    )
    .fetch_all(exec)
    .await?;
    Ok(rows.into_iter().map(Chapter::from).collect())
}
