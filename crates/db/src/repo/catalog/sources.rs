//! Provider sources: the per-provider rows attached to a canonical series, their scan
//! bookkeeping, and the stub registration a catalogue crawl performs before any deep scan.

use super::series::{Resolution, SeriesUpsert, resolve_canonical_series};
use crate::error::{DbError, DbResult};
use sqlx::{FromRow, PgExecutor};
use tankovault_domain::matching::Canonicaliser;
use tankovault_domain::{
    ContentType, ProviderId, ProviderState, SeriesId, SeriesSource, SeriesSourceId, SeriesStatus,
    normalize_title,
};
use time::OffsetDateTime;
use uuid::Uuid;

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

/// Returns the series the `(provider, path)` source is filed under, `None` when it is not
/// registered.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only.
pub async fn attached_series<'e, E: PgExecutor<'e>>(
    exec: E,
    provider_id: ProviderId,
    source_path: &str,
) -> DbResult<Option<SeriesId>> {
    let id = sqlx::query_scalar!(
        "SELECT series_id FROM series_sources WHERE provider_id = $1 AND source_path = $2",
        provider_id.as_uuid(),
        source_path,
    )
    .fetch_optional(exec)
    .await?;
    Ok(id.map(SeriesId::from_uuid))
}

/// Upserts the (provider, path) source for a series. Idempotent on `(provider_id, source_path)`.
///
/// On conflict the existing row's id comes back, so a source keeps its identity across rescans,
/// and `series_id` is not updated, so an attached source is never re-pointed. The second
/// element is the series the row is filed under; a caller that just created `series_id` must
/// compare the two, because on a mismatch its new series holds no source.
///
/// An unknown `series_id` or `provider_id` is a foreign-key violation.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only.
pub async fn upsert_source<'e, E: PgExecutor<'e>>(
    exec: E,
    series_id: SeriesId,
    provider_id: ProviderId,
    source_path: &str,
    provider_title: Option<&str>,
) -> DbResult<(SeriesSourceId, SeriesId)> {
    let row = sqlx::query!(
        "INSERT INTO series_sources \
            (id, series_id, provider_id, source_path, provider_title, provider_title_normalized) \
         VALUES ($1,$2,$3,$4,$5,$6) \
         ON CONFLICT (provider_id, source_path) DO UPDATE \
            SET provider_title = EXCLUDED.provider_title, \
                provider_title_normalized = EXCLUDED.provider_title_normalized \
         RETURNING id, series_id",
        SeriesSourceId::new().as_uuid(),
        series_id.as_uuid(),
        provider_id.as_uuid(),
        source_path,
        provider_title,
        provider_title.map(normalize_title),
    )
    .fetch_one(exec)
    .await?;
    Ok((
        SeriesSourceId::from_uuid(row.id),
        SeriesId::from_uuid(row.series_id),
    ))
}

/// Ensures a series **source** row exists for a catalogue entry.
///
/// Creates a canonical series from the listing title when the source is new, and does nothing
/// when it exists, so it never downgrades metadata a later `Series` task enriched. The result
/// does not say which happened; [`register_source_stubs`] counts.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only.
pub async fn register_source_stub(
    pool: &sqlx::PgPool,
    provider_id: ProviderId,
    source_path: &str,
    title: &str,
    canonicaliser: &dyn Canonicaliser,
) -> DbResult<()> {
    let mut tx = pool.begin().await?;

    // Already registered by this or an earlier scan: leave the enriched row untouched.
    if attached_series(&mut *tx, provider_id, source_path)
        .await?
        .is_some()
    {
        return Ok(());
    }

    let series_id = resolve_stub_series(&mut tx, title, canonicaliser)
        .await?
        .series_id;
    let (_, filed_under) =
        upsert_source(&mut *tx, series_id, provider_id, source_path, Some(title)).await?;
    if filed_under != series_id {
        // A concurrent registration won; dropping `tx` discards the series this one created.
        return Ok(());
    }

    tx.commit().await?;
    Ok(())
}

