//! Catalog write + read path: canonical series, provider sources, and chapters.
//!
//! Every write is an idempotent `INSERT ... ON CONFLICT` so re-running a task under
//! at-least-once delivery is safe (design Appendix A §4). Chapter upserts report which
//! rows were genuinely new — via the `xmax = 0` idiom — so the worker can emit
//! `chapter.discovered` only for real discoveries.

use crate::error::{DbError, DbResult};
use sqlx::{FromRow, PgExecutor};
use tankovault_domain::{
    Chapter, ChapterId, ContentType, ProviderId, ProviderState, Series, SeriesId, SeriesSource,
    SeriesSourceId, SeriesStatus, normalize_title,
};
use time::OffsetDateTime;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Series
// ---------------------------------------------------------------------------

/// Canonical-series metadata to upsert (from an adapter's `fetch_series`).
pub struct SeriesUpsert {
    pub canonical_title: String,
    pub normalized_title: String,
    pub description: Option<String>,
    pub cover_url: Option<String>,
    pub content_type: ContentType,
    pub status: SeriesStatus,
    pub release_year: Option<i32>,
}

#[derive(FromRow)]
struct SeriesRow {
    id: Uuid,
    canonical_title: String,
    normalized_title: String,
    description: Option<String>,
    cover_url: Option<String>,
    content_type: ContentType,
    status: SeriesStatus,
    release_year: Option<i32>,
    created_at: OffsetDateTime,
    updated_at: OffsetDateTime,
}

impl TryFrom<SeriesRow> for Series {
    type Error = DbError;
    fn try_from(r: SeriesRow) -> Result<Self, Self::Error> {
        Ok(Self {
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
        })
    }
}

/// Resolve the canonical series for a scanned source using the canonicalisation pipeline
/// (design §10): trigram candidate lookup + [`tankovault_matcher`] scoring.
///
/// - **High confidence** → attach the source to the existing series.
/// - **Ambiguous band** → create a new series *and* record a `merge_candidate` for
///   operator review (one-click merge/split in the console).
/// - **Low/no confidence** → create a new canonical series.
///
/// Runs inside the ingest transaction so lookup + create are atomic for a single worker.
/// Concurrent first-creation of the same title across providers can still produce two
/// series; that is the case the ambiguous/merge queue and re-scan Attach path converge.
pub async fn resolve_canonical_series(
    conn: &mut sqlx::PgConnection,
    meta: &SeriesUpsert,
) -> DbResult<SeriesId> {
    let candidates = crate::repo::matching::find_candidates(&mut *conn, &meta.normalized_title, 10)
        .await?
        .into_iter()
        .map(|c| tankovault_matcher::Candidate {
            series_id: c.series_id,
            normalized_title: c.normalized_title,
            similarity: c.similarity,
            content_type: c.content_type,
            release_year: c.release_year,
            tags: c.tags,
            authors: c.authors,
        })
        .collect::<Vec<_>>();

    // No tag/author signal on the query side here: a scanned source's own tags/authors
    // aren't threaded into `SeriesUpsert` (they're written separately in `ingest_series`).
    // The bonus simply never fires — unchanged behaviour from before this field existed.
    let query = tankovault_matcher::Query {
        normalized_title: meta.normalized_title.clone(),
        content_type: meta.content_type,
        release_year: meta.release_year,
        tags: Vec::new(),
        authors: Vec::new(),
    };

    match tankovault_matcher::decide(
        &query,
        &candidates,
        tankovault_matcher::Thresholds::default(),
    ) {
        tankovault_matcher::Decision::Attach(id) => Ok(id),
        tankovault_matcher::Decision::Ambiguous { candidate, score } => {
            let id = create_series(conn, meta).await?;
            crate::repo::matching::record_merge_candidate(
                &mut *conn,
                id,
                candidate,
                score,
                "ambiguous title match",
            )
            .await?;
            Ok(id)
        }
        tankovault_matcher::Decision::Create => create_series(conn, meta).await,
    }
}

/// Insert a fresh canonical series from scanned metadata, returning its new id.
async fn create_series(conn: &mut sqlx::PgConnection, meta: &SeriesUpsert) -> DbResult<SeriesId> {
    let id = SeriesId::new();
    sqlx::query!(
        "INSERT INTO series (id, canonical_title, normalized_title, description, \
         cover_url, content_type, status, release_year) \
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8)",
        id.as_uuid(),
        &meta.canonical_title,
        &meta.normalized_title,
        meta.description.as_deref(),
        meta.cover_url.as_deref(),
        meta.content_type as ContentType,
        meta.status as SeriesStatus,
        meta.release_year,
    )
    .execute(&mut *conn)
    .await?;
    Ok(id)
}

