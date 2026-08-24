//! Chapters: idempotent upserts that report which rows were genuinely new, plus the counts
//! and listings the reading surfaces read back.
//!
//! # The storage form, and why the SQL here looks the way it does
//!
//! Migration 0055 stores the chapter number as `number_milli int` — the number scaled by
//! [`tankovault_domain::chapter_number::MILLI_SCALE`] — and the path relative to the source's own
//! path. Both are storage details that
//! stop at this module's boundary: [`Chapter`] carries an `f64` number and an expanded path, and
//! nothing above the repo layer knows otherwise.
//!
//! The translations, which every chapter query in the crate uses and none may spell differently:
//!
//! | Domain question | SQL |
//! |---|---|
//! | `floor(number)` | `number_milli / 10000` (integer division; exact only because the column is `CHECK (number_milli >= 0)`) |
//! | `number` is a part release | `number_milli % 10000 <> 0` |
//! | `floor(number) > w` | `number_milli >= (w + 1) * 10000`, computed in **bigint** |
//! | `number <= p` | `number_milli <= (p * 10000)::bigint` |
//! | the absolute path | `chapter_url_path(ss.source_path, c.path)` |
//!
//! The bigint on the third row is not decoration, though not for the obvious reason: an in-range
//! bound, `(200000 + 1) * 10000`, fits `int` comfortably. `w` comes from
//! `read_progress.last_read_whole_number`, which is still `numeric(10,4)` and was never
//! range-checked before the chapter ceiling existed — a row holding a date-shaped value from that
//! era yields a bound two orders of magnitude past `i32::MAX`, and that is an error raised on a
//! read path rather than a wrong answer. Postgres has an `int4 >= int8` operator in the default
//! btree opfamily, so widening the bound costs nothing: the comparison stays an **index cond**
//! rather than a filter, which is the whole point of the storage change and is verified by
//! `repo_query_plans`.

use crate::error::DbResult;
use sqlx::{FromRow, PgExecutor};
use tankovault_domain::chapter_number::{from_milli, to_milli};
use tankovault_domain::{Chapter, ChapterAccess, ProviderId, SeriesId, SeriesSourceId, UserId};
use time::OffsetDateTime;
use uuid::Uuid;

/// One chapter to upsert (from an adapter's `fetch_chapters`).
pub struct ChapterUpsert {
    /// Chapter number, fractional for a part release.
    pub number: f64,
    /// Title as the provider gives it, `None` when it publishes only a number.
    pub title: Option<String>,
    /// RELATIVE link to the chapter page, as the adapter emitted it — site-relative and whole.
    /// [`upsert_chapters`] compresses it against the source path; callers do not.
    pub path: String,
    /// When the provider says it went up, `None` when it publishes no date.
    pub published_at: Option<OffsetDateTime>,
    /// What the provider says about reading it: free, or behind a paywall.
    pub access: ChapterAccess,
    /// When an early-access chapter opens, where the provider states a date. `None` on a
    /// locked chapter means "no date published", which the read paths keep treating as locked.
    pub unlocks_at: Option<OffsetDateTime>,
}

#[derive(FromRow)]
struct ChapterRow {
    series_source_id: Uuid,
    number_milli: i32,
    title: Option<String>,
    path: String,
    published_at: Option<OffsetDateTime>,
    discovered_at: OffsetDateTime,
}

impl From<ChapterRow> for Chapter {
    fn from(r: ChapterRow) -> Self {
        Self {
            series_source_id: SeriesSourceId::from_uuid(r.series_source_id),
            number: from_milli(r.number_milli),
            title: r.title,
            path: r.path,
            published_at: r.published_at,
            discovered_at: r.discovered_at,
        }
    }
}

