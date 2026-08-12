//! Assembling one page of watchlist cards, and the single-card lookup beside it.

use std::collections::HashMap;

use crate::error::DbResult;
use sqlx::{FromRow, PgPool};
use tankovault_domain::{ProviderState, SeriesId, SeriesSourceId, UserId, WatchStatus};
use time::OffsetDateTime;
use uuid::Uuid;

use super::query::{
    NextUnread, WatchlistCard, WatchlistCounts, WatchlistCursor, WatchlistFilter, WatchlistPage,
    WatchlistSource, search_pattern,
};
use super::summary::{fetch_counts, fetch_groups};

/// The row `query_as!` fills for one page of the list.
#[derive(FromRow)]
struct CardRow {
    series_id: Uuid,
    series_title: String,
    cover_url: Option<String>,
    status: WatchStatus,
    notify: bool,
    added_at: OffsetDateTime,
    last_read_number: Option<f64>,
    unread: i64,
    read_count: i64,
    total_chapters: i64,
    latest_chapter_number: Option<f64>,
    latest_chapter_at: Option<OffsetDateTime>,
    next_unread_number: Option<f64>,
    next_unread_title: Option<String>,
    next_unread_at: Option<OffsetDateTime>,
    preferred_source_name: Option<String>,
    source_count: i64,
    source_degraded: bool,
    sync_excluded: bool,
    pinned_source_id: Option<Uuid>,
    /// The key the statement ordered by, carried out so [`WatchlistCursor`] cannot recompute
    /// it differently.
    sort_num: Option<f64>,
    sort_text: Option<String>,
}

impl CardRow {
    /// The cursor that resumes immediately after this row.
    fn cursor(&self) -> WatchlistCursor {
        WatchlistCursor {
            num: self.sort_num,
            text: self.sort_text.clone(),
            series_id: SeriesId::from_uuid(self.series_id),
        }
    }
}

impl From<CardRow> for WatchlistCard {
    fn from(r: CardRow) -> Self {
        Self {
            series_id: SeriesId::from_uuid(r.series_id),
            series_title: r.series_title,
            cover_url: r.cover_url,
            status: r.status,
            notify: r.notify,
            added_at: r.added_at,
            last_read_number: r.last_read_number,
            unread: r.unread,
            read_count: r.read_count,
            // The number and the timestamp come from the same row, so either both are present
            // or the reader is caught up; a number without an instant cannot occur.
            next_unread: r
                .next_unread_number
                .zip(r.next_unread_at)
                .map(|(number, released_at)| NextUnread {
                    number,
                    title: r.next_unread_title,
                    released_at,
                }),
            total_chapters: r.total_chapters,
            latest_chapter_number: r.latest_chapter_number,
            latest_chapter_at: r.latest_chapter_at,
            preferred_source_name: r.preferred_source_name,
            source_count: r.source_count,
            source_degraded: r.source_degraded,
            sync_excluded: r.sync_excluded,
            pinned_source_id: r.pinned_source_id.map(SeriesSourceId::from_uuid),
            sources: Vec::new(),
        }
    }
}