/// Canonicalises a catalogue listing title into a series id, creating one if nothing matches.
///
/// The stub carries no description, cover, type, status or year, so it never blanks what a
/// later `Series` task enriched.
async fn resolve_stub_series(
    conn: &mut sqlx::PgConnection,
    title: &str,
    canonicaliser: &dyn Canonicaliser,
) -> DbResult<Resolution> {
    let meta = SeriesUpsert {
        canonical_title: title.to_owned(),
        normalized_title: normalize_title(title),
        description: None,
        cover_url: None,
        content_type: ContentType::Unknown,
        status: SeriesStatus::Unknown,
        release_year: None,
    };
    resolve_canonical_series(conn, &meta, canonicaliser).await
}

/// Upserts several `(provider, path)` sources in one statement. Idempotent on `(provider_id,
/// source_path)`, like [`upsert_source`].
///
/// `DISTINCT ON (source_path)` is required: a catalogue page can list the same path twice, and
/// `ON CONFLICT DO UPDATE` cannot touch one row twice in a single statement.
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
    // Borrowed, as in `register_source_stubs`: the `UNNEST` arrays live only for this
    // statement, so cloning each path and title would be waste.
    let paths: Vec<&str> = rows.iter().map(|(_, p, _)| p.as_str()).collect();
    let titles: Vec<&str> = rows.iter().map(|(_, _, t)| t.as_str()).collect();
    let normalized: Vec<String> = titles.iter().map(|t| normalize_title(t)).collect();
    sqlx::query!(
        "INSERT INTO series_sources \
            (id, series_id, provider_id, source_path, provider_title, provider_title_normalized) \
         SELECT DISTINCT ON (source_path) id, series_id, $2, source_path, provider_title, \
                provider_title_normalized \
         FROM UNNEST($1::uuid[], $3::uuid[], $4::text[], $5::text[], $6::text[]) \
              AS t(id, series_id, source_path, provider_title, provider_title_normalized) \
         ON CONFLICT (provider_id, source_path) DO UPDATE \
            SET provider_title = EXCLUDED.provider_title, \
                provider_title_normalized = EXCLUDED.provider_title_normalized",
        &ids,
        provider_id.as_uuid(),
        &series_ids,
        &paths as _,
        &titles as _,
        &normalized,
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// How many new stubs share one transaction. Sized so the begin/commit and the source insert
/// amortise to near-nothing per entry while a chunk still holds its locks for a bounded time.
const STUB_CHUNK: usize = 500;

/// Registers one chunk of genuinely-new entries in a single transaction.
///
/// Canonicalisation cannot be batched, since each entry resolves against the series its
/// predecessors created inside the transaction, but the `upsert_source` tail can. A chunk costs
/// `begin + N canonicalisations + 1 insert + commit` instead of
/// `N × (begin + check + canonicalisation + insert + commit)`.
async fn register_chunk(
    pool: &sqlx::PgPool,
    provider_id: ProviderId,
    chunk: &[(&str, &str)],
    canonicaliser: &dyn Canonicaliser,
) -> DbResult<usize> {
    let mut tx = pool.begin().await?;
    let mut sources = Vec::with_capacity(chunk.len());
    let mut created = Vec::new();
    for (path, title) in chunk {
        // Per entry, inside the transaction: entry N must be able to match the series entry N-1
        // just created.
        let resolved = resolve_stub_series(&mut tx, title, canonicaliser).await?;
        if resolved.created {
            created.push(resolved.series_id.as_uuid());
        }
        sources.push((resolved.series_id, (*path).to_owned(), (*title).to_owned()));
    }
    upsert_sources(&mut tx, provider_id, &sources).await?;
    discard_unsourced(&mut tx, &created).await?;
    tx.commit().await?;
    Ok(chunk.len())
}

/// Deletes each series in `created` that holds no source.
///
/// `upsert_sources` keeps a path's existing series on conflict, so a path a concurrent scan
/// registered between the batch existence check and this chunk lands on that scan's series, and
/// the series this chunk created for it is left with nothing, an orphan no scan will reach.
/// Only series this chunk created are considered, and only empty ones, because a later entry of
/// the same chunk may have attached to one.
async fn discard_unsourced(conn: &mut sqlx::PgConnection, created: &[Uuid]) -> DbResult<()> {
    if created.is_empty() {
        return Ok(());
    }
    let discarded = sqlx::query!(
        "DELETE FROM series s WHERE s.id = ANY($1) \
            AND NOT EXISTS (SELECT 1 FROM series_sources ss WHERE ss.series_id = s.id)",
        created,
    )
    .execute(conn)
    .await?
    .rows_affected();
    if discarded > 0 {
        tracing::debug!(
            discarded,
            "discarded series a concurrent registration made redundant"
        );
    }
    Ok(())
}

/// Registers a whole catalogue page's worth of entries, skipping those already known.
///
/// The same as [`register_source_stub`] per entry, but set-based where it can be: the "already
/// registered?" check, the only work most entries need on a rescan, is one query for the whole
/// batch, and the new remainder is registered `STUB_CHUNK` entries per transaction.
///
/// The chunking matters for a **first** scan, which is all new: one transaction per entry would
/// cost 20 000 transactions of about 5 round trips for a sitemap page, blowing the `JetStream`
/// ack deadline and causing redelivery of the very scans that matter most.
///
/// A source a concurrent scan registers between the batch check and the chunk keeps the series
/// that scan filed it under, since `upsert_sources` never re-points, and the series this chunk
/// created for it is deleted before commit (`discard_unsourced`).
///
/// Returns the number of sources newly registered. A failed chunk is retried entry by entry,
/// and an entry that fails again is logged at `warn` and left out of the count, so losing one
/// series does not lose the page (`docs/ARCHITECTURE.md` §12). The count is therefore the only
/// failure signal: `0` on a page of new entries is a failure report.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only, from the batch existence check.
pub async fn register_source_stubs(
    pool: &sqlx::PgPool,
    provider_id: ProviderId,
    entries: &[(&str, &str)],
    canonicaliser: &dyn Canonicaliser,
) -> DbResult<usize> {
    if entries.is_empty() {
        return Ok(0);
    }

    // Borrowed: `sqlx` encodes `&[&str]` as `text[]` like `&[String]`, and a catalogue page can
    // be 20 000 entries, each an allocation discarded within one statement.
    let paths: Vec<&str> = entries.iter().map(|(path, _)| *path).collect();
    let known: std::collections::HashSet<String> = sqlx::query_scalar!(
        "SELECT source_path FROM series_sources WHERE provider_id = $1 AND source_path = ANY($2)",
        provider_id.as_uuid(),
        &paths as _,
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
        match register_chunk(pool, provider_id, chunk, canonicaliser).await {
            Ok(n) => registered += n,
            Err(e) => {
                tracing::warn!(
                    entries = chunk.len(),
                    error = %e,
                    "batched series registration failed; retrying entry by entry"
                );
                for (path, title) in chunk {
                    match register_source_stub(pool, provider_id, path, title, canonicaliser).await
                    {
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

/// Records the result of a source scan: content hash, chapter count and timestamp.
///
/// A missing `source_id` matches nothing. The only caller writes inside the ingest transaction
/// that created the source; elsewhere a no-op would only make the next scan redo work.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only.
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

/// Returns the stored content hash for a source, if any, for the change-detection skip.
///
/// No such source and a source never scanned are both `None`: there is no hash, so scan.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only.
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

/// Lists the sources of a canonical series, for the "Read on: A · B · C" strip.
///
/// An unknown `series_id` and a series no provider carries are the same empty list: the series
/// page establishes that the series exists, and this only decides whether the strip renders.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only.
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
/// Returns the `(provider_id, base_url)` a source belongs to, which the API needs to resolve a
/// source's relative paths into absolute links.
///
/// The absent case is an error, unlike most of this crate, because every caller is resolving a
/// link it has committed to rendering and there is no partial answer.
///
/// # Errors
/// [`crate::DbError::NotFound`] when `source_id` names no source or its provider is gone;
/// otherwise [`crate::DbError::Sqlx`].
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
