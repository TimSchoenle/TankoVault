//! Chapters: idempotent upserts that report which rows were genuinely new, plus the counts
//! and listings the reading surfaces read back.

use crate::error::DbResult;
use sqlx::{FromRow, PgExecutor};
use tankovault_domain::{
    Chapter, ChapterAccess, ChapterId, ProviderId, SeriesId, SeriesSourceId, UserId,
};
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
    /// What the provider says about reading it: free, or behind a paywall.
    pub access: ChapterAccess,
    /// When an early-access chapter opens, where the provider states a date. `None` on a
    /// locked chapter means "no date published", which the read paths keep treating as locked.
    pub unlocks_at: Option<OffsetDateTime>,
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

/// Upsert one chapter and report whether it was newly discovered (`xmax = 0`).
///
/// # Errors
/// [`crate::DbError::Sqlx`] only. A non-finite or out-of-precision `number` errors rather than
/// rounding — a silently wrong number would misplace a reader's progress.
pub async fn upsert_chapter<'e, E: PgExecutor<'e>>(
    exec: E,
    source_id: SeriesSourceId,
    ch: &ChapterUpsert,
) -> DbResult<ChapterUpsertResult> {
    let inserted = sqlx::query_scalar!(
        // `access`/`unlocks_at` are overwritten, not coalesced: they are the provider's current
        // verdict, and the whole point is that a chapter which has since unlocked stops being
        // reported as locked. Coalescing would freeze the first answer forever.
        "INSERT INTO chapters (id, series_source_id, number, volume, title, path, published_at, \
                               access, unlocks_at) \
         VALUES ($1,$2,$3::float8::numeric(10,4),$4,$5,$6,$7,$8,$9) \
         ON CONFLICT (series_source_id, number) DO UPDATE \
            SET title = EXCLUDED.title, path = EXCLUDED.path, \
                published_at = COALESCE(EXCLUDED.published_at, chapters.published_at), \
                access = EXCLUDED.access, unlocks_at = EXCLUDED.unlocks_at \
         RETURNING (xmax = 0) AS \"inserted!\"",
        ChapterId::new().as_uuid(),
        source_id.as_uuid(),
        ch.number,
        ch.volume,
        ch.title.as_deref(),
        &ch.path,
        ch.published_at,
        ch.access as ChapterAccess,
        ch.unlocks_at,
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
/// One statement, not a loop over [`upsert_chapter`] — the ingest transaction holds row locks
/// on shared `tags`/`authors` rows, so per-chapter round trips there stall other ingests.
/// `DISTINCT ON` avoids `ON CONFLICT DO UPDATE` aborting when a page lists one chapter number
/// twice.
///
/// **The `DISTINCT ON` key must stay the cast expression, not the raw `float8`.** The unique
/// index is on `numeric(10,4)`, so two `float8` values that differ only past the fourth decimal
/// are distinct to the dedup and identical to the constraint — both rows survive and the
/// statement aborts with "ON CONFLICT DO UPDATE command cannot affect row a second time",
/// failing the whole ingest batch. Same reason `ORDER BY` casts: `DISTINCT ON` requires its
/// leading sort key to be the dedup expression, and `u.ord DESC` behind it is what makes the
/// last listing of a repeated number win.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only; one bad chapter fails the whole batch. An empty `Vec` means
/// either no input or a fully-converged re-scan — only `Err` means nothing was written.
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
    // Borrowed, not cloned: arrays live only for this statement.
    let titles: Vec<Option<&str>> = chapters.iter().map(|c| c.title.as_deref()).collect();
    let paths: Vec<&str> = chapters.iter().map(|c| c.path.as_str()).collect();
    let published: Vec<Option<OffsetDateTime>> = chapters.iter().map(|c| c.published_at).collect();
    let accesses: Vec<ChapterAccess> = chapters.iter().map(|c| c.access).collect();
    let unlocks: Vec<Option<OffsetDateTime>> = chapters.iter().map(|c| c.unlocks_at).collect();

    let rows = sqlx::query!(
        // See `upsert_chapter` on why the access columns are overwritten rather than coalesced.
        "INSERT INTO chapters (id, series_source_id, number, volume, title, path, published_at, \
                               access, unlocks_at) \
         SELECT DISTINCT ON (u.number::float8::numeric(10,4)) \
                u.id, $2, u.number::float8::numeric(10,4), u.volume, u.title, u.path, \
                u.published_at, u.access, u.unlocks_at \
           FROM UNNEST($1::uuid[], $3::float8[], $4::int[], $5::text[], $6::text[], \
                       $7::timestamptz[], $8::chapter_access[], $9::timestamptz[]) \
                WITH ORDINALITY AS u(id, number, volume, title, path, published_at, access, \
                                     unlocks_at, ord) \
          ORDER BY u.number::float8::numeric(10,4), u.ord DESC \
         ON CONFLICT (series_source_id, number) DO UPDATE \
            SET title = EXCLUDED.title, path = EXCLUDED.path, \
                published_at = COALESCE(EXCLUDED.published_at, chapters.published_at), \
                access = EXCLUDED.access, unlocks_at = EXCLUDED.unlocks_at \
         RETURNING number::float8 AS \"number!\", (xmax = 0) AS \"inserted!\"",
        &ids,
        source_id.as_uuid(),
        &numbers,
        &volumes as &[Option<i32>],
        &titles as _,
        &paths as _,
        &published as &[Option<OffsetDateTime>],
        &accesses as &[ChapterAccess],
        &unlocks as &[Option<OffsetDateTime>],
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
/// [`crate::DbError::Sqlx`] only; no chapters or unknown source is `Ok(None)`, never
/// `Ok(Some(0.0))` — a zero would be read as chapter 0 existing.
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

/// Distinct **whole** chapters a source has; part releases (`152.1`..`152.6`) collapse into
/// their whole chapter rather than each counting separately. Distinct from
/// `series_sources.chapter_count`, a raw scanned-row count.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only; no chapters or unknown source is `Ok(0)`. Saturates at
/// `i32::MAX` rather than erroring or wrapping.
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
/// [`crate::DbError::Sqlx`] only; unknown source or a not-yet-scanned stub is the same empty
/// `Vec`.
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

/// Distinct whole-chapter counts for a series, one row per provider — the batched counterpart
/// to [`count_full_chapters_across`].
///
/// # Errors
/// [`crate::DbError::Sqlx`] only. Inner join: a provider with no chapters has no row, not a row
/// of `0` — callers must treat a missing key as zero.
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

/// Distinct **whole** chapters across a *set* of sources (design §10 same-source merge) — the
/// union-aware counterpart to [`count_full_chapters`]; summing per-source counts would
/// double-count a chapter two sources share. Empty slice yields `0`.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only; saturates at `i32::MAX` like [`count_full_chapters`].
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

/// Chapters spanning a *set* of sources, de-duplicated (`DISTINCT ON (number)`,
/// earliest-discovered wins) and newest-first — the merge-aware counterpart to
/// [`list_chapters`]. Caller must ensure all sources share one provider; not checked here, and
/// mixed providers produce links resolved against the wrong `base_url`.
///
/// `viewer` is who is asking, and it decides whether the provider's paid early-access chapters
/// are in the list at all. They are omitted unless that reader has opted the provider in — a row
/// nobody can open is a link to a paywall, and listing it next to readable ones is what put a
/// locked chapter under the series screen's "next up" marker. `None` (an anonymous request) sees
/// only what is free, which is also what an anonymous visitor sees on the provider's own site.
///
/// A chapter whose stated unlock time has passed is free to everyone without a rescan, the same
/// rule the unread predicate applies; a locked one with no stated time stays hidden, because a
/// missing date is not a date in the past.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only; empty or unknown ids are the same empty `Vec`.
pub async fn list_chapters_across<'e, E: PgExecutor<'e>>(
    exec: E,
    source_ids: &[SeriesSourceId],
    viewer: Option<UserId>,
) -> DbResult<Vec<Chapter>> {
    let ids: Vec<Uuid> = source_ids.iter().map(|s| s.as_uuid()).collect();
    let rows = sqlx::query_as!(
        ChapterRow,
        "SELECT DISTINCT ON (c.number) c.id, c.series_source_id, c.number::float8 AS \"number!\", \
         c.volume, c.title, c.path, c.published_at, c.discovered_at \
         FROM chapters c JOIN series_sources ss ON ss.id = c.series_source_id \
         WHERE c.series_source_id = ANY($1) \
           AND (c.access = 'free' OR c.unlocks_at <= now() \
                OR EXISTS (SELECT 1 FROM user_provider_early_access e \
                            WHERE e.user_id = $2::uuid \
                              AND e.provider_id = ss.provider_id)) \
         ORDER BY c.number DESC, c.discovered_at ASC",
        &ids,
        viewer.map(UserId::as_uuid) as Option<Uuid>,
    )
    .fetch_all(exec)
    .await?;
    Ok(rows.into_iter().map(Chapter::from).collect())
}

/// The de-duplicated chapter figures a catalogue card shows.
pub struct SeriesChapterStats {
    /// Distinct **whole** chapters across every source, so a title carried by four providers
    /// counts its chapters once. Part releases collapse into their whole chapter, matching what
    /// the series screen counts.
    pub chapter_count: i64,
    /// The highest chapter number any source carries.
    pub latest_number: Option<f64>,
}

/// [`SeriesChapterStats`] for a set of series, in one statement.
///
/// Batched rather than folded into the browse projection as a correlated subquery: the browse
/// statements are the ones the plan audit budgets, and a `chapters` reach per candidate row is
/// charged against every row of `series` under a generic plan. Keyed on the page's ids, this
/// touches only the rows about to be rendered.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only. Inner join: a series whose sources hold no chapters is absent
/// from the map rather than present as zero — callers must treat a missing key as zero.
pub async fn chapter_stats_for_series<'e, E: PgExecutor<'e>>(
    exec: E,
    series_ids: &[SeriesId],
) -> DbResult<std::collections::HashMap<SeriesId, SeriesChapterStats>> {
    #[derive(FromRow)]
    struct Row {
        series_id: Uuid,
        chapters: i64,
        latest: Option<f64>,
    }
    let ids: Vec<Uuid> = series_ids.iter().map(|s| s.as_uuid()).collect();
    let rows = sqlx::query_as!(
        Row,
        "SELECT ss.series_id, \
                count(DISTINCT floor(c.number)) AS \"chapters!\", \
                max(c.number)::float8 AS \"latest?\" \
         FROM series_sources ss JOIN chapters c ON c.series_source_id = ss.id \
         WHERE ss.series_id = ANY($1) \
         GROUP BY ss.series_id",
        &ids,
    )
    .fetch_all(exec)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| {
            (
                SeriesId::from_uuid(r.series_id),
                SeriesChapterStats {
                    chapter_count: r.chapters,
                    latest_number: r.latest,
                },
            )
        })
        .collect())
}