/// Refresh metadata on an existing series, coalescing new non-null values over old.
pub async fn update_series_meta<'e, E: PgExecutor<'e>>(
    exec: E,
    id: SeriesId,
    meta: &SeriesUpsert,
) -> DbResult<()> {
    sqlx::query!(
        "UPDATE series SET \
            canonical_title = $2, \
            description = COALESCE($3, description), \
            cover_url = COALESCE($4, cover_url), \
            content_type = $5, \
            status = $6, \
            release_year = COALESCE($7, release_year), \
            updated_at = now() \
         WHERE id = $1",
        id.as_uuid(),
        &meta.canonical_title,
        meta.description.as_deref(),
        meta.cover_url.as_deref(),
        meta.content_type as ContentType,
        meta.status as SeriesStatus,
        meta.release_year,
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// Fetch one canonical series by id.
pub async fn get_series<'e, E: PgExecutor<'e>>(exec: E, id: SeriesId) -> DbResult<Series> {
    let row = sqlx::query_as!(
        SeriesRow,
        "SELECT id, canonical_title, normalized_title, description, cover_url, \
         content_type AS \"content_type: ContentType\", status AS \"status: SeriesStatus\", \
         release_year, created_at, updated_at \
         FROM series WHERE id = $1",
        id.as_uuid(),
    )
    .fetch_optional(exec)
    .await?;
    row.ok_or(DbError::NotFound)?.try_into()
}

/// A minimal series row for the tokenless metadata-enrichment worker: enough to look the
/// work up upstream (by mapped external id or by title) and to feed the metadata-priority
/// resolver the current locally-scraped values.
pub struct SeriesEnrichmentRow {
    pub id: SeriesId,
    pub canonical_title: String,
    pub description: Option<String>,
    pub cover_url: Option<String>,
}

/// List series for background metadata enrichment, oldest-updated first so a slow,
/// rate-limited sweep eventually covers the whole catalogue. Plain offset paging is fine
/// here — this is a background worker, not a hot path.
pub async fn list_series_for_enrichment<'e, E: PgExecutor<'e>>(
    exec: E,
    limit: i64,
    offset: i64,
) -> DbResult<Vec<SeriesEnrichmentRow>> {
    #[derive(FromRow)]
    struct Row {
        id: Uuid,
        canonical_title: String,
        description: Option<String>,
        cover_url: Option<String>,
    }
    let rows = sqlx::query_as!(
        Row,
        "SELECT id, canonical_title, description, cover_url FROM series \
         ORDER BY updated_at ASC LIMIT $1 OFFSET $2",
        limit,
        offset,
    )
    .fetch_all(exec)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| SeriesEnrichmentRow {
            id: SeriesId::from_uuid(r.id),
            canonical_title: r.canonical_title,
            description: r.description,
            cover_url: r.cover_url,
        })
        .collect())
}

/// A batch of metadata to fold into an existing series (the tokenless enrichment worker's
/// output). `description`/`cover_url` are already the values chosen by the metadata-priority
/// resolver; a `None` leaves the current value untouched. Titles/tags/authors are additive.
pub struct MetadataEnrichment<'a> {
    pub description: Option<&'a str>,
    pub cover_url: Option<&'a str>,
    /// Content-type token (e.g. `manga`/`manhwa`); only fills a currently-`unknown` series.
    pub content_type: Option<&'a str>,
    /// Release year; only fills a series whose year is currently null.
    pub release_year: Option<i32>,
    pub alt_titles: &'a [(String, String)],
    pub tags: &'a [String],
    pub authors: &'a [String],
}

/// Apply an enrichment batch to a series in one transaction: overwrite description/cover
/// (priority already applied by the caller) and additively record alternative titles, tags,
/// and authors. Idempotent — re-running converges to the same rows.
pub async fn apply_enrichment(
    pool: &sqlx::PgPool,
    series_id: SeriesId,
    enrichment: &MetadataEnrichment<'_>,
) -> DbResult<()> {
    let mut tx = pool.begin().await?;
    sqlx::query!(
        "UPDATE series SET \
            description = COALESCE($2, description), \
            cover_url = COALESCE($3, cover_url), \
            content_type = CASE WHEN content_type = 'unknown' \
                                THEN COALESCE($4::text::content_type, content_type) \
                                ELSE content_type END, \
            release_year = COALESCE(release_year, $5), \
            updated_at = now() \
         WHERE id = $1",
        series_id.as_uuid(),
        enrichment.description,
        enrichment.cover_url,
        enrichment.content_type,
        enrichment.release_year,
    )
    .execute(&mut *tx)
    .await?;
    if !enrichment.alt_titles.is_empty() {
        add_series_titles(&mut tx, series_id, enrichment.alt_titles).await?;
    }
    if !enrichment.tags.is_empty() {
        add_series_tags(&mut tx, series_id, enrichment.tags).await?;
    }
    if !enrichment.authors.is_empty() {
        add_series_authors(&mut tx, series_id, enrichment.authors).await?;
    }
    tx.commit().await?;
    Ok(())
}

/// Add alternative titles (idempotent on the natural key).
pub async fn add_series_titles(
    conn: &mut sqlx::PgConnection,
    series_id: SeriesId,
    titles: &[(String, String)],
) -> DbResult<()> {
    for (title, normalized) in titles {
        if normalized.is_empty() {
            continue;
        }
        sqlx::query!(
            "INSERT INTO series_titles (series_id, title, normalized) VALUES ($1,$2,$3) \
             ON CONFLICT (series_id, normalized) DO UPDATE SET title = EXCLUDED.title",
            series_id.as_uuid(),
            title,
            normalized,
        )
        .execute(&mut *conn)
        .await?;
    }
    Ok(())
}

/// A URL-safe, lowercase identity key for a display name (tag or author). Deliberately
/// distinct from [`normalize_title`] — that function drops "noise" words like "scan" or
/// "comic" which would wrongly mangle a genre or a person's name.
fn slugify(name: &str) -> String {
    let mut slug = String::with_capacity(name.len());
    let mut last_was_dash = true; // suppresses a leading dash
    for c in name.to_lowercase().chars() {
        if c.is_alphanumeric() {
            slug.push(c);
            last_was_dash = false;
        } else if !last_was_dash {
            slug.push('-');
            last_was_dash = true;
        }
    }
    slug.trim_end_matches('-').to_owned()
}

