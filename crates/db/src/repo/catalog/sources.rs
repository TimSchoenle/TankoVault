//! Provider sources: the per-provider rows attached to a canonical series, their scan
//! bookkeeping, and the stub registration a catalogue crawl performs before any deep scan.

use super::series::{SeriesUpsert, resolve_canonical_series};
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

/// Upsert the (provider, path) source for a series. Idempotent on `(provider_id, source_path)`.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only — no other variant is reachable. An unknown `series_id` or
/// `provider_id` is a foreign-key violation and so a 500. There is no `Option` here and none is
/// needed: `RETURNING id` fires on both the insert and the update arm, so the row always exists
/// by the time this returns.
///
/// The id it returns is not necessarily the one passed in: on the conflict arm the *existing*
/// row's id comes back, so a source keeps its identity across re-scans and the freshly generated
/// [`SeriesSourceId`] is discarded. `series_id` is likewise **not** in the `DO UPDATE` list — a
/// source that has already been attached to a canonical series is never re-pointed here.
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
///
/// # Errors
/// [`crate::DbError::Sqlx`] only — no other variant is reachable. An already-registered source
/// is `Ok(())` with nothing written and the transaction dropped unsent, which is the no-op the
/// paragraph above promises; it is indistinguishable from a fresh registration, so a caller that
/// needs to know whether anything was created must count with [`register_source_stubs`] instead.
pub async fn register_source_stub(
    pool: &sqlx::PgPool,
    provider_id: ProviderId,
    source_path: &str,
    title: &str,
    canonicaliser: &dyn Canonicaliser,
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

    let series_id = resolve_stub_series(&mut tx, title, canonicaliser).await?;
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
    canonicaliser: &dyn Canonicaliser,
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
    resolve_canonical_series(conn, &meta, canonicaliser).await
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
    // Borrowed for the same reason as in `register_source_stubs`: the `UNNEST` arrays live
    // only for this statement, so cloning each path and title is pure waste (PERF-19).
    let paths: Vec<&str> = rows.iter().map(|(_, p, _)| p.as_str()).collect();
    let titles: Vec<&str> = rows.iter().map(|(_, _, t)| t.as_str()).collect();
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
        &paths as _,
        &titles as _,
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
    canonicaliser: &dyn Canonicaliser,
) -> DbResult<usize> {
    let mut tx = pool.begin().await?;
    let mut sources = Vec::with_capacity(chunk.len());
    for (path, title) in chunk {
        // Per entry, inside the transaction, deliberately: the policy is a pure function over
        // values, so the only reason it cannot be lifted out of this loop is the one that
        // matters — entry N must be able to match the series entry N-1 just created (PERF-15).
        let series_id = resolve_stub_series(&mut tx, title, canonicaliser).await?;
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
/// genuinely-new remainder is registered `STUB_CHUNK` entries per transaction.
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
///
/// # Errors
/// [`crate::DbError::Sqlx`] only — no other variant is reachable, and only from the **batch
/// existence check**. Past that point registration failures are deliberately *not* errors: a
/// failed chunk is retried entry by entry, and an entry that fails again is logged at `warn` and
/// omitted from the count. That is the design §12 rule stated above — losing one series must not
/// lose the page — but it means the returned count is the only signal a caller has, so
/// `Ok(0)` on a page of genuinely new entries is a failure report, not an empty page. An empty
/// `entries` short-circuits to `Ok(0)` without touching the database.
pub async fn register_source_stubs(
    pool: &sqlx::PgPool,
    provider_id: ProviderId,
    entries: &[(&str, &str)],
    canonicaliser: &dyn Canonicaliser,
) -> DbResult<usize> {
    if entries.is_empty() {
        return Ok(0);
    }

    // Borrowed, not owned: `sqlx` encodes `&[&str]` as `text[]` just as it does `&[String]`,
    // so copying every path onto the heap bought nothing. A catalogue page can be 20 000
    // entries, which is 20 000 allocations made and discarded within one statement (PERF-19).
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

/// Record the result of a source scan: content hash + chapter count + timestamp.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only — no other variant is reachable. A `source_id` that no longer
/// exists matches nothing and is still `Ok(())`, not [`crate::DbError::NotFound`]. Its only
/// caller writes inside the ingest transaction that created the source, so the row is there; a
/// silent no-op elsewhere would mean the next scan re-does work the hash was meant to skip,
/// which is wasteful rather than wrong.
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
///
/// # Errors
/// [`crate::DbError::Sqlx`] only — no other variant is reachable. Two different situations
/// collapse into one `Ok(None)` — no such source, and a source registered but never scanned —
/// and the `flatten` is what merges them. Both mean the same thing to the caller: there is no
/// hash to compare against, so scan. That makes `None` the safe default and an error the one
/// answer that must not be swallowed into it silently, since defaulting a failure to `Some(hash)`
/// would skip a scan that should have run.
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
///
/// # Errors
/// [`crate::DbError::Sqlx`] only — no other variant is reachable. An unknown `series_id` and a
/// series no provider carries are the same empty `Vec`, not [`crate::DbError::NotFound`]: the
/// series page establishes the series exists, and this only decides whether the "Read on" strip
/// renders. The `try_from` per row cannot currently fail, for the same reason as in
/// [`super::series::get_series`].
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
/// The `(provider_id, base_url)` a source belongs to — the minimal input the API needs
/// to resolve a source's relative paths into absolute links at read time.
///
/// # Errors
/// - [`crate::DbError::NotFound`] if `source_id` names no source. The inner join means a source
///   whose provider row has been deleted reports the same thing, which is correct for the
///   caller's purpose: without a `base_url` there is no way to build the link either way.
/// - [`crate::DbError::Sqlx`] for any driver or connection failure.
///
/// The absent case is an error rather than `Ok(None)` here — unlike most of this crate — because
/// every caller is resolving a link it has already committed to rendering, and there is no
/// sensible partial answer.
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