/// List a user's watchlist, filtered, sorted and paginated in SQL, together with the tab
/// counts and the group-header aggregates. `unread` counts distinct whole chapters
/// (`floor(number)`) so part releases don't inflate it.
///
/// The unread filter is the fourth copy of the predicate documented on
/// [`dashboard`](crate::repo::tracking::dashboard); it must stay the negation of
/// [`ReadProgress::covers`](crate::repo::tracking::ReadProgress::covers), or this badge disagrees with the feed
/// that links to the same chapters. `repo_tracking`'s `unread_predicate_agrees_everywhere` test
/// is what holds the four together.
///
/// # Why three statements
///
/// The page, the per-status counts and the group aggregates answer three different questions
/// over three different predicate sets — the counts deliberately drop the `status` filter, the
/// groups keep it — so no single statement produces all three without a `GROUPING SETS`
/// construction that would be harder to read than the three it replaced. They are issued
/// concurrently on separate pool connections, so the trio costs one round trip, exactly as
/// [`list_series_filtered`](crate::repo::catalog::list_series_filtered) does.
///
/// `total` is **derived** from the group aggregates rather than counted a fourth time: the
/// grouping query carries the identical predicate list, every row lands in exactly one band,
/// and a separate `count(*)` could only ever disagree with the sum by racing it.
///
/// Takes `&PgPool` rather than a generic executor precisely so the three can overlap.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only — no other variant is reachable. A user tracking nothing, and
/// a filter matching nothing, are both an empty page with zeroed counts rather than
/// [`crate::DbError::NotFound`]. A tracked series with no progress row comes back through the
/// `LEFT JOIN` with `last_read_number: None` and its full chapter count as `unread`, which is a
/// valid row rather than a missing one; a tracked series with no chapters at all comes back
/// with `total_chapters: 0` and `latest_chapter_at: None`, and bands as
/// [`ReleaseBucket::Earlier`](super::ReleaseBucket::Earlier).
pub async fn watchlist_page(
    pool: &PgPool,
    user_id: UserId,
    filter: &WatchlistFilter,
) -> DbResult<WatchlistPage> {
    let query = filter
        .query
        .as_deref()
        .map(str::trim)
        .filter(|q| !q.is_empty())
        .map(search_pattern);
    let query = query.as_deref();

    let (rows, status_counts, groups) = tokio::try_join!(
        fetch_page(pool, user_id, filter, query),
        fetch_counts(pool, user_id, filter, query),
        fetch_groups(pool, user_id, filter, query),
    )?;

    let mut counts = WatchlistCounts::default();
    for (status, n, degraded) in status_counts {
        counts.add(status, n, degraded);
    }
    let total = groups.iter().map(|g| g.title_count).sum();

    let next_cursor = (i64::try_from(rows.len()).unwrap_or(i64::MAX) >= filter.limit)
        .then(|| rows.last().map(CardRow::cursor))
        .flatten();

    Ok(WatchlistPage {
        items: attach_sources(pool, rows).await?,
        total,
        counts,
        groups,
        next_cursor,
    })
}

/// Turn page rows into cards with their [`WatchlistCard::sources`] filled in.
///
/// The one place a `WatchlistCard` is built for a caller, so no path can hand out a card whose
/// empty `sources` means "not loaded" rather than "no sources".
async fn attach_sources(pool: &PgPool, rows: Vec<CardRow>) -> DbResult<Vec<WatchlistCard>> {
    let ids: Vec<Uuid> = rows.iter().map(|r| r.series_id).collect();
    let mut by_series = fetch_sources(pool, &ids).await?;
    Ok(rows
        .into_iter()
        .map(|row| {
            let mut card = WatchlistCard::from(row);
            card.sources = by_series.remove(&card.series_id).unwrap_or_default();
            card
        })
        .collect())
}

