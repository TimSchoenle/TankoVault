//! Catalog write + read path: canonical series, provider sources, and chapters.
//!
//! Every write is an idempotent `INSERT ... ON CONFLICT` so re-running a task under
//! at-least-once delivery is safe (design Appendix A §4). Chapter upserts report which
//! rows were genuinely new — via the `xmax = 0` idiom — so the worker can emit
//! `chapter.discovered` only for real discoveries.

use crate::error::{DbError, DbResult};
use tankovault_domain::{
    Chapter, ChapterId, ContentType, ProviderId, ProviderState, Series, SeriesId, SeriesSource,
    SeriesSourceId, SeriesStatus, normalize_title,
};
use sqlx::{FromRow, PgExecutor};
use std::str::FromStr;
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
    content_type: String,
    status: String,
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
            content_type: ContentType::from_str(&r.content_type)?,
            status: SeriesStatus::from_str(&r.status)?,
            release_year: r.release_year,
            created_at: r.created_at,
            updated_at: r.updated_at,
        })
    }
}

/// `series` column projection as a literal (keeps composed queries static).
macro_rules! series_cols {
    () => {
        "id, canonical_title, normalized_title, description, cover_url, \
         content_type::text AS content_type, status::text AS status, release_year, \
         created_at, updated_at"
    };
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
        })
        .collect::<Vec<_>>();

    let query = tankovault_matcher::Query {
        normalized_title: meta.normalized_title.clone(),
        content_type: meta.content_type,
        release_year: meta.release_year,
    };

    match tankovault_matcher::decide(&query, &candidates, tankovault_matcher::Thresholds::default()) {
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
    sqlx::query(
        "INSERT INTO series (id, canonical_title, normalized_title, description, \
         cover_url, content_type, status, release_year) \
         VALUES ($1,$2,$3,$4,$5,$6::content_type,$7::series_status,$8)",
    )
    .bind(id.as_uuid())
    .bind(&meta.canonical_title)
    .bind(&meta.normalized_title)
    .bind(&meta.description)
    .bind(&meta.cover_url)
    .bind(meta.content_type.as_str())
    .bind(meta.status.as_str())
    .bind(meta.release_year)
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
    sqlx::query(
        "UPDATE series SET \
            canonical_title = $2, \
            description = COALESCE($3, description), \
            cover_url = COALESCE($4, cover_url), \
            content_type = $5::content_type, \
            status = $6::series_status, \
            release_year = COALESCE($7, release_year), \
            updated_at = now() \
         WHERE id = $1",
    )
    .bind(id.as_uuid())
    .bind(&meta.canonical_title)
    .bind(&meta.description)
    .bind(&meta.cover_url)
    .bind(meta.content_type.as_str())
    .bind(meta.status.as_str())
    .bind(meta.release_year)
    .execute(exec)
    .await?;
    Ok(())
}

/// Fetch one canonical series by id.
pub async fn get_series<'e, E: PgExecutor<'e>>(exec: E, id: SeriesId) -> DbResult<Series> {
    let row: Option<SeriesRow> = sqlx::query_as(concat!(
        "SELECT ",
        series_cols!(),
        " FROM series WHERE id = $1"
    ))
    .bind(id.as_uuid())
    .fetch_optional(exec)
    .await?;
    row.ok_or(DbError::NotFound)?.try_into()
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
        sqlx::query(
            "INSERT INTO series_titles (series_id, title, normalized) VALUES ($1,$2,$3) \
             ON CONFLICT (series_id, normalized) DO UPDATE SET title = EXCLUDED.title",
        )
        .bind(series_id.as_uuid())
        .bind(title)
        .bind(normalized)
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
    state: String,
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
            state: ProviderState::from_str(&r.state)?,
        })
    }
}

/// `series_sources` column projection as a literal.
macro_rules! source_cols {
    () => {
        "id, series_id, provider_id, source_path, provider_title, \
         content_hash, chapter_count, last_scanned_at, state::text AS state"
    };
}

/// Upsert the (provider, path) source for a series. Idempotent on `(provider_id, source_path)`.
pub async fn upsert_source<'e, E: PgExecutor<'e>>(
    exec: E,
    series_id: SeriesId,
    provider_id: ProviderId,
    source_path: &str,
    provider_title: Option<&str>,
) -> DbResult<SeriesSourceId> {
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO series_sources (id, series_id, provider_id, source_path, provider_title) \
         VALUES ($1,$2,$3,$4,$5) \
         ON CONFLICT (provider_id, source_path) DO UPDATE \
            SET provider_title = EXCLUDED.provider_title \
         RETURNING id",
    )
    .bind(SeriesSourceId::new().as_uuid())
    .bind(series_id.as_uuid())
    .bind(provider_id.as_uuid())
    .bind(source_path)
    .bind(provider_title)
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
    let existing: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM series_sources WHERE provider_id = $1 AND source_path = $2",
    )
    .bind(provider_id.as_uuid())
    .bind(source_path)
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
    sqlx::query(
        "UPDATE series_sources SET content_hash = $2, chapter_count = $3, \
         last_scanned_at = now() WHERE id = $1",
    )
    .bind(source_id.as_uuid())
    .bind(content_hash)
    .bind(chapter_count)
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
    let hash: Option<Option<Vec<u8>>> = sqlx::query_scalar(
        "SELECT content_hash FROM series_sources WHERE provider_id = $1 AND source_path = $2",
    )
    .bind(provider_id.as_uuid())
    .bind(source_path)
    .fetch_optional(exec)
    .await?;
    Ok(hash.flatten())
}