/// Add genre/tag names to a series (idempotent, additive-only — never removes a tag a
/// different source contributed). Empty/unslugifiable names are skipped.
pub async fn add_series_tags(
    conn: &mut sqlx::PgConnection,
    series_id: SeriesId,
    tags: &[String],
) -> DbResult<()> {
    for name in tags {
        let slug = slugify(name);
        if slug.is_empty() {
            continue;
        }
        let tag_id = sqlx::query_scalar!(
            "INSERT INTO tags (slug, name) VALUES ($1,$2) \
             ON CONFLICT (slug) DO UPDATE SET name = tags.name RETURNING id",
            &slug,
            name,
        )
        .fetch_one(&mut *conn)
        .await?;
        sqlx::query!(
            "INSERT INTO series_tags (series_id, tag_id) VALUES ($1,$2) \
             ON CONFLICT DO NOTHING",
            series_id.as_uuid(),
            tag_id,
        )
        .execute(&mut *conn)
        .await?;
    }
    Ok(())
}

/// Add author/artist credits to a series (idempotent, additive-only — mirrors
/// [`add_series_tags`]).
pub async fn add_series_authors(
    conn: &mut sqlx::PgConnection,
    series_id: SeriesId,
    authors: &[String],
) -> DbResult<()> {
    for name in authors {
        let slug = slugify(name);
        if slug.is_empty() {
            continue;
        }
        let author_id = sqlx::query_scalar!(
            "INSERT INTO authors (slug, name) VALUES ($1,$2) \
             ON CONFLICT (slug) DO UPDATE SET name = authors.name RETURNING id",
            &slug,
            name,
        )
        .fetch_one(&mut *conn)
        .await?;
        sqlx::query!(
            "INSERT INTO series_authors (series_id, author_id) VALUES ($1,$2) \
             ON CONFLICT DO NOTHING",
            series_id.as_uuid(),
            author_id,
        )
        .execute(&mut *conn)
        .await?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Series sources
// ---------------------------------------------------------------------------

#[derive(FromRow)]
struct SourceRow {
    id: Uuid,
    series_id: Uuid,
    provider_id: Uuid,
    source_path: String,
    provider_title: Option<String>,
    content_hash: Option<Vec<u8>>,
    chapter_count: i32,
    last_scanned_at: Option<OffsetDateTime>,
    state: ProviderState,
}

impl TryFrom<SourceRow> for SeriesSource {
    type Error = DbError;
    fn try_from(r: SourceRow) -> Result<Self, Self::Error> {
        Ok(Self {
            id: SeriesSourceId::from_uuid(r.id),
            series_id: SeriesId::from_uuid(r.series_id),
            provider_id: ProviderId::from_uuid(r.provider_id),
            source_path: r.source_path,
            provider_title: r.provider_title,
            content_hash: r.content_hash,
            chapter_count: r.chapter_count,
            last_scanned_at: r.last_scanned_at,
            state: r.state,
        })
    }
}

/// Upsert the (provider, path) source for a series. Idempotent on `(provider_id, source_path)`.
pub async fn upsert_source<'e, E: PgExecutor<'e>>(
    exec: E,
    series_id: SeriesId,
    provider_id: ProviderId,
    source_path: &str,
    provider_title: Option<&str>,
) -> DbResult<SeriesSourceId> {
    let id = sqlx::query_scalar!(
        "INSERT INTO series_sources (id, series_id, provider_id, source_path, provider_title) \
         VALUES ($1,$2,$3,$4,$5) \
         ON CONFLICT (provider_id, source_path) DO UPDATE \
            SET provider_title = EXCLUDED.provider_title \
         RETURNING id",
        SeriesSourceId::new().as_uuid(),
        series_id.as_uuid(),
        provider_id.as_uuid(),
        source_path,
        provider_title,
    )
    .fetch_one(exec)
    .await?;
    Ok(SeriesSourceId::from_uuid(id))
}

/// Ensure a series **source** row exists for a catalogue entry, creating a canonical
/// series from the listing title when the source is new.
///
/// This is the breadth-first "collect all series first" step of a full scan (design §12):
/// the catalogue walk registers every series immediately from its listing title + path, so
/// the complete series list materialises before any per-series chapter fetch runs. It is a
/// **no-op when the source already exists**, so it never downgrades metadata that a later
/// `Series` task (or an earlier scan) has already enriched, and it never touches chapters.
///
/// For a genuinely new source it runs the same canonicalisation pipeline as `ingest_series`
/// ([`resolve_canonical_series`]); the subsequent `Series` task, resolving from the fuller
/// series-page title, attaches to this same canonical series in the common case where the
/// titles agree after normalisation.
pub async fn register_source_stub(
    pool: &sqlx::PgPool,
    provider_id: ProviderId,
    source_path: &str,
    title: &str,
) -> DbResult<()> {
    let mut tx = pool.begin().await?;

    // Already registered (this or an earlier scan) — leave the enriched row untouched.
    let existing = sqlx::query_scalar!(
        "SELECT id FROM series_sources WHERE provider_id = $1 AND source_path = $2",
        provider_id.as_uuid(),
        source_path,
    )
    .fetch_optional(&mut *tx)
    .await?;
    if existing.is_some() {
        return Ok(());
    }

    let meta = SeriesUpsert {
        canonical_title: title.to_owned(),
        normalized_title: normalize_title(title),
        description: None,
        cover_url: None,
        content_type: ContentType::Unknown,
        status: SeriesStatus::Unknown,
        release_year: None,
    };
    let series_id = resolve_canonical_series(&mut tx, &meta).await?;
    upsert_source(&mut *tx, series_id, provider_id, source_path, Some(title)).await?;

    tx.commit().await?;
    Ok(())
}

/// Record the result of a source scan: content hash + chapter count + timestamp.
pub async fn update_source_scan<'e, E: PgExecutor<'e>>(
    exec: E,
    source_id: SeriesSourceId,
    content_hash: &[u8],
    chapter_count: i32,
) -> DbResult<()> {
    sqlx::query!(
        "UPDATE series_sources SET content_hash = $2, chapter_count = $3, \
         last_scanned_at = now() WHERE id = $1",
        source_id.as_uuid(),
        content_hash,
        chapter_count,
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// The previously stored content hash for a source, if any (change-detection skip).
pub async fn source_content_hash<'e, E: PgExecutor<'e>>(
    exec: E,
    provider_id: ProviderId,
    source_path: &str,
) -> DbResult<Option<Vec<u8>>> {
    let hash = sqlx::query_scalar!(
        "SELECT content_hash FROM series_sources WHERE provider_id = $1 AND source_path = $2",
        provider_id.as_uuid(),
        source_path,
    )
    .fetch_optional(exec)
    .await?;
    Ok(hash.flatten())
}

/// List the sources of a canonical series (for the "Read on: A · B · C" strip).
pub async fn list_sources_for_series<'e, E: PgExecutor<'e>>(
    exec: E,
    series_id: SeriesId,
) -> DbResult<Vec<SeriesSource>> {
    let rows = sqlx::query_as!(
        SourceRow,
        "SELECT id, series_id, provider_id, source_path, provider_title, \
         content_hash, chapter_count, last_scanned_at, state AS \"state: ProviderState\" \
         FROM series_sources WHERE series_id = $1 ORDER BY id",
        series_id.as_uuid(),
    )
    .fetch_all(exec)
    .await?;
    rows.into_iter().map(SeriesSource::try_from).collect()
}

// ---------------------------------------------------------------------------
// Chapters
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// Read model: series listing (for GET /v1/series)
// ---------------------------------------------------------------------------

/// A row in the discover/browse list: the series plus its resolvable cover and a
/// count of provider sources.
pub struct SeriesListItem {
    pub series: Series,
    pub source_count: i64,
}

/// Query the browse list with keyset pagination on `(created_at, id)`.
pub async fn list_series<'e, E: PgExecutor<'e>>(
    exec: E,
    query: Option<&str>,
    limit: i64,
) -> DbResult<Vec<SeriesListItem>> {
    // Trigram + FTS aware search when a query is supplied; otherwise most-recent first.
    #[derive(FromRow)]
    struct ListRow {
        id: Uuid,
        canonical_title: String,
        normalized_title: String,
        description: Option<String>,
        cover_url: Option<String>,
        content_type: ContentType,
        status: SeriesStatus,
        release_year: Option<i32>,
        created_at: OffsetDateTime,
        updated_at: OffsetDateTime,
        source_count: i64,
    }

    let rows = if let Some(q) = query {
        // Search matches the canonical title, the full-text vector, **and** any alternative
        // title/synonym (english/native/AniList synonyms recorded in `series_titles`), so a
        // work found under a non-primary name still surfaces. Ranking takes the best trigram
        // similarity across the canonical and alternative titles.
        sqlx::query_as!(
            ListRow,
            "SELECT s.id, s.canonical_title, s.normalized_title, s.description, s.cover_url, \
                    s.content_type AS \"content_type: ContentType\", \
                    s.status AS \"status: SeriesStatus\", s.release_year, \
                    s.created_at, s.updated_at, \
                    (SELECT count(*) FROM series_sources ss WHERE ss.series_id = s.id) AS \"source_count!\" \
             FROM series s \
             WHERE s.normalized_title % $1 OR s.search_vec @@ plainto_tsquery('simple', $1) \
                OR EXISTS (SELECT 1 FROM series_titles st \
                           WHERE st.series_id = s.id AND st.normalized % $1) \
             ORDER BY GREATEST( \
                        similarity(s.normalized_title, $1), \
                        COALESCE((SELECT MAX(similarity(st.normalized, $1)) \
                                  FROM series_titles st WHERE st.series_id = s.id), 0) \
                      ) DESC \
             LIMIT $2",
            q,
            limit,
        )
        .fetch_all(exec)
        .await?
    } else {
        sqlx::query_as!(
            ListRow,
            "SELECT s.id, s.canonical_title, s.normalized_title, s.description, s.cover_url, \
                    s.content_type AS \"content_type: ContentType\", \
                    s.status AS \"status: SeriesStatus\", s.release_year, \
                    s.created_at, s.updated_at, \
                    (SELECT count(*) FROM series_sources ss WHERE ss.series_id = s.id) AS \"source_count!\" \
             FROM series s ORDER BY s.updated_at DESC LIMIT $1",
            limit,
        )
        .fetch_all(exec)
        .await?
    };

    Ok(rows
        .into_iter()
        .map(|r| SeriesListItem {
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
        })
        .collect())
}

// ---------------------------------------------------------------------------
// Read model: filtered/sorted/paginated series listing (GET /v1/series, §9.1)
// ---------------------------------------------------------------------------

/// Server-side filter/sort/paginate criteria for the Discover grid (frontend §9.1).
///
/// Every field is optional; `None`/empty means "no constraint". Enum filters match on the
/// text token (`content_type::text` / `status::text`) so callers never touch the Postgres
/// enum types. `tags` requires the series to carry **all** listed slugs; `exclude_tags`
/// removes any series carrying **any** listed slug.
#[derive(Debug, Default, Clone)]
pub struct SeriesFilter {
    pub query: Option<String>,
    pub content_type: Option<String>,
    pub status: Option<String>,
    pub provider_slug: Option<String>,
    pub tags: Vec<String>,
    pub exclude_tags: Vec<String>,
    pub year_min: Option<i32>,
    pub year_max: Option<i32>,
    pub min_chapters: Option<i32>,
    /// `updated | title | chapters | sources | year`; anything else falls back to `updated`.
    pub sort: Option<String>,
    pub limit: i64,
    pub offset: i64,
}

/// A page of the filtered browse list plus the total number of matching rows (via the
/// window `count(*) OVER()`), so the API can render `{ items, total, next_cursor }` from a
/// single query.
pub struct SeriesPage {
    pub items: Vec<SeriesListItem>,
    pub total: i64,
}

#[derive(FromRow)]
struct FilteredRow {
    id: Uuid,
    canonical_title: String,
    normalized_title: String,
    description: Option<String>,
    cover_url: Option<String>,
    content_type: ContentType,
    status: SeriesStatus,
    release_year: Option<i32>,
    created_at: OffsetDateTime,
    updated_at: OffsetDateTime,
    source_count: i64,
    total: i64,
}

/// Query the browse list with server-side filtering, sorting, and offset pagination
/// (frontend §9.1). Returns the page plus the total match count for the pager.
///
/// Every filter is expressed as an `($n IS NULL OR …)` guard so each sort variant stays a
/// single static SQL string (`sqlx::query_as!` requires a string literal) while binds toggle
/// each constraint on or off.
// The compile-time-checked macros cannot take a dynamically-assembled SQL string, so each
// sort order is spelled out as its own otherwise-identical `query_as!`; that repetition is
// what pushes this over the line-count lint, not real complexity.
#[allow(clippy::too_many_lines)]
pub async fn list_series_filtered<'e, E: PgExecutor<'e>>(
    exec: E,
    filter: &SeriesFilter,
) -> DbResult<SeriesPage> {
    let query = filter
        .query
        .as_deref()
        .map(str::trim)
        .filter(|q| !q.is_empty());

    // Shared projection + WHERE. `source_count` is ordinal 11; `ORDER BY` variants that
    // previously referenced the `chapter_total` select-alias inline that expression instead
    // (compile-time macros cannot carry `!` markers into ORDER BY aliases, and an unused
    // select column would not map onto `FilteredRow`).
    let rows = match filter.sort.as_deref() {
        Some("title") => {
            sqlx::query_as!(
                FilteredRow,
                "SELECT s.id, s.canonical_title, s.normalized_title, s.description, s.cover_url, \
                        s.content_type AS \"content_type: ContentType\", \
                        s.status AS \"status: SeriesStatus\", s.release_year, \
                        s.created_at, s.updated_at, \
                        (SELECT count(*) FROM series_sources ss WHERE ss.series_id = s.id) AS \"source_count!\", \
                        count(*) OVER() AS \"total!\" \
                 FROM series s \
                 WHERE ($1::text IS NULL OR s.normalized_title % $1 \
                         OR s.search_vec @@ plainto_tsquery('simple', $1) \
                         OR EXISTS (SELECT 1 FROM series_titles st \
                                    WHERE st.series_id = s.id AND st.normalized % $1)) \
                   AND ($2::text IS NULL OR s.content_type::text = $2) \
                   AND ($3::text IS NULL OR s.status::text = $3) \
                   AND ($4::int IS NULL OR s.release_year >= $4) \
                   AND ($5::int IS NULL OR s.release_year <= $5) \
                   AND ($6::text IS NULL OR EXISTS ( \
                         SELECT 1 FROM series_sources ss JOIN providers p ON p.id = ss.provider_id \
                         WHERE ss.series_id = s.id AND p.slug = $6)) \
                   AND ($7::int IS NULL OR ( \
                         SELECT COALESCE(sum(ss.chapter_count),0) FROM series_sources ss \
                         WHERE ss.series_id = s.id) >= $7) \
                   AND (cardinality($8::text[]) = 0 OR NOT EXISTS ( \
                         SELECT unnest($8::text[]) \
                         EXCEPT \
                         SELECT t.slug FROM series_tags stg JOIN tags t ON t.id = stg.tag_id \
                         WHERE stg.series_id = s.id)) \
                   AND (cardinality($9::text[]) = 0 OR NOT EXISTS ( \
                         SELECT 1 FROM series_tags stg JOIN tags t ON t.id = stg.tag_id \
                         WHERE stg.series_id = s.id AND t.slug = ANY($9::text[]))) \
                 ORDER BY s.canonical_title ASC \
                 LIMIT $10 OFFSET $11",
                query,
                filter.content_type.as_deref(),
                filter.status.as_deref(),
                filter.year_min,
                filter.year_max,
                filter.provider_slug.as_deref(),
                filter.min_chapters,
                &filter.tags as &[String],
                &filter.exclude_tags as &[String],
                filter.limit,
                filter.offset,
            )
            .fetch_all(exec)
            .await?
        }
        Some("chapters") => {
            sqlx::query_as!(
                FilteredRow,
                "SELECT s.id, s.canonical_title, s.normalized_title, s.description, s.cover_url, \
                        s.content_type AS \"content_type: ContentType\", \
                        s.status AS \"status: SeriesStatus\", s.release_year, \
                        s.created_at, s.updated_at, \
                        (SELECT count(*) FROM series_sources ss WHERE ss.series_id = s.id) AS \"source_count!\", \
                        count(*) OVER() AS \"total!\" \
                 FROM series s \
                 WHERE ($1::text IS NULL OR s.normalized_title % $1 \
                         OR s.search_vec @@ plainto_tsquery('simple', $1) \
                         OR EXISTS (SELECT 1 FROM series_titles st \
                                    WHERE st.series_id = s.id AND st.normalized % $1)) \
                   AND ($2::text IS NULL OR s.content_type::text = $2) \
                   AND ($3::text IS NULL OR s.status::text = $3) \
                   AND ($4::int IS NULL OR s.release_year >= $4) \
                   AND ($5::int IS NULL OR s.release_year <= $5) \
                   AND ($6::text IS NULL OR EXISTS ( \
                         SELECT 1 FROM series_sources ss JOIN providers p ON p.id = ss.provider_id \
                         WHERE ss.series_id = s.id AND p.slug = $6)) \
                   AND ($7::int IS NULL OR ( \
                         SELECT COALESCE(sum(ss.chapter_count),0) FROM series_sources ss \
                         WHERE ss.series_id = s.id) >= $7) \
                   AND (cardinality($8::text[]) = 0 OR NOT EXISTS ( \
                         SELECT unnest($8::text[]) \
                         EXCEPT \
                         SELECT t.slug FROM series_tags stg JOIN tags t ON t.id = stg.tag_id \
                         WHERE stg.series_id = s.id)) \
                   AND (cardinality($9::text[]) = 0 OR NOT EXISTS ( \
                         SELECT 1 FROM series_tags stg JOIN tags t ON t.id = stg.tag_id \
                         WHERE stg.series_id = s.id AND t.slug = ANY($9::text[]))) \
                 ORDER BY (SELECT COALESCE(sum(ss.chapter_count),0)::int8 FROM series_sources ss \
                             WHERE ss.series_id = s.id) DESC, s.updated_at DESC \
                 LIMIT $10 OFFSET $11",
                query,
                filter.content_type.as_deref(),
                filter.status.as_deref(),
                filter.year_min,
                filter.year_max,
                filter.provider_slug.as_deref(),
                filter.min_chapters,
                &filter.tags as &[String],
                &filter.exclude_tags as &[String],
                filter.limit,
                filter.offset,
            )
            .fetch_all(exec)
            .await?
        }
        Some("sources") => {
            sqlx::query_as!(
                FilteredRow,
                "SELECT s.id, s.canonical_title, s.normalized_title, s.description, s.cover_url, \
                        s.content_type AS \"content_type: ContentType\", \
                        s.status AS \"status: SeriesStatus\", s.release_year, \
                        s.created_at, s.updated_at, \
                        (SELECT count(*) FROM series_sources ss WHERE ss.series_id = s.id) AS \"source_count!\", \
                        count(*) OVER() AS \"total!\" \
                 FROM series s \
                 WHERE ($1::text IS NULL OR s.normalized_title % $1 \
                         OR s.search_vec @@ plainto_tsquery('simple', $1) \
                         OR EXISTS (SELECT 1 FROM series_titles st \
                                    WHERE st.series_id = s.id AND st.normalized % $1)) \
                   AND ($2::text IS NULL OR s.content_type::text = $2) \
                   AND ($3::text IS NULL OR s.status::text = $3) \
                   AND ($4::int IS NULL OR s.release_year >= $4) \
                   AND ($5::int IS NULL OR s.release_year <= $5) \
                   AND ($6::text IS NULL OR EXISTS ( \
                         SELECT 1 FROM series_sources ss JOIN providers p ON p.id = ss.provider_id \
                         WHERE ss.series_id = s.id AND p.slug = $6)) \
                   AND ($7::int IS NULL OR ( \
                         SELECT COALESCE(sum(ss.chapter_count),0) FROM series_sources ss \
                         WHERE ss.series_id = s.id) >= $7) \
                   AND (cardinality($8::text[]) = 0 OR NOT EXISTS ( \
                         SELECT unnest($8::text[]) \
                         EXCEPT \
                         SELECT t.slug FROM series_tags stg JOIN tags t ON t.id = stg.tag_id \
                         WHERE stg.series_id = s.id)) \
                   AND (cardinality($9::text[]) = 0 OR NOT EXISTS ( \
                         SELECT 1 FROM series_tags stg JOIN tags t ON t.id = stg.tag_id \
                         WHERE stg.series_id = s.id AND t.slug = ANY($9::text[]))) \
                 ORDER BY 11 DESC, s.updated_at DESC \
                 LIMIT $10 OFFSET $11",
                query,
                filter.content_type.as_deref(),
                filter.status.as_deref(),
                filter.year_min,
                filter.year_max,
                filter.provider_slug.as_deref(),
                filter.min_chapters,
                &filter.tags as &[String],
                &filter.exclude_tags as &[String],
                filter.limit,
                filter.offset,
            )
            .fetch_all(exec)
            .await?
        }
        Some("year") => {
            sqlx::query_as!(
                FilteredRow,
                "SELECT s.id, s.canonical_title, s.normalized_title, s.description, s.cover_url, \
                        s.content_type AS \"content_type: ContentType\", \
                        s.status AS \"status: SeriesStatus\", s.release_year, \
                        s.created_at, s.updated_at, \
                        (SELECT count(*) FROM series_sources ss WHERE ss.series_id = s.id) AS \"source_count!\", \
                        count(*) OVER() AS \"total!\" \
                 FROM series s \
                 WHERE ($1::text IS NULL OR s.normalized_title % $1 \
                         OR s.search_vec @@ plainto_tsquery('simple', $1) \
                         OR EXISTS (SELECT 1 FROM series_titles st \
                                    WHERE st.series_id = s.id AND st.normalized % $1)) \
                   AND ($2::text IS NULL OR s.content_type::text = $2) \
                   AND ($3::text IS NULL OR s.status::text = $3) \
                   AND ($4::int IS NULL OR s.release_year >= $4) \
                   AND ($5::int IS NULL OR s.release_year <= $5) \
                   AND ($6::text IS NULL OR EXISTS ( \
                         SELECT 1 FROM series_sources ss JOIN providers p ON p.id = ss.provider_id \
                         WHERE ss.series_id = s.id AND p.slug = $6)) \
                   AND ($7::int IS NULL OR ( \
                         SELECT COALESCE(sum(ss.chapter_count),0) FROM series_sources ss \
                         WHERE ss.series_id = s.id) >= $7) \
                   AND (cardinality($8::text[]) = 0 OR NOT EXISTS ( \
                         SELECT unnest($8::text[]) \
                         EXCEPT \
                         SELECT t.slug FROM series_tags stg JOIN tags t ON t.id = stg.tag_id \
                         WHERE stg.series_id = s.id)) \
                   AND (cardinality($9::text[]) = 0 OR NOT EXISTS ( \
                         SELECT 1 FROM series_tags stg JOIN tags t ON t.id = stg.tag_id \
                         WHERE stg.series_id = s.id AND t.slug = ANY($9::text[]))) \
                 ORDER BY s.release_year DESC NULLS LAST, s.updated_at DESC \
                 LIMIT $10 OFFSET $11",
                query,
                filter.content_type.as_deref(),
                filter.status.as_deref(),
                filter.year_min,
                filter.year_max,
                filter.provider_slug.as_deref(),
                filter.min_chapters,
                &filter.tags as &[String],
                &filter.exclude_tags as &[String],
                filter.limit,
                filter.offset,
            )
            .fetch_all(exec)
            .await?
        }
        // `updated` and design-only `rating` (no column) both fall back to recency.
        _ => {
            sqlx::query_as!(
                FilteredRow,
                "SELECT s.id, s.canonical_title, s.normalized_title, s.description, s.cover_url, \
                        s.content_type AS \"content_type: ContentType\", \
                        s.status AS \"status: SeriesStatus\", s.release_year, \
                        s.created_at, s.updated_at, \
                        (SELECT count(*) FROM series_sources ss WHERE ss.series_id = s.id) AS \"source_count!\", \
                        count(*) OVER() AS \"total!\" \
                 FROM series s \
                 WHERE ($1::text IS NULL OR s.normalized_title % $1 \
                         OR s.search_vec @@ plainto_tsquery('simple', $1) \
                         OR EXISTS (SELECT 1 FROM series_titles st \
                                    WHERE st.series_id = s.id AND st.normalized % $1)) \
                   AND ($2::text IS NULL OR s.content_type::text = $2) \
                   AND ($3::text IS NULL OR s.status::text = $3) \
                   AND ($4::int IS NULL OR s.release_year >= $4) \
                   AND ($5::int IS NULL OR s.release_year <= $5) \
                   AND ($6::text IS NULL OR EXISTS ( \
                         SELECT 1 FROM series_sources ss JOIN providers p ON p.id = ss.provider_id \
                         WHERE ss.series_id = s.id AND p.slug = $6)) \
                   AND ($7::int IS NULL OR ( \
                         SELECT COALESCE(sum(ss.chapter_count),0) FROM series_sources ss \
                         WHERE ss.series_id = s.id) >= $7) \
                   AND (cardinality($8::text[]) = 0 OR NOT EXISTS ( \
                         SELECT unnest($8::text[]) \
                         EXCEPT \
                         SELECT t.slug FROM series_tags stg JOIN tags t ON t.id = stg.tag_id \
                         WHERE stg.series_id = s.id)) \
                   AND (cardinality($9::text[]) = 0 OR NOT EXISTS ( \
                         SELECT 1 FROM series_tags stg JOIN tags t ON t.id = stg.tag_id \
                         WHERE stg.series_id = s.id AND t.slug = ANY($9::text[]))) \
                 ORDER BY s.updated_at DESC \
                 LIMIT $10 OFFSET $11",
                query,
                filter.content_type.as_deref(),
                filter.status.as_deref(),
                filter.year_min,
                filter.year_max,
                filter.provider_slug.as_deref(),
                filter.min_chapters,
                &filter.tags as &[String],
                &filter.exclude_tags as &[String],
                filter.limit,
                filter.offset,
            )
            .fetch_all(exec)
            .await?
        }
    };

    let total = rows.first().map_or(0, |r| r.total);
    let items = rows
        .into_iter()
        .map(|r| SeriesListItem {
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
        })
        .collect();
    Ok(SeriesPage { items, total })
}

/// Alternative titles of a series (design §9.2 enrichment). Empty when none are recorded.
pub async fn list_series_titles<'e, E: PgExecutor<'e>>(
    exec: E,
    series_id: SeriesId,
) -> DbResult<Vec<String>> {
    let rows = sqlx::query_scalar!(
        "SELECT title FROM series_titles WHERE series_id = $1 ORDER BY title",
        series_id.as_uuid(),
    )
    .fetch_all(exec)
    .await?;
    Ok(rows)
}

/// Author/artist credits attached to a series, alphabetically (mirrors [`list_series_tags`]).
pub async fn list_series_authors<'e, E: PgExecutor<'e>>(
    exec: E,
    series_id: SeriesId,
) -> DbResult<Vec<tankovault_domain::Author>> {
    #[derive(FromRow)]
    struct Row {
        id: Uuid,
        slug: String,
        name: String,
    }
    let rows = sqlx::query_as!(
        Row,
        "SELECT a.id, a.slug, a.name FROM series_authors sa JOIN authors a ON a.id = sa.author_id \
         WHERE sa.series_id = $1 ORDER BY a.name",
        series_id.as_uuid(),
    )
    .fetch_all(exec)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| tankovault_domain::Author {
            id: tankovault_domain::AuthorId::from_uuid(r.id),
            slug: r.slug,
            name: r.name,
        })
        .collect())
}

/// Tags attached to a series, alphabetically (design §9.2 enrichment).
pub async fn list_series_tags<'e, E: PgExecutor<'e>>(
    exec: E,
    series_id: SeriesId,
) -> DbResult<Vec<tankovault_domain::Tag>> {
    #[derive(FromRow)]
    struct Row {
        id: Uuid,
        slug: String,
        name: String,
    }
    let rows = sqlx::query_as!(
        Row,
        "SELECT t.id, t.slug, t.name FROM series_tags stg JOIN tags t ON t.id = stg.tag_id \
         WHERE stg.series_id = $1 ORDER BY t.name",
        series_id.as_uuid(),
    )
    .fetch_all(exec)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| tankovault_domain::Tag {
            id: tankovault_domain::TagId::from_uuid(r.id),
            slug: r.slug,
            name: r.name,
        })
        .collect())
}