/// One page of matching rows, in the requested order.
///
/// `pattern` is already wrapped and escaped by [`search_pattern`]; binding a raw term instead
/// would let a typed `%` or `_` act as a wildcard.
///
/// # Why the expensive columns are computed after the `LIMIT`
///
/// `total_chapters` and `read_count` count distinct whole chapters across *every* chapter the
/// series has on every source; there is no predicate that narrows them, so each one reads the
/// series' whole chapter list. Computed alongside the filters they run for every tracked
/// series — 851 entries × ~375 chapters, measured at 570 ms warm and 4.7 s cold. Nothing reads
/// them except the returned rows, so the statement is two stages: the `page` CTE carries only
/// what the filters, the sort and the cursor need, and the outer query joins the per-row
/// aggregates onto the ≤`limit` rows that survived. Same rows, same order, 95 ms warm.
///
/// What that leaves in the CTE is deliberately cheap: `unread` takes the unread predicate in
/// its `WHERE`, so `floor(number) >` becomes an **index cond** on
/// `chapters_source_floor_num_idx` and the scan stays index-only over the unread tail;
/// `latest_chapter_at` is a scalar `max()` per source, the form that lets Postgres apply the
/// MIN/MAX index optimisation on `chapters_source_disc_idx`. The `progress` sort is the one
/// key that genuinely needs `total_chapters` before the `LIMIT`, so it is a subquery inside the
/// sort-key `CASE` — an untaken `CASE` arm never executes its subplan, so the other four sorts
/// do not pay for it.
///
/// The outer `ORDER BY` repeats the CTE's: a CTE's row order is not contractual, and the two
/// lateral joins below are free to reorder it.
///
/// # Why the sort key is computed in a subquery
///
/// The order is chosen by two bound tokens, so `ORDER BY` needs an ascending and a descending
/// arm for each. Written flat, that means repeating the five-branch key expression twice, and
/// the two copies drifting is a list silently ordered by the wrong thing under one direction.
/// Naming the key once in the subquery and ordering the outer query by it costs a nesting
/// level Postgres flattens anyway.
///
/// The final `series_id` tiebreaker is not decoration: without it rows sharing a leading key —
/// and with `unread` over 600 entries there are hundreds of ties — have no defined order, so
/// two adjacent `OFFSET` pages can repeat one row and skip another.
#[expect(
    clippy::too_many_lines,
    reason = "one `query_as!` invocation: the length is the SQL literal and its bindings, and \
              splitting a statement across helpers is exactly the drift the `--wl-cols` comment \
              and the sort-key subquery both exist to prevent"
)]
async fn fetch_page(
    pool: &PgPool,
    user_id: UserId,
    filter: &WatchlistFilter,
    pattern: Option<&str>,
) -> DbResult<Vec<CardRow>> {
    let cursor = filter.cursor.as_ref();
    let rows = sqlx::query_as!(
        CardRow,
        "WITH page AS ( \
           SELECT q.* FROM ( \
             SELECT w.series_id, s.canonical_title AS series_title, s.cover_url, w.status, \
                    w.notify, w.added_at, w.sync_excluded, w.pinned_source_id, \
                    rp.last_read_whole_number, rp.last_read_part_number, \
                    unr.unread, la.latest_chapter_at, \
                    src.preferred_source_name, src.source_count, src.source_degraded, \
                    CASE $7 \
                      WHEN 'released' THEN extract(epoch FROM la.latest_chapter_at)::float8 \
                      WHEN 'unread'   THEN unr.unread::float8 \
                      WHEN 'added'    THEN extract(epoch FROM w.added_at)::float8 \
                      WHEN 'progress' THEN ( \
                        SELECT CASE WHEN count(DISTINCT floor(c.number)) > 0 \
                                    THEN COALESCE(rp.last_read_whole_number, 0)::float8 \
                                         / count(DISTINCT floor(c.number)) END \
                        FROM series_sources ss JOIN chapters c ON c.series_source_id = ss.id \
                        WHERE ss.series_id = w.series_id \
                          AND (c.access = 'free' OR c.unlocks_at <= now() \
                               OR EXISTS (SELECT 1 FROM user_provider_early_access e \
                                           WHERE e.user_id = w.user_id \
                                             AND e.provider_id = ss.provider_id))) \
                    END AS sort_num, \
                    CASE WHEN $7 = 'title' THEN s.canonical_title END AS sort_text \
             FROM watchlist_entries w \
             JOIN series s ON s.id = w.series_id \
             LEFT JOIN read_progress rp ON rp.user_id = w.user_id AND rp.series_id = w.series_id \
             CROSS JOIN LATERAL ( \
               SELECT COALESCE(count(DISTINCT floor(c.number)), 0) AS unread \
               FROM series_sources ss JOIN chapters c ON c.series_source_id = ss.id \
               WHERE ss.series_id = w.series_id \
                 AND floor(c.number) > COALESCE(rp.last_read_whole_number, 0) \
                 AND NOT (c.number <> floor(c.number) \
                          AND rp.last_read_part_number IS NOT NULL \
                          AND c.number <= rp.last_read_part_number) \
                 AND (c.access = 'free' OR c.unlocks_at <= now() \
                      OR EXISTS (SELECT 1 FROM user_provider_early_access e \
                                  WHERE e.user_id = w.user_id \
                                    AND e.provider_id = ss.provider_id)) \
             ) unr \
             CROSS JOIN LATERAL ( \
               SELECT max((SELECT max(c.discovered_at) FROM chapters c \
                           WHERE c.series_source_id = ss.id \
                             AND (c.access = 'free' OR c.unlocks_at <= now() \
                                  OR EXISTS (SELECT 1 FROM user_provider_early_access e \
                                              WHERE e.user_id = w.user_id \
                                                AND e.provider_id = ss.provider_id)))) \
                        AS latest_chapter_at \
               FROM series_sources ss WHERE ss.series_id = w.series_id \
             ) la \
             CROSS JOIN LATERAL ( \
               SELECT count(DISTINCT ss.provider_id) AS source_count, \
                      (array_agg(p.name ORDER BY ss.chapter_count DESC, \
                                                  ss.last_scanned_at DESC NULLS LAST, \
                                                  p.slug))[1] AS preferred_source_name, \
                      COALESCE((array_agg(ss.state <> 'active' OR p.state <> 'active' \
                                          ORDER BY ss.chapter_count DESC, \
                                                   ss.last_scanned_at DESC NULLS LAST, \
                                                   p.slug))[1], false) AS source_degraded \
               FROM series_sources ss JOIN providers p ON p.id = ss.provider_id \
               WHERE ss.series_id = w.series_id \
             ) src \
             WHERE w.user_id = $1 \
               AND ($2::watch_status IS NULL OR w.status = $2) \
               AND ($3::text IS NULL \
                    OR s.canonical_title ILIKE $3 \
                    OR EXISTS (SELECT 1 FROM series_titles st \
                               WHERE st.series_id = w.series_id \
                                 AND st.title ILIKE $3) \
                    OR EXISTS (SELECT 1 FROM series_tags stg JOIN tags t ON t.id = stg.tag_id \
                               WHERE stg.series_id = w.series_id \
                                 AND t.name ILIKE $3) \
                    OR EXISTS (SELECT 1 FROM series_authors sa \
                               JOIN authors a ON a.id = sa.author_id \
                               WHERE sa.series_id = w.series_id \
                                 AND a.name ILIKE $3)) \
               AND (NOT $4::boolean OR unr.unread > 0) \
               AND ($5::timestamptz IS NULL OR la.latest_chapter_at >= $5) \
               AND (NOT $6::boolean OR src.source_degraded) \
               AND ($11::uuid IS NULL OR w.series_id = $11) \
           ) q \
           WHERE NOT $12::boolean \
              OR CASE WHEN $7 = 'title' THEN \
                   (q.sort_text IS NULL AND NOT $15::boolean) \
                   OR ($15 AND q.sort_text IS NULL AND q.series_id > $16::uuid) \
                   OR (NOT $15 AND q.sort_text IS NOT NULL AND ( \
                          ($8 = 'desc' AND q.sort_text < $14::text) \
                       OR ($8 = 'asc'  AND q.sort_text > $14::text) \
                       OR (q.sort_text = $14 AND q.series_id > $16::uuid))) \
                 ELSE \
                   (q.sort_num IS NULL AND NOT $15::boolean) \
                   OR ($15 AND q.sort_num IS NULL AND q.series_id > $16::uuid) \
                   OR (NOT $15 AND q.sort_num IS NOT NULL AND ( \
                          ($8 = 'desc' AND q.sort_num < $13::float8) \
                       OR ($8 = 'asc'  AND q.sort_num > $13::float8) \
                       OR (q.sort_num = $13 AND q.series_id > $16::uuid))) \
                 END \
           ORDER BY CASE WHEN $8 = 'asc'  THEN q.sort_num  END ASC  NULLS LAST, \
                    CASE WHEN $8 = 'desc' THEN q.sort_num  END DESC NULLS LAST, \
                    CASE WHEN $8 = 'asc'  THEN q.sort_text END ASC  NULLS LAST, \
                    CASE WHEN $8 = 'desc' THEN q.sort_text END DESC NULLS LAST, \
                    q.series_id \
           LIMIT $9 OFFSET $10 \
         ) \
         SELECT p.series_id AS \"series_id!\", p.series_title AS \"series_title!\", p.cover_url, \
                p.status AS \"status!: WatchStatus\", p.notify AS \"notify!\", \
                p.added_at AS \"added_at!\", p.sync_excluded AS \"sync_excluded!\", \
                p.last_read_whole_number::float8 AS last_read_number, \
                p.unread AS \"unread!\", ch.read_count AS \"read_count!\", \
                ch.total_chapters AS \"total_chapters!\", ch.latest_chapter_number, \
                p.latest_chapter_at, \
                nu.number AS \"next_unread_number?\", \
                nu.title AS \"next_unread_title?\", \
                nu.discovered_at AS \"next_unread_at?\", p.preferred_source_name, \
                p.source_count AS \"source_count!\", p.source_degraded AS \"source_degraded!\", \
                p.pinned_source_id, p.sort_num, p.sort_text \
         FROM page p \
         CROSS JOIN LATERAL ( \
           SELECT COALESCE(count(DISTINCT floor(c.number)), 0) AS total_chapters, \
                  COALESCE(count(DISTINCT floor(c.number)) FILTER ( \
                    WHERE floor(c.number) <= COALESCE(p.last_read_whole_number, 0) \
                  ), 0) AS read_count, \
                  max(c.number)::float8 AS latest_chapter_number \
           FROM series_sources ss JOIN chapters c ON c.series_source_id = ss.id \
           WHERE ss.series_id = p.series_id \
             AND (c.access = 'free' OR c.unlocks_at <= now() \
                  OR EXISTS (SELECT 1 FROM user_provider_early_access e \
                              WHERE e.user_id = $1 \
                                AND e.provider_id = ss.provider_id)) \
         ) ch \
         LEFT JOIN LATERAL ( \
           SELECT c.number::float8 AS number, c.title, c.discovered_at \
           FROM series_sources ss JOIN chapters c ON c.series_source_id = ss.id \
           WHERE ss.series_id = p.series_id \
             AND floor(c.number) > COALESCE(p.last_read_whole_number, 0) \
             AND NOT (c.number <> floor(c.number) \
                      AND p.last_read_part_number IS NOT NULL \
                      AND c.number <= p.last_read_part_number) \
             AND (c.access = 'free' OR c.unlocks_at <= now() \
                  OR EXISTS (SELECT 1 FROM user_provider_early_access e \
                              WHERE e.user_id = $1 \
                                AND e.provider_id = ss.provider_id)) \
           ORDER BY c.number, c.discovered_at, c.id \
           LIMIT 1 \
         ) nu ON true \
         ORDER BY CASE WHEN $8 = 'asc'  THEN p.sort_num  END ASC  NULLS LAST, \
                  CASE WHEN $8 = 'desc' THEN p.sort_num  END DESC NULLS LAST, \
                  CASE WHEN $8 = 'asc'  THEN p.sort_text END ASC  NULLS LAST, \
                  CASE WHEN $8 = 'desc' THEN p.sort_text END DESC NULLS LAST, \
                  p.series_id",
        user_id.as_uuid(),
        filter.status as Option<WatchStatus>,
        pattern,
        filter.unread_only,
        filter.released_since,
        filter.source_issues,
        filter.sort.as_token(),
        filter.order.as_token(),
        filter.limit,
        // A cursor replaces the offset rather than adding to it: seeking past the row the
        // caller named and *then* skipping N more would drop rows nobody asked to skip.
        if cursor.is_some() { 0 } else { filter.offset },
        filter.series_id.map(SeriesId::as_uuid),
        cursor.is_some(),
        cursor.and_then(|c| c.num),
        cursor.and_then(|c| c.text.as_deref()),
        cursor.is_some_and(WatchlistCursor::key_is_null),
        cursor.map_or_else(uuid::Uuid::nil, |c| c.series_id.as_uuid()),
    )
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// Every provider carrying each of `series_ids`, preferred first — **one row per provider**.
///
/// A second statement keyed on the page's ids rather than an aggregate folded into
/// [`fetch_page`]: the ledger only renders this column above 1500px, and four more `array_agg`
/// columns per row are paid on every page whether or not anything reads them. Empty in, empty
/// out — no statement is issued for an empty page.
///
/// The `preferred` flag repeats [`fetch_page`]'s ranking (`chapter_count`, then the most recent
/// scan, then the slug) because the two must name the same source; a `Sources` column whose
/// tinted tile disagrees with the row's own submeta is worse than an untinted one.
///
/// # Why the `DISTINCT ON`
///
/// `series_sources` is unique on `(provider_id, source_path)`, not on `(series_id,
/// provider_id)`: one provider can legitimately carry the same series under several paths, and
/// a mis-matched scan can attach hundreds. Emitting a row each made the column repeat one
/// carrier's monogram and overflow into a `+n` counting paths rather than providers — and it
/// disagreed with `source_count`, which has always been `count(DISTINCT provider_id)`. The
/// per-provider survivor is picked by the same key the ranking uses, so it is the row
/// `preferred_source_name` would have named.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only — no other variant is reachable. A series with no sources is
/// simply absent from the map.
async fn fetch_sources(
    pool: &PgPool,
    series_ids: &[Uuid],
) -> DbResult<HashMap<SeriesId, Vec<WatchlistSource>>> {
    #[derive(FromRow)]
    struct Row {
        series_id: Uuid,
        code: String,
        name: String,
        state: ProviderState,
        preferred: bool,
    }

    if series_ids.is_empty() {
        return Ok(HashMap::new());
    }
    let rows = sqlx::query_as!(
        Row,
        "SELECT s.series_id AS \"series_id!\", s.code AS \"code!\", s.name AS \"name!\", \
                s.state AS \"state!: ProviderState\", \
                (row_number() OVER (PARTITION BY s.series_id \
                                    ORDER BY s.chapter_count DESC, \
                                             s.last_scanned_at DESC NULLS LAST, \
                                             s.code) = 1) AS \"preferred!\" \
         FROM ( \
           SELECT DISTINCT ON (ss.series_id, ss.provider_id) \
                  ss.series_id, p.slug AS code, p.name, \
                  CASE WHEN p.state <> 'active' THEN p.state ELSE ss.state END AS state, \
                  ss.chapter_count, ss.last_scanned_at \
           FROM series_sources ss JOIN providers p ON p.id = ss.provider_id \
           WHERE ss.series_id = ANY($1) \
           ORDER BY ss.series_id, ss.provider_id, ss.chapter_count DESC, \
                    ss.last_scanned_at DESC NULLS LAST, ss.id \
         ) s \
         ORDER BY s.series_id, s.chapter_count DESC, s.last_scanned_at DESC NULLS LAST, s.code",
        series_ids,
    )
    .fetch_all(pool)
    .await?;

    let mut out: HashMap<SeriesId, Vec<WatchlistSource>> = HashMap::new();
    for row in rows {
        out.entry(SeriesId::from_uuid(row.series_id))
            .or_default()
            .push(WatchlistSource {
                code: row.code,
                name: row.name,
                state: row.state,
                preferred: row.preferred,
            });
    }
    Ok(out)
}

/// One series' watchlist row, enriched exactly as the list's rows are — or `None` when the
/// user does not track it.
///
/// The Series page needs the same card the list renders (status, notify, progress, sync
/// exclusion). Fetching the whole watchlist to find one row breaks once the list paginates:
/// past the first page the entry simply is not in the response, so the page would render
/// "not tracked" for a series the user tracks.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only — no other variant is reachable. An untracked series is
/// `Ok(None)` rather than [`crate::DbError::NotFound`]: "do you track this" is a question with
/// a negative answer, not a missing resource.
pub async fn watchlist_card(
    pool: &PgPool,
    user_id: UserId,
    series_id: SeriesId,
) -> DbResult<Option<WatchlistCard>> {
    let filter = WatchlistFilter {
        series_id: Some(series_id),
        limit: 1,
        ..WatchlistFilter::default()
    };
    let rows = fetch_page(pool, user_id, &filter, None).await?;
    Ok(attach_sources(pool, rows).await?.into_iter().next())
}