/// List the sources of a canonical series (for the "Read on: A · B · C" strip).
pub async fn list_sources_for_series<'e, E: PgExecutor<'e>>(
    exec: E,
    series_id: SeriesId,
) -> DbResult<Vec<SeriesSource>> {
    let rows: Vec<SourceRow> = sqlx::query_as(concat!(
        "SELECT ",
        source_cols!(),
        " FROM series_sources WHERE series_id = $1 ORDER BY id"
    ))
    .bind(series_id.as_uuid())
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
    let inserted: bool = sqlx::query_scalar(
        "INSERT INTO chapters (id, series_source_id, number, volume, title, path, published_at) \
         VALUES ($1,$2,$3::numeric(10,4),$4,$5,$6,$7) \
         ON CONFLICT (series_source_id, number) DO UPDATE \
            SET title = EXCLUDED.title, path = EXCLUDED.path, \
                published_at = COALESCE(EXCLUDED.published_at, chapters.published_at) \
         RETURNING (xmax = 0) AS inserted",
    )
    .bind(ChapterId::new().as_uuid())
    .bind(source_id.as_uuid())
    .bind(ch.number)
    .bind(ch.volume)
    .bind(&ch.title)
    .bind(&ch.path)
    .bind(ch.published_at)
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
    let max: Option<f64> =
        sqlx::query_scalar("SELECT MAX(number)::float8 FROM chapters WHERE series_source_id = $1")
            .bind(source_id.as_uuid())
            .fetch_one(exec)
            .await?;
    Ok(max)
}

/// List chapters of a source, newest first (resolved to absolute links by the caller).
pub async fn list_chapters<'e, E: PgExecutor<'e>>(
    exec: E,
    source_id: SeriesSourceId,
) -> DbResult<Vec<Chapter>> {
    let rows: Vec<ChapterRow> = sqlx::query_as(
        "SELECT id, series_source_id, number::float8 AS number, volume, title, path, \
         published_at, discovered_at FROM chapters WHERE series_source_id = $1 \
         ORDER BY number DESC",
    )
    .bind(source_id.as_uuid())
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
        content_type: String,
        status: String,
        release_year: Option<i32>,
        created_at: OffsetDateTime,
        updated_at: OffsetDateTime,
        source_count: i64,
    }

    let sql = if query.is_some() {
        "SELECT s.id, s.canonical_title, s.normalized_title, s.description, s.cover_url, \
                s.content_type::text AS content_type, s.status::text AS status, s.release_year, \
                s.created_at, s.updated_at, \
                (SELECT count(*) FROM series_sources ss WHERE ss.series_id = s.id) AS source_count \
         FROM series s \
         WHERE s.normalized_title % $1 OR s.search_vec @@ plainto_tsquery('simple', $1) \
         ORDER BY similarity(s.normalized_title, $1) DESC \
         LIMIT $2"
    } else {
        "SELECT s.id, s.canonical_title, s.normalized_title, s.description, s.cover_url, \
                s.content_type::text AS content_type, s.status::text AS status, s.release_year, \
                s.created_at, s.updated_at, \
                (SELECT count(*) FROM series_sources ss WHERE ss.series_id = s.id) AS source_count \
         FROM series s ORDER BY s.updated_at DESC LIMIT $2"
    };

    let rows: Vec<ListRow> = if let Some(q) = query {
        sqlx::query_as(sql)
            .bind(q)
            .bind(limit)
            .fetch_all(exec)
            .await?
    } else {
        sqlx::query_as(sql)
            .bind(Option::<String>::None)
            .bind(limit)
            .fetch_all(exec)
            .await?
    };

    rows.into_iter()
        .map(|r| {
            Ok(SeriesListItem {
                series: Series {
                    id: SeriesId::from_uuid(r.id),
                    canonical_title: r.canonical_title,
                    normalized_title: r.normalized_title,
                    description: r.description,
                    cover_url: r.cover_url,
                    content_type: ContentType::from_str(&r.content_type)?,
                    status: SeriesStatus::from_str(&r.status)?,
                    release_year: r.release_year,
                    created_at: r.created_at,
                    updated_at: r.updated_at,
                },
                source_count: r.source_count,
            })
        })
        .collect()
}

/// List all tags/genres, alphabetically (design §11 `GET /v1/tags`).
pub async fn list_tags<'e, E: PgExecutor<'e>>(exec: E) -> DbResult<Vec<tankovault_domain::Tag>> {
    #[derive(FromRow)]
    struct Row {
        id: Uuid,
        slug: String,
        name: String,
    }
    let rows: Vec<Row> = sqlx::query_as("SELECT id, slug, name FROM tags ORDER BY name")
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
    let row: Option<(Uuid, String)> = sqlx::query_as(
        "SELECT p.id, p.base_url FROM providers p \
         JOIN series_sources ss ON ss.provider_id = p.id WHERE ss.id = $1",
    )
    .bind(source_id.as_uuid())
    .fetch_optional(exec)
    .await?;
    let (pid, base) = row.ok_or(DbError::NotFound)?;
    Ok((ProviderId::from_uuid(pid), base))
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