/// Upsert a whole chapter list in one statement, returning the numbers that were **new**.
///
/// One statement, not a per-chapter loop — the ingest transaction holds row locks on shared
/// `tags`/`authors` rows, so per-chapter round trips there stall other ingests. `DISTINCT ON`
/// avoids `ON CONFLICT DO UPDATE` aborting when a page lists one chapter number twice.
///
/// `source_path` is the source's own path, which the stored `path` is compressed against.
///
/// A chapter whose number is outside the storable range is **skipped**, not an error: the worker
/// already drops those (`drop_unstorable`), and a second one reaching here must not be able to
/// fail the whole per-source transaction — which is the failure the range check exists to prevent.
///
/// # The `WHERE` on the `DO UPDATE`, which is not optional
///
/// Without it, a *converged* rescan — one where the provider published nothing and every value is
/// byte-identical to what is stored — still writes a new version of every row, because an `UPDATE`
/// that assigns the same value is still an `UPDATE`. Measured on 1.2 M rows: the relation went
/// from 456 MB to **1046 MB after a single no-op rescan**, and to 1465 MB after three. With this
/// clause, three converged rescans leave it at 456 MB.
///
/// The `published_at` arm has to mirror the `COALESCE` above it. Written as a plain
/// `IS DISTINCT FROM`, a row whose stored `published_at` is set and whose incoming one is NULL
/// would compare as differing forever and be rewritten on every scan — the same bug, quieter.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only; one bad chapter fails the whole batch. An empty `Vec` means
/// no input, a fully-converged re-scan, or every row skipped — only `Err` means a write failed.
pub async fn upsert_chapters<'e, E: PgExecutor<'e>>(
    exec: E,
    source_id: SeriesSourceId,
    source_path: &str,
    chapters: &[ChapterUpsert],
) -> DbResult<Vec<f64>> {
    if chapters.is_empty() {
        return Ok(Vec::new());
    }

    let storable: Vec<(&ChapterUpsert, i32)> = chapters
        .iter()
        .filter_map(|c| to_milli(c.number).map(|m| (c, m)))
        .collect();
    if storable.is_empty() {
        return Ok(Vec::new());
    }

    let numbers: Vec<i32> = storable.iter().map(|(_, m)| *m).collect();
    // Borrowed where the value is already owned by the caller; the compressed paths have to be
    // built, so they are the one allocation per row this function makes.
    let titles: Vec<Option<&str>> = storable.iter().map(|(c, _)| c.title.as_deref()).collect();
    let paths: Vec<String> = storable
        .iter()
        .map(|(c, _)| tankovault_domain::compress_chapter_path(source_path, &c.path))
        .collect();
    let published: Vec<Option<OffsetDateTime>> =
        storable.iter().map(|(c, _)| c.published_at).collect();
    let accesses: Vec<ChapterAccess> = storable.iter().map(|(c, _)| c.access).collect();
    let unlocks: Vec<Option<OffsetDateTime>> = storable.iter().map(|(c, _)| c.unlocks_at).collect();

    let rows = sqlx::query!(
        // `access`/`unlocks_at` are overwritten, not coalesced: they are the provider's current
        // verdict, and the whole point is that a chapter which has since unlocked stops being
        // reported as locked. Coalescing would freeze the first answer forever.
        //
        // `DISTINCT ON (u.number_milli)` with `u.ord DESC` behind it makes the last listing of a
        // repeated number win. It no longer needs a cast: the dedup key and the unique index are
        // now the same `int`, where under `numeric(10,4)` they were two different types and two
        // `float8` values differing past the fourth decimal deduped as distinct but collided in
        // the constraint — aborting the batch with "ON CONFLICT DO UPDATE command cannot affect
        // row a second time".
        "INSERT INTO chapters (series_source_id, number_milli, title, path, published_at, \
                               access, unlocks_at) \
         SELECT DISTINCT ON (u.number_milli) \
                $1, u.number_milli, u.title, u.path, u.published_at, u.access, u.unlocks_at \
           FROM UNNEST($2::int[], $3::text[], $4::text[], $5::timestamptz[], \
                       $6::chapter_access[], $7::timestamptz[]) \
                WITH ORDINALITY AS u(number_milli, title, path, published_at, access, \
                                     unlocks_at, ord) \
          ORDER BY u.number_milli, u.ord DESC \
         ON CONFLICT (series_source_id, number_milli) DO UPDATE \
            SET title = EXCLUDED.title, path = EXCLUDED.path, \
                published_at = COALESCE(EXCLUDED.published_at, chapters.published_at), \
                access = EXCLUDED.access, unlocks_at = EXCLUDED.unlocks_at \
          WHERE chapters.title      IS DISTINCT FROM EXCLUDED.title \
             OR chapters.path       IS DISTINCT FROM EXCLUDED.path \
             OR chapters.access     IS DISTINCT FROM EXCLUDED.access \
             OR chapters.unlocks_at IS DISTINCT FROM EXCLUDED.unlocks_at \
             OR (EXCLUDED.published_at IS NOT NULL \
                 AND chapters.published_at IS DISTINCT FROM EXCLUDED.published_at) \
         RETURNING number_milli AS \"number_milli!\", (xmax = 0) AS \"inserted!\"",
        source_id.as_uuid(),
        &numbers,
        &titles as _,
        &paths,
        &published as &[Option<OffsetDateTime>],
        &accesses as &[ChapterAccess],
        &unlocks as &[Option<OffsetDateTime>],
    )
    .fetch_all(exec)
    .await?;

    Ok(rows
        .into_iter()
        .filter(|r| r.inserted)
        .map(|r| from_milli(r.number_milli))
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
        "SELECT MAX(number_milli) AS \"max?\" FROM chapters WHERE series_source_id = $1",
        source_id.as_uuid(),
    )
    .fetch_one(exec)
    .await?;
    Ok(max.map(from_milli))
}