/// List all tags/genres, alphabetically (design §11 `GET /v1/tags`).
pub async fn list_tags<'e, E: PgExecutor<'e>>(exec: E) -> DbResult<Vec<tankovault_domain::Tag>> {
    #[derive(FromRow)]
    struct Row {
        id: Uuid,
        slug: String,
        name: String,
    }
    let rows = sqlx::query_as!(Row, "SELECT id, slug, name FROM tags ORDER BY name")
        .fetch_all(exec)
        .await?;
    Ok(rows
        .into_iter()
        .map(|r| tankovault_domain::Tag {
            id: tankovault_domain::TagId::from_uuid(r.id),
            slug: r.slug,
            name: r.name,
        })
        .collect())
}

/// The `(provider_id, base_url)` a source belongs to — the minimal input the API needs
/// to resolve a source's relative paths into absolute links at read time.
pub async fn source_provider_base_url<'e, E: PgExecutor<'e>>(
    exec: E,
    source_id: SeriesSourceId,
) -> DbResult<(ProviderId, String)> {
    #[derive(FromRow)]
    struct Row {
        id: Uuid,
        base_url: String,
    }
    let row = sqlx::query_as!(
        Row,
        "SELECT p.id, p.base_url FROM providers p \
         JOIN series_sources ss ON ss.provider_id = p.id WHERE ss.id = $1",
        source_id.as_uuid(),
    )
    .fetch_optional(exec)
    .await?;
    let row = row.ok_or(DbError::NotFound)?;
    Ok((ProviderId::from_uuid(row.id), row.base_url))
}

