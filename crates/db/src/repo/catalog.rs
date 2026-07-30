//! Catalog write + read path: canonical series, provider sources, and chapters.
//!
//! Every write is an idempotent `INSERT ... ON CONFLICT` so re-running a task under
//! at-least-once delivery is safe (design Appendix A §4). Chapter upserts report which
//! rows were genuinely new — via the `xmax = 0` idiom — so the worker can emit
//! `chapter.discovered` only for real discoveries.

use crate::error::{DbError, DbResult};
use sqlx::{FromRow, PgExecutor, PgPool};
use tankovault_config::MatchingConfig;
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
/// `matching` carries the confidence policy: the same policy external sync applies when it
/// resolves a remote entry, so the two paths cannot disagree about whether two series are the
/// same (ARCH-16). It used to be `Thresholds::default()` hardcoded here.
pub async fn resolve_canonical_series(
    conn: &mut sqlx::PgConnection,
    meta: &SeriesUpsert,
    matching: &MatchingConfig,
) -> DbResult<SeriesId> {
    let candidates = crate::repo::matching::find_candidates(
        &mut *conn,
        &meta.normalized_title,
        matching.candidate_limit,
    )
    .await?
    .into_iter()
    .map(tankovault_matcher::Candidate::from)
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

    match tankovault_matcher::decide(&query, &candidates, matching.thresholds()) {
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
    /// The row's sort key, carried back so the caller can page from it. Part of the cursor,
    /// not of the enrichment payload.
    pub updated_at: OffsetDateTime,
}

/// One page of series for the background metadata-enrichment sweep, as a **keyset** walk.
///
/// `after` is the `(updated_at, id)` of the last row of the previous page, and `started_at`
/// is the timestamp the sweep began. Both are load-bearing:
///
/// - The previous shape was `ORDER BY updated_at ASC LIMIT $1 OFFSET $2`, and enrichment
///   *writes* `updated_at = now()`. So the sort key moved under the cursor: every enriched row
///   jumped to the end of the ordering, the rows behind it shifted forward by one, and the
///   next `OFFSET` skipped exactly those. The sweep silently missed series — not a slowdown,
///   a correctness bug.
/// - `updated_at < started_at` excludes rows this sweep has already touched, so a row cannot
///   be handed back to the same run.
/// - Keyset paging also drops the cost from O(n²/batch): `OFFSET` re-sorted the whole table
///   per batch (5 000 sorts of 500 000 rows for a full catalogue), whereas this seeks straight
///   into `series_enrichment_cursor_idx`.
pub async fn list_series_for_enrichment<'e, E: PgExecutor<'e>>(
    exec: E,
    limit: i64,
    after: Option<(OffsetDateTime, Uuid)>,
    started_at: OffsetDateTime,
) -> DbResult<Vec<SeriesEnrichmentRow>> {
    #[derive(FromRow)]
    struct Row {
        id: Uuid,
        canonical_title: String,
        description: Option<String>,
        cover_url: Option<String>,
        updated_at: OffsetDateTime,
    }
    let (after_updated, after_id) = match after {
        Some((updated, id)) => (Some(updated), Some(id)),
        None => (None, None),
    };
    let rows = sqlx::query_as!(
        Row,
        "SELECT id, canonical_title, description, cover_url, updated_at FROM series \
         WHERE updated_at < $4 \
           AND ($2::timestamptz IS NULL OR (updated_at, id) > ($2, $3)) \
         ORDER BY updated_at ASC, id ASC \
         LIMIT $1",
        limit,
        after_updated,
        after_id,
        started_at,
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
            updated_at: r.updated_at,
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
    // One statement, not one per title. `ingest_series` calls this inside a transaction that
    // also writes tags, authors and every chapter; each extra round trip was time the
    // transaction spent holding locks (PERF-11).
    //
    // De-duplicated on `normalized` first, because `ON CONFLICT ... DO UPDATE` refuses to
    // touch the same row twice within one command ("cannot affect row a second time"). The
    // per-row loop tolerated a provider listing one work under two spellings that normalise
    // identically; this keeps that tolerance instead of turning it into an error.
    let mut seen = std::collections::HashSet::new();
    let mut display = Vec::with_capacity(titles.len());
    let mut normalized = Vec::with_capacity(titles.len());
    for (title, norm) in titles {
        if norm.is_empty() || !seen.insert(norm.as_str()) {
            continue;
        }
        display.push(title.as_str());
        normalized.push(norm.as_str());
    }
    if normalized.is_empty() {
        return Ok(());
    }

    sqlx::query!(
        "INSERT INTO series_titles (series_id, title, normalized) \
         SELECT $1, u.title, u.normalized \
         FROM UNNEST($2::text[], $3::text[]) AS u(title, normalized) \
         ON CONFLICT (series_id, normalized) DO UPDATE SET title = EXCLUDED.title",
        series_id.as_uuid(),
        &display as &[&str],
        &normalized as &[&str],
    )
    .execute(&mut *conn)
    .await?;
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
    let (slugs, names) = dedup_by_slug(tags);
    if slugs.is_empty() {
        return Ok(());
    }

    // Two set-based statements instead of two per tag.
    //
    // `ON CONFLICT (slug) DO NOTHING`, not `DO UPDATE SET name = tags.name`. That no-op
    // update existed only to make `RETURNING id` fire on an existing row, and it cost a
    // row-level *write* lock plus a dead tuple per tag per ingest. `tags` rows are globally
    // shared, so two workers ingesting series that share a genre serialised on the same lock
    // for as long as the ingest transaction ran (PERF-11). Resolving ids by slug in the
    // second statement needs no lock at all.
    sqlx::query!(
        "INSERT INTO tags (slug, name) SELECT * FROM UNNEST($1::text[], $2::text[]) \
         ON CONFLICT (slug) DO NOTHING",
        &slugs as &[String],
        &names as &[&str],
    )
    .execute(&mut *conn)
    .await?;

    sqlx::query!(
        "INSERT INTO series_tags (series_id, tag_id) \
         SELECT $1, t.id FROM tags t WHERE t.slug = ANY($2::text[]) \
         ON CONFLICT DO NOTHING",
        series_id.as_uuid(),
        &slugs as &[String],
    )
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// Add author/artist credits to a series (idempotent, additive-only — mirrors
/// [`add_series_tags`]).
pub async fn add_series_authors(
    conn: &mut sqlx::PgConnection,
    series_id: SeriesId,
    authors: &[String],
) -> DbResult<()> {
    let (slugs, names) = dedup_by_slug(authors);
    if slugs.is_empty() {
        return Ok(());
    }

    // Same shape, and the same reasoning, as [`add_series_tags`]: `authors` is globally
    // shared, so the no-op `DO UPDATE` that used to make `RETURNING` fire took a write lock
    // on a row every concurrent ingest of the same creator also wanted.
    sqlx::query!(
        "INSERT INTO authors (slug, name) SELECT * FROM UNNEST($1::text[], $2::text[]) \
         ON CONFLICT (slug) DO NOTHING",
        &slugs as &[String],
        &names as &[&str],
    )
    .execute(&mut *conn)
    .await?;

    sqlx::query!(
        "INSERT INTO series_authors (series_id, author_id) \
         SELECT $1, a.id FROM authors a WHERE a.slug = ANY($2::text[]) \
         ON CONFLICT DO NOTHING",
        series_id.as_uuid(),
        &slugs as &[String],
    )
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// Slugify a list of display names, dropping empties and keeping the first spelling of each
/// slug.
///
/// The de-duplication is required, not tidiness: binding a slug twice in one `UNNEST` insert
/// would be harmless under `DO NOTHING`, but the link statement would then try to attach the
/// same `(series_id, tag_id)` pair twice. Returning the display names alongside keeps the two
/// arrays index-aligned for the `UNNEST`.
fn dedup_by_slug(names: &[String]) -> (Vec<String>, Vec<&str>) {
    let mut seen = std::collections::HashSet::new();
    let mut slugs = Vec::with_capacity(names.len());
    let mut display = Vec::with_capacity(names.len());
    for name in names {
        let slug = slugify(name);
        if slug.is_empty() || !seen.insert(slug.clone()) {
            continue;
        }
        slugs.push(slug);
        display.push(name.as_str());
    }
    (slugs, display)
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
    matching: &MatchingConfig,
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

    let series_id = resolve_stub_series(&mut tx, title, matching).await?;
    upsert_source(&mut *tx, series_id, provider_id, source_path, Some(title)).await?;

    tx.commit().await?;
    Ok(())
}

/// Canonicalise a catalogue listing title into a series id, creating one if nothing matches.
///
/// Shared by the single-entry and batched stub registration so both run *identical*
/// canonicalisation — the listing title carries no description, cover, type, status or year, and
/// deliberately so: a later `Series` task enriches the row from the fuller series page, and a
/// stub must never overwrite that with blanks.
async fn resolve_stub_series(
    conn: &mut sqlx::PgConnection,
    title: &str,
    matching: &MatchingConfig,
) -> DbResult<SeriesId> {
    let meta = SeriesUpsert {
        canonical_title: title.to_owned(),
        normalized_title: normalize_title(title),
        description: None,
        cover_url: None,
        content_type: ContentType::Unknown,
        status: SeriesStatus::Unknown,
        release_year: None,
    };
    resolve_canonical_series(conn, &meta, matching).await
}

/// Upsert several `(provider, path)` sources in one statement. Idempotent on
/// `(provider_id, source_path)`, like [`upsert_source`].
///
/// `DISTINCT ON (source_path)` is required rather than tidy: a catalogue page can list the same
/// path twice, and `ON CONFLICT DO UPDATE` cannot touch one row twice in a single statement.
async fn upsert_sources(
    conn: &mut sqlx::PgConnection,
    provider_id: ProviderId,
    rows: &[(SeriesId, String, String)],
) -> DbResult<()> {
    if rows.is_empty() {
        return Ok(());
    }
    let ids: Vec<Uuid> = rows
        .iter()
        .map(|_| SeriesSourceId::new().as_uuid())
        .collect();
    let series_ids: Vec<Uuid> = rows.iter().map(|(s, _, _)| s.as_uuid()).collect();
    let paths: Vec<String> = rows.iter().map(|(_, p, _)| p.clone()).collect();
    let titles: Vec<String> = rows.iter().map(|(_, _, t)| t.clone()).collect();
    sqlx::query!(
        "INSERT INTO series_sources (id, series_id, provider_id, source_path, provider_title) \
         SELECT DISTINCT ON (source_path) id, series_id, $2, source_path, provider_title \
         FROM UNNEST($1::uuid[], $3::uuid[], $4::text[], $5::text[]) \
              AS t(id, series_id, source_path, provider_title) \
         ON CONFLICT (provider_id, source_path) DO UPDATE \
            SET provider_title = EXCLUDED.provider_title",
        &ids,
        provider_id.as_uuid(),
        &series_ids,
        &paths,
        &titles,
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// How many new stubs share one transaction. Sized so the begin/commit and the source insert
/// amortise to near-nothing per entry while a chunk still holds its locks for a bounded time.
const STUB_CHUNK: usize = 500;

/// Register one chunk of genuinely-new entries in a single transaction.
///
/// Canonicalisation genuinely cannot be batched — each entry resolves against the series its
/// predecessors created, and that is visible inside the transaction — but the `upsert_source`
/// tail can, so a chunk costs `begin + N canonicalisations + 1 insert + commit` instead of
/// `N × (begin + check + canonicalisation + insert + commit)`.
async fn register_chunk(
    pool: &sqlx::PgPool,
    provider_id: ProviderId,
    chunk: &[(&str, &str)],
    matching: &MatchingConfig,
) -> DbResult<usize> {
    let mut tx = pool.begin().await?;
    let mut sources = Vec::with_capacity(chunk.len());
    for (path, title) in chunk {
        let series_id = resolve_stub_series(&mut tx, title, matching).await?;
        sources.push((series_id, (*path).to_owned(), (*title).to_owned()));
    }
    upsert_sources(&mut tx, provider_id, &sources).await?;
    tx.commit().await?;
    Ok(chunk.len())
}

/// Register a whole catalogue page's worth of entries, skipping those already known.
///
/// Same semantics as calling [`register_source_stub`] per entry, but set-based where it can be:
/// the "is this source already registered?" check — the *only* work needed for the overwhelming
/// majority of entries on a re-scan — is answered for the entire batch in one query, and the
/// genuinely-new remainder is registered [`STUB_CHUNK`] entries per transaction.
///
/// # Why the chunking matters (PERF-15)
///
/// A re-scan was already cheap; a **first** scan is all-new, and one transaction per entry meant
/// 20 000 fresh entries on a sitemap page cost 20 000 transactions of ~5 round trips each. That
/// is what blows the `JetStream` ack deadline the worker's queue module warns about, causing
/// redelivery and duplicated work — a self-amplifying slowdown on exactly the scans that matter
/// most.
///
/// The per-entry existence check inside the transaction is *not* repeated here: the caller-side
/// batch check already filtered. A source registered by a concurrent scan in the window between
/// the two is harmless — `upsert_sources` is `ON CONFLICT DO UPDATE` on `(provider_id,
/// source_path)`, and canonicalisation re-attaches to the series that already matches the title
/// rather than creating a second one.
///
/// Returns the number of sources newly registered. A chunk that fails is retried entry by entry
/// so one bad entry costs only itself: losing one series must not lose the rest, and the
/// enrichment task is enqueued regardless (design §12).
pub async fn register_source_stubs(
    pool: &sqlx::PgPool,
    provider_id: ProviderId,
    entries: &[(&str, &str)],
    matching: &MatchingConfig,
) -> DbResult<usize> {
    if entries.is_empty() {
        return Ok(0);
    }

    let paths: Vec<String> = entries.iter().map(|(path, _)| (*path).to_owned()).collect();
    let known: std::collections::HashSet<String> = sqlx::query_scalar!(
        "SELECT source_path FROM series_sources WHERE provider_id = $1 AND source_path = ANY($2)",
        provider_id.as_uuid(),
        &paths,
    )
    .fetch_all(pool)
    .await?
    .into_iter()
    .collect();

    let fresh: Vec<(&str, &str)> = entries
        .iter()
        .copied()
        .filter(|(path, _)| !known.contains(*path))
        .collect();

    let mut registered = 0usize;
    for chunk in fresh.chunks(STUB_CHUNK) {
        match register_chunk(pool, provider_id, chunk, matching).await {
            Ok(n) => registered += n,
            Err(e) => {
                tracing::warn!(
                    entries = chunk.len(),
                    error = %e,
                    "batched series registration failed; retrying entry by entry"
                );
                for (path, title) in chunk {
                    match register_source_stub(pool, provider_id, path, title, matching).await {
                        Ok(()) => registered += 1,
                        Err(e) => {
                            tracing::warn!(path = %path, error = %e, "series registration failed");
                        }
                    }
                }
            }
        }
    }
    Ok(registered)
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
                    (SELECT count(DISTINCT ss.provider_id) FROM series_sources ss WHERE ss.series_id = s.id) AS \"source_count!\" \
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
                    (SELECT count(DISTINCT ss.provider_id) FROM series_sources ss WHERE ss.series_id = s.id) AS \"source_count!\" \
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

/// How the Discover grid is ordered.
///
/// A closed enum rather than the `Option<String>` this used to be: an unrecognised token
/// silently fell back to `updated`, so a typo in a client produced a page that looked right
/// and was ordered wrong. The handler parses this and answers `400` instead.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SeriesSort {
    /// Most recently updated first. The default, the overwhelmingly common case, and the
    /// only one with a dedicated index (`series_updated_idx`).
    #[default]
    Updated,
    Title,
    Chapters,
    Sources,
    Year,
    /// Accepted, and ordered by recency. There is no rating column yet; the token is in the
    /// design and the frontend's sort control offers it, so refusing it would break a visible
    /// control for no gain. Ordering by recency is the honest fallback — and it is declared
    /// here rather than hidden in a `_ =>` arm, so adding the column later is a change to one
    /// obvious place.
    Rating,
}

impl SeriesSort {
    /// The wire token, which is also the value bound into the `ORDER BY` `CASE` expressions.
    #[must_use]
    pub fn as_token(self) -> &'static str {
        match self {
            Self::Updated => "updated",
            Self::Title => "title",
            Self::Chapters => "chapters",
            Self::Sources => "sources",
            Self::Year => "year",
            Self::Rating => "rating",
        }
    }

    /// Whether this order is served by the dedicated recency statement.
    fn is_recency(self) -> bool {
        matches!(self, Self::Updated | Self::Rating)
    }
}

impl std::str::FromStr for SeriesSort {
    type Err = ParseSeriesSortError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "updated" => Ok(Self::Updated),
            "title" => Ok(Self::Title),
            "chapters" => Ok(Self::Chapters),
            "sources" => Ok(Self::Sources),
            "year" => Ok(Self::Year),
            "rating" => Ok(Self::Rating),
            other => Err(ParseSeriesSortError(other.to_owned())),
        }
    }
}

/// Raised when a client asks for a sort order that does not exist.
#[derive(Debug, Clone, thiserror::Error)]
#[error("unknown sort order: {0:?}")]
pub struct ParseSeriesSortError(pub String);

/// Server-side filter/sort/paginate criteria for the Discover grid (frontend §9.1).
///
/// Every field is optional; `None`/empty means "no constraint". The enum filters bind as
/// their native Postgres enum types rather than as text: `s.content_type::text = $2` cast the
/// column away from any index it could use, and it let an unparseable token through to match
/// nothing instead of being refused at the edge. `tags` requires the series to carry **all**
/// listed slugs; `exclude_tags` removes any series carrying **any** listed slug.
#[derive(Debug, Default, Clone)]
pub struct SeriesFilter {
    pub query: Option<String>,
    pub content_type: Option<ContentType>,
    pub status: Option<SeriesStatus>,
    pub provider_slug: Option<String>,
    pub tags: Vec<String>,
    pub exclude_tags: Vec<String>,
    pub year_min: Option<i32>,
    pub year_max: Option<i32>,
    pub min_chapters: Option<i32>,
    pub sort: SeriesSort,
    pub limit: i64,
    pub offset: i64,
}

/// A page of the filtered browse list plus the total number of matching rows, so the API can
/// render `{ items, total, next_cursor }`.
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
}

/// Query the browse list with server-side filtering, sorting and offset pagination
/// (frontend §9.1). Returns the page plus the total match count for the pager.
///
/// # Why this is two statements — not five, and not one
///
/// It was **five** near-identical `query_as!` blocks differing only in `ORDER BY`, each
/// repeating the same nine-predicate `WHERE` and eleven binds. Adding a filter meant five
/// identical edits, and any one of them drifting produced a filter that applied under four
/// sort orders and not the fifth — on a predicate set that includes provider scoping and tag
/// exclusion. The shared text cannot be factored into a constant: `sqlx`'s compile-time
/// macros need a string *literal* and do not expand `concat!` (checked, not assumed).
///
/// It is not one statement either. Folding all six orders into bound `CASE` expressions does
/// work — Postgres constant-folds the inactive branches under a custom plan and still reaches
/// `series_title_sort_idx` for `title` — but it puts two correlated aggregate subqueries
/// (`chapters`, `sources`) into the sort key of *every* browse request, and under a generic
/// plan the default order loses `series_updated_idx` and falls back to a parallel seq scan.
/// The default order is the unauthenticated, highest-traffic route in the product, so it gets
/// its own statement and its plan cannot regress. Measured on the 78 850-row development
/// catalogue: indexed recency 2.9 ms; the `CASE` form under a forced generic plan 79 ms.
///
/// So: **one statement for recency, one for everything else.** Two copies of the `WHERE`
/// clause is a real cost; it is the smallest one available without giving up either
/// compile-time checking or the index on the hot path.
///
/// # Why the total is a separate query
///
/// The page query used to carry `count(*) OVER()`. A window function with no `PARTITION BY`
/// is evaluated over the whole result set, so Postgres materialised *every* matching row —
/// including the per-row `source_count` subquery — before the `LIMIT` could take 40. On the
/// development catalogue that meant a full sequential scan, a top-N sort and 66 MB spilled to
/// disk to return 40 rows: 179 ms. Counting separately lets the page query stop at `LIMIT`
/// and lets the count run as an index-only scan, and the two are issued concurrently so the
/// pair costs one round trip. Same catalogue, same filters: 179 ms → ~5 ms.
///
/// Takes `&PgPool` rather than a generic executor precisely so the two can overlap.
pub async fn list_series_filtered(pool: &PgPool, filter: &SeriesFilter) -> DbResult<SeriesPage> {
    let query = filter
        .query
        .as_deref()
        .map(str::trim)
        .filter(|q| !q.is_empty());

    let (rows, total) = tokio::try_join!(
        fetch_filtered_page(pool, filter, query),
        count_filtered(pool, filter, query),
    )?;

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

/// One page of matching rows, in the requested order.
///
/// Both statements end with `s.id DESC`. Without a tiebreaker, rows sharing the leading sort
/// key had no defined order, so two adjacent `OFFSET` pages could repeat one row and skip
/// another; it is also what makes the recency order match `series_updated_idx
/// (updated_at DESC, id DESC)` exactly.
async fn fetch_filtered_page(
    pool: &PgPool,
    filter: &SeriesFilter,
    query: Option<&str>,
) -> DbResult<Vec<FilteredRow>> {
    if filter.sort.is_recency() {
        fetch_page_by_recency(pool, filter, query).await
    } else {
        fetch_page_by_sort_token(pool, filter, query).await
    }
}

/// The default order: newest-updated first, straight down `series_updated_idx`.
async fn fetch_page_by_recency(
    pool: &PgPool,
    filter: &SeriesFilter,
    query: Option<&str>,
) -> DbResult<Vec<FilteredRow>> {
    let rows = sqlx::query_as!(
            FilteredRow,
            "SELECT s.id, s.canonical_title, s.normalized_title, s.description, s.cover_url, \
                    s.content_type AS \"content_type: ContentType\", \
                    s.status AS \"status: SeriesStatus\", s.release_year, \
                    s.created_at, s.updated_at, \
                    (SELECT count(DISTINCT ss.provider_id) FROM series_sources ss WHERE ss.series_id = s.id) AS \"source_count!\" \
             FROM series s \
             WHERE ($1::text IS NULL OR s.normalized_title % $1 \
                     OR s.search_vec @@ plainto_tsquery('simple', $1) \
                     OR EXISTS (SELECT 1 FROM series_titles st \
                                WHERE st.series_id = s.id AND st.normalized % $1)) \
               AND ($2::content_type IS NULL OR s.content_type = $2) \
               AND ($3::series_status IS NULL OR s.status = $3) \
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
             ORDER BY s.updated_at DESC, s.id DESC \
             LIMIT $10 OFFSET $11",
            query,
            filter.content_type as Option<ContentType>,
            filter.status as Option<SeriesStatus>,
            filter.year_min,
            filter.year_max,
            filter.provider_slug.as_deref(),
            filter.min_chapters,
            &filter.tags as &[String],
            &filter.exclude_tags as &[String],
            filter.limit,
            filter.offset,
        )
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// Every other order, selected by the bound sort token.
///
/// Postgres constant-folds the inactive `CASE` branches, so `title` still reaches
/// `series_title_sort_idx` and the two correlated aggregates are never evaluated unless the
/// order that needs them was asked for.
async fn fetch_page_by_sort_token(
    pool: &PgPool,
    filter: &SeriesFilter,
    query: Option<&str>,
) -> DbResult<Vec<FilteredRow>> {
    let rows = sqlx::query_as!(
            FilteredRow,
            "SELECT s.id, s.canonical_title, s.normalized_title, s.description, s.cover_url, \
                    s.content_type AS \"content_type: ContentType\", \
                    s.status AS \"status: SeriesStatus\", s.release_year, \
                    s.created_at, s.updated_at, \
                    (SELECT count(DISTINCT ss.provider_id) FROM series_sources ss WHERE ss.series_id = s.id) AS \"source_count!\" \
             FROM series s \
             WHERE ($1::text IS NULL OR s.normalized_title % $1 \
                     OR s.search_vec @@ plainto_tsquery('simple', $1) \
                     OR EXISTS (SELECT 1 FROM series_titles st \
                                WHERE st.series_id = s.id AND st.normalized % $1)) \
               AND ($2::content_type IS NULL OR s.content_type = $2) \
               AND ($3::series_status IS NULL OR s.status = $3) \
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
             ORDER BY \
               CASE WHEN $12 = 'title' THEN s.canonical_title END ASC NULLS LAST, \
               CASE WHEN $12 = 'year' THEN s.release_year END DESC NULLS LAST, \
               CASE WHEN $12 = 'chapters' THEN ( \
                     SELECT COALESCE(sum(ss.chapter_count),0)::int8 FROM series_sources ss \
                     WHERE ss.series_id = s.id) END DESC NULLS LAST, \
               CASE WHEN $12 = 'sources' THEN ( \
                     SELECT count(DISTINCT ss.provider_id)::int8 FROM series_sources ss \
                     WHERE ss.series_id = s.id) END DESC NULLS LAST, \
               s.updated_at DESC, s.id DESC \
             LIMIT $10 OFFSET $11",
            query,
            filter.content_type as Option<ContentType>,
            filter.status as Option<SeriesStatus>,
            filter.year_min,
            filter.year_max,
            filter.provider_slug.as_deref(),
            filter.min_chapters,
            &filter.tags as &[String],
            &filter.exclude_tags as &[String],
            filter.limit,
            filter.offset,
            filter.sort.as_token(),
    )
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// How many rows match the filter, ignoring `limit`/`offset`.
///
/// The predicate list is [`fetch_filtered_page`]'s and must stay that way — a total that
/// disagrees with the page is a pager offering a page that comes back empty.
async fn count_filtered(
    pool: &PgPool,
    filter: &SeriesFilter,
    query: Option<&str>,
) -> DbResult<i64> {
    let total = sqlx::query_scalar!(
        "SELECT count(*) AS \"total!\" FROM series s \
         WHERE ($1::text IS NULL OR s.normalized_title % $1 \
                 OR s.search_vec @@ plainto_tsquery('simple', $1) \
                 OR EXISTS (SELECT 1 FROM series_titles st \
                            WHERE st.series_id = s.id AND st.normalized % $1)) \
           AND ($2::content_type IS NULL OR s.content_type = $2) \
           AND ($3::series_status IS NULL OR s.status = $3) \
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
                 WHERE stg.series_id = s.id AND t.slug = ANY($9::text[])))",
        query,
        filter.content_type as Option<ContentType>,
        filter.status as Option<SeriesStatus>,
        filter.year_min,
        filter.year_max,
        filter.provider_slug.as_deref(),
        filter.min_chapters,
        &filter.tags as &[String],
        &filter.exclude_tags as &[String],
    )
    .fetch_one(pool)
    .await?;
    Ok(total)
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
pub async fn ingest_series(
    pool: &sqlx::PgPool,
    scanned: ScannedSeries,
    matching: &MatchingConfig,
) -> DbResult<IngestOutcome> {
    let mut tx = pool.begin().await?;

    let series_id = resolve_canonical_series(&mut tx, &scanned.meta, matching).await?;
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

    // One statement, not one per chapter. A series with two thousand chapters used to mean
    // two thousand sequential round trips inside this transaction — which also holds row
    // locks on the shared `tags`/`authors` rows, so a single slow series stalled every other
    // provider's ingest behind it.
    let mut new_chapters = upsert_chapters(&mut *tx, source_id, &scanned.chapters).await?;
    // The row-at-a-time loop emitted these in listing order; `RETURNING` does not promise
    // any order. `chapter.discovered` consumers do not depend on it, but a stable order
    // keeps the notification stream and the tests deterministic.
    new_chapters.sort_by(f64::total_cmp);

    let count = i32::try_from(scanned.chapters.len()).unwrap_or(i32::MAX);
    update_source_scan(&mut *tx, source_id, &scanned.content_hash, count).await?;

    tx.commit().await?;
    Ok(IngestOutcome {
        series_id,
        source_id,
        new_chapters,
    })
}

#[cfg(test)]
mod sort_tests {
    use super::SeriesSort;
    use std::str::FromStr as _;

    /// Every token the API accepts must round-trip, and an unknown one must be an error
    /// rather than a silent fallback.
    ///
    /// The bug this pins: `SeriesFilter::sort` was an `Option<String>` matched with a
    /// trailing `_ =>` arm, so `?sort=titel` returned a `200` ordered by recency. It also
    /// pins the token strings themselves, which are bound straight into the `ORDER BY`
    /// `CASE` expressions — renaming one there without renaming it here would silently
    /// disable that sort order.
    #[test]
    fn every_sort_token_round_trips_and_unknown_is_refused() {
        for sort in [
            SeriesSort::Updated,
            SeriesSort::Title,
            SeriesSort::Chapters,
            SeriesSort::Sources,
            SeriesSort::Year,
            SeriesSort::Rating,
        ] {
            assert_eq!(SeriesSort::from_str(sort.as_token()).unwrap(), sort);
        }
        assert!(SeriesSort::from_str("titel").is_err());
        assert!(SeriesSort::from_str("").is_err());
    }

    /// Only the two orders the dedicated, index-backed recency statement can serve may claim
    /// it. If `Title` ever answered `true` here it would be ordered by `updated_at` with no
    /// error anywhere.
    #[test]
    fn only_recency_orders_use_the_indexed_statement() {
        assert!(SeriesSort::Updated.is_recency());
        assert!(SeriesSort::Rating.is_recency());
        for sort in [
            SeriesSort::Title,
            SeriesSort::Chapters,
            SeriesSort::Sources,
            SeriesSort::Year,
        ] {
            assert!(
                !sort.is_recency(),
                "{} must not use recency",
                sort.as_token()
            );
        }
    }
}