/// Distinct whole chapters a source has, with part releases folded into their whole.
///
/// `152.1` through `152.6` count once, as chapter 152. Not the same figure as
/// `series_sources.chapter_count`, which is a raw scanned-row count.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only; no chapters or unknown source is `Ok(0)`. Saturates at
/// `i32::MAX` rather than erroring or wrapping.
pub async fn count_full_chapters<'e, E: PgExecutor<'e>>(
    exec: E,
    source_id: SeriesSourceId,
) -> DbResult<i32> {
    let count = sqlx::query_scalar!(
        "SELECT count(DISTINCT number_milli / 10000) AS \"count!\" FROM chapters \
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
        "SELECT c.series_source_id, c.number_milli, c.title, \
                chapter_url_path(ss.source_path, c.path) AS \"path!\", \
                c.published_at, c.discovered_at \
         FROM chapters c JOIN series_sources ss ON ss.id = c.series_source_id \
         WHERE c.series_source_id = $1 \
         ORDER BY c.number_milli DESC",
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
        "SELECT ss.provider_id, count(DISTINCT c.number_milli / 10000) AS \"count!\" \
           FROM series_sources ss JOIN chapters c ON c.series_source_id = ss.id \
          WHERE ss.series_id = $1 \
          GROUP BY ss.provider_id",
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

/// Distinct whole chapters across a set of sources, counted once each (design §10).
///
/// The union-aware counterpart to [`count_full_chapters`]: summing per-source counts would
/// double every chapter two sources both carry. An empty slice yields `0`.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only; saturates at `i32::MAX` like [`count_full_chapters`].
pub async fn count_full_chapters_across<'e, E: PgExecutor<'e>>(
    exec: E,
    source_ids: &[SeriesSourceId],
) -> DbResult<i32> {
    let ids: Vec<Uuid> = source_ids.iter().map(|s| s.as_uuid()).collect();
    let count = sqlx::query_scalar!(
        "SELECT count(DISTINCT number_milli / 10000) AS \"count!\" FROM chapters \
         WHERE series_source_id = ANY($1)",
        &ids,
    )
    .fetch_one(exec)
    .await?;
    Ok(i32::try_from(count).unwrap_or(i32::MAX))
}

/// Chapters across a set of sources, de-duplicated by number and newest first.
///
/// The merge-aware counterpart to [`list_chapters`]. Where two sources carry one number, the
/// earliest-discovered row wins. Every source must belong to one provider: nothing checks it,
/// and a mixed set produces links resolved against the wrong `base_url`.
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
        "SELECT DISTINCT ON (c.number_milli) c.series_source_id, c.number_milli, c.title, \
                chapter_url_path(ss.source_path, c.path) AS \"path!\", \
                c.published_at, c.discovered_at \
         FROM chapters c JOIN series_sources ss ON ss.id = c.series_source_id \
         WHERE c.series_source_id = ANY($1) \
           AND (c.access = 'free' OR c.unlocks_at <= now() \
                OR ss.provider_id = ANY(ARRAY( \
                     SELECT e.provider_id FROM user_provider_early_access e \
                     WHERE e.user_id = $2::uuid))) \
         ORDER BY c.number_milli DESC, c.discovered_at ASC",
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
        latest: Option<i32>,
    }
    let ids: Vec<Uuid> = series_ids.iter().map(|s| s.as_uuid()).collect();
    let rows = sqlx::query_as!(
        Row,
        "SELECT ss.series_id, \
                count(DISTINCT c.number_milli / 10000) AS \"chapters!\", \
                max(c.number_milli) AS \"latest?\" \
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
                    latest_number: r.latest.map(from_milli),
                },
            )
        })
        .collect())
}

#[cfg(test)]
mod tests {
    /// The scale the SQL in this module hard-codes as `10000` has to be the domain's, or every
    /// `floor` translation silently means something else.
    #[test]
    fn the_sql_scale_matches_the_domain_scale() {
        assert!((tankovault_domain::chapter_number::MILLI_SCALE - 10_000.0).abs() < f64::EPSILON);
    }
}