// ---------------------------------------------------------------------------
// Composite ingest (worker `series` task): one transaction, idempotent, returns
// the genuinely new chapters for `chapter.discovered` fan-out.
// ---------------------------------------------------------------------------

/// A fully-scanned series ready to persist: canonical metadata, alternative titles,
/// and the full chapter list, plus the content hash for change detection.
pub struct ScannedSeries {
    pub provider_id: ProviderId,
    pub source_path: String,
    pub provider_title: Option<String>,
    pub meta: SeriesUpsert,
    pub alt_titles: Vec<(String, String)>,
    pub tags: Vec<String>,
    pub authors: Vec<String>,
    pub chapters: Vec<ChapterUpsert>,
    pub content_hash: Vec<u8>,
}

/// Result of ingesting a scanned series.
pub struct IngestOutcome {
    pub series_id: SeriesId,
    pub source_id: SeriesSourceId,
    /// Newly-discovered chapter numbers, in input order (for `chapter.discovered`).
    pub new_chapters: Vec<f64>,
}

/// Persist a scanned series and its chapters in a single transaction.
///
/// All writes are idempotent (`ON CONFLICT`), so replaying a task under at-least-once
/// delivery converges to the same state and reports no false-new chapters.
pub async fn ingest_series(pool: &sqlx::PgPool, scanned: ScannedSeries) -> DbResult<IngestOutcome> {
    let mut tx = pool.begin().await?;

    let series_id = resolve_canonical_series(&mut tx, &scanned.meta).await?;
    update_series_meta(&mut *tx, series_id, &scanned.meta).await?;
    if !scanned.alt_titles.is_empty() {
        add_series_titles(&mut tx, series_id, &scanned.alt_titles).await?;
    }
    if !scanned.tags.is_empty() {
        add_series_tags(&mut tx, series_id, &scanned.tags).await?;
    }
    if !scanned.authors.is_empty() {
        add_series_authors(&mut tx, series_id, &scanned.authors).await?;
    }

    let source_id = upsert_source(
        &mut *tx,
        series_id,
        scanned.provider_id,
        &scanned.source_path,
        scanned.provider_title.as_deref(),
    )
    .await?;

    let mut new_chapters = Vec::new();
    for ch in &scanned.chapters {
        let res = upsert_chapter(&mut *tx, source_id, ch).await?;
        if res.inserted {
            new_chapters.push(res.number);
        }
    }

    let count = i32::try_from(scanned.chapters.len()).unwrap_or(i32::MAX);
    update_source_scan(&mut *tx, source_id, &scanned.content_hash, count).await?;

    tx.commit().await?;
    Ok(IngestOutcome {
        series_id,
        source_id,
        new_chapters,
    })
}
