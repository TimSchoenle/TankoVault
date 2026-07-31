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
///
/// # Errors
/// [`crate::DbError::Sqlx`] only — no other variant is reachable. A `source_id` that does not
/// exist is a foreign-key violation and so a 500. `number` is cast to `numeric(10,4)`, so a value
/// outside that precision — including a non-finite `f64` — is a driver error rather than a
/// rounded or dropped chapter; that is deliberate, since silently storing the wrong number would
/// attach a chapter to the wrong position in someone's reading progress.
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
///
/// # Errors
/// [`crate::DbError::Sqlx`] only — no other variant is reachable, with the same foreign-key and
/// `numeric(10,4)` shapes as [`upsert_chapter`]; one bad chapter fails the whole statement, so
/// this is all-or-nothing rather than a partial list. An empty `chapters` returns an empty `Vec`
/// without issuing a statement.
///
/// The empty `Vec` is doing double duty and the caller must not read it as failure: it is also
/// what a *converged* re-scan returns — every chapter matched the conflict arm, none was new —
/// which is the ordinary result of the overwhelming majority of scans. Only the `Err` says
/// nothing was written.
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
    // Borrowed: the arrays are bound for one statement and dropped, so cloning a title and a
    // path per chapter allocates twice for every chapter of every scanned series (PERF-19).
    let titles: Vec<Option<&str>> = chapters.iter().map(|c| c.title.as_deref()).collect();
    let paths: Vec<&str> = chapters.iter().map(|c| c.path.as_str()).collect();
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
        &titles as _,
        &paths as _,
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
///
/// # Errors
/// [`crate::DbError::Sqlx`] only — no other variant is reachable. A source with no chapters, and
/// a `source_id` that does not exist, are the same `Ok(None)`: `MAX` over an empty set is `NULL`,
/// and `fetch_one` still returns a row because the aggregate always produces one. Not
/// [`crate::DbError::NotFound`], and not `Ok(Some(0.0))` — the caller compares this against the
/// highest number a listing offers, and a zero would read as "the source has chapter 0", which
/// would skip every chapter below the first one seen.
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
///
/// # Errors
/// [`crate::DbError::Sqlx`] only — no other variant is reachable. A source with no chapters and
/// an unknown `source_id` are both `Ok(0)` — `count` over an empty set is `0` and never `NULL`,
/// which is what the `count!` override asserts. The `i32::try_from(...).unwrap_or(i32::MAX)` saturates rather than
/// erroring or wrapping: a count above two billion is not a value any caller can render
/// meaningfully, and clamping keeps a display path from failing over a number that cannot occur.
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
///
/// # Errors
/// [`crate::DbError::Sqlx`] only — no other variant is reachable. An unknown `source_id` and a
/// registered-but-never-scanned source are the same empty `Vec`, not [`crate::DbError::NotFound`]
/// — a stub registered by the catalogue walk legitimately has no chapters until its `Series`
/// task runs, so an empty list is a normal intermediate state rather than a miss.
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
/// [`crate::DbError::Sqlx`] only — no other variant is reachable. A series with no chapters
/// anywhere is an empty `Vec`, and so is an unknown `series_id`. The join is inner, so a provider
/// whose sources carry *no* chapters has no row at all rather than a row of `0` — callers
/// building a per-provider view must treat a missing key as zero rather than as an absent
/// provider.
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
///
/// # Errors
/// [`crate::DbError::Sqlx`] only — no other variant is reachable. The empty-slice case still
/// issues the statement — `= ANY('{}')` matches nothing — so `Ok(0)` there costs a round trip
/// rather than short-circuiting; it is on a per-series read path that already has a connection,
/// and the honesty of one code path is worth more than the saving. Saturates at [`i32::MAX`] for
/// the same reason as [`count_full_chapters`].
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
///
/// # Errors
/// [`crate::DbError::Sqlx`] only — no other variant is reachable. An empty `source_ids`, ids that
/// name nothing, and sources with no chapters are all the same empty `Vec`, not
/// [`crate::DbError::NotFound`].
///
/// The final sentence of the summary is a **precondition this function does not check**: it
/// unions whatever ids it is given, so passing sources from two different providers returns a
/// list the caller will then resolve against one `base_url`, producing links that point at the
/// wrong host. The grouping that guarantees one provider happens in `services/api`.
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
