//! Metadata enrichment: the sweep's work list, folding resolved upstream metadata into a
//! series, and the alternative-title / tag / author link tables it writes.

use crate::error::DbResult;
use sqlx::{FromRow, PgExecutor};
use tankovault_domain::SeriesId;
use time::OffsetDateTime;
use uuid::Uuid;

/// A minimal series row for the tokenless metadata-enrichment worker: enough to look the
/// work up upstream (by mapped external id or by title) and to feed the metadata-priority
/// resolver the current locally-scraped values.
pub struct SeriesEnrichmentRow {
    pub id: SeriesId,
    pub canonical_title: String,
    pub description: Option<String>,
    pub cover_url: Option<String>,
}

/// The shared row shape behind the two enrichment work-list queries.
#[derive(FromRow)]
struct EnrichmentRow {
    id: Uuid,
    canonical_title: String,
    description: Option<String>,
    cover_url: Option<String>,
}

impl From<EnrichmentRow> for SeriesEnrichmentRow {
    fn from(r: EnrichmentRow) -> Self {
        Self {
            id: SeriesId::from_uuid(r.id),
            canonical_title: r.canonical_title,
            description: r.description,
            cover_url: r.cover_url,
        }
    }
}

/// One page of series for the background metadata-enrichment sweep, least-recently-attempted
/// first (never-attempted series lead).
///
/// `started_at` is the timestamp the sweep began, and it is the *only* thing that stops a page
/// being handed back: every row this returns must have `metadata_checked_at` stamped with a
/// `now()` before the next page is asked for, which then fails this predicate. That is why the
/// caller stamps a series it could not resolve, and one whose lookup errored, exactly as it
/// stamps a success — an unstamped row would lead every subsequent page and the sweep would
/// spin on it for its whole per-run budget.
///
/// The predicate replaces two earlier shapes, both of which silently lost series:
///
/// - `ORDER BY updated_at ASC LIMIT $1 OFFSET $2`, where enrichment *writes* `updated_at`: every
///   enriched row jumped to the end of the ordering, the rows behind it shifted forward, and the
///   next `OFFSET` skipped exactly those.
/// - The keyset walk that replaced it, which fixed the skipping but still ordered by a column
///   only a *successful* enrichment wrote. A series no provider resolved kept its old
///   `updated_at` and led the ordering again on the next sweep, so a catalogue holding more
///   unresolvable series than the per-run cap never advanced past them.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only — no other variant is reachable. An exhausted sweep is an empty
/// `Vec`, not [`crate::DbError::NotFound`], and that emptiness is the *only* termination signal
/// the caller has: a failure defaulted to an empty page would end the sweep early while
/// reporting that the whole catalogue had been walked.
pub async fn list_series_for_enrichment<'e, E: PgExecutor<'e>>(
    exec: E,
    limit: i64,
    started_at: OffsetDateTime,
) -> DbResult<Vec<SeriesEnrichmentRow>> {
    let rows = sqlx::query_as!(
        EnrichmentRow,
        "SELECT id, canonical_title, description, cover_url FROM series \
         WHERE metadata_checked_at IS NULL OR metadata_checked_at < $2 \
         ORDER BY metadata_checked_at ASC NULLS FIRST, id ASC \
         LIMIT $1",
        limit,
        started_at,
    )
    .fetch_all(exec)
    .await?;
    Ok(rows.into_iter().map(Into::into).collect())
}

/// Which of `series_ids` have not had their metadata attempted since `stale_before`, with the
/// current locally-stored values the priority resolver needs.
///
/// The list-reconciliation counterpart to [`list_series_for_enrichment`]: a linked account's
/// sync already knows the upstream id of every series it matched, so it can fold that metadata
/// in immediately instead of waiting for a catalogue-wide sweep to reach the row. One statement
/// for the whole matched set, not one per entry — a library is hundreds of entries and this runs
/// on every scheduled reconciliation.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only. Ids that do not exist simply do not come back.
pub async fn series_needing_metadata<'e, E: PgExecutor<'e>>(
    exec: E,
    series_ids: &[Uuid],
    stale_before: OffsetDateTime,
) -> DbResult<Vec<SeriesEnrichmentRow>> {
    let rows = sqlx::query_as!(
        EnrichmentRow,
        "SELECT id, canonical_title, description, cover_url FROM series \
         WHERE id = ANY($1::uuid[]) \
           AND (metadata_checked_at IS NULL OR metadata_checked_at < $2)",
        series_ids,
        stale_before,
    )
    .fetch_all(exec)
    .await?;
    Ok(rows.into_iter().map(Into::into).collect())
}

/// Record that a series was examined by an enrichment pass that had nothing to write — no
/// provider resolved it, or the lookup failed.
///
/// Deliberately does **not** touch `updated_at`: a lookup that found nothing is not a change to
/// the series, and every catalogue listing ordered by `updated_at` would otherwise be reshuffled
/// by the failures of a background sweep.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only. A `series_id` that no longer exists matches nothing and is
/// still `Ok(())` — the sweep reads its work list from `series`, so a row erased between the two
/// is a no-op rather than an error.
pub async fn mark_metadata_checked<'e, E: PgExecutor<'e>>(
    exec: E,
    series_id: SeriesId,
) -> DbResult<()> {
    sqlx::query!(
        "UPDATE series SET metadata_checked_at = now() WHERE id = $1",
        series_id.as_uuid(),
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// A batch of metadata to fold into an existing series (the tokenless enrichment worker's
/// output). `description`/`cover_url` are already the values chosen by the metadata-priority
/// resolver; a `None` leaves the current value untouched. Titles/tags/authors are additive.
pub struct MetadataEnrichment<'a> {
    pub description: Option<&'a str>,
    pub cover_url: Option<&'a str>,
    /// Content-type token (e.g. `manga`/`manhwa`); only fills a currently-`unknown` series.
    pub content_type: Option<&'a str>,
    /// Publication-status token (e.g. `ongoing`/`completed`); only fills a currently-`unknown`
    /// series, for the same reason as `content_type` — a provider page that states a status
    /// outranks an upstream catalogue that may lag a chapter behind it.
    pub status: Option<&'a str>,
    /// Release year; only fills a series whose year is currently null.
    pub release_year: Option<i32>,
    pub alt_titles: &'a [(String, String)],
    pub tags: &'a [String],
    pub authors: &'a [String],
}

/// Apply an enrichment batch to a series in one transaction: overwrite description/cover
/// (priority already applied by the caller), gap-fill content type, publication status and
/// release year, and additively record alternative titles, tags and authors. Idempotent —
/// re-running converges to the same rows.
///
/// `metadata_checked_at` is stamped here as well as by [`mark_metadata_checked`], so a
/// successful enrichment and a fruitless lookup both take the series out of the sweep's work
/// list. See [`list_series_for_enrichment`] for why that is load-bearing rather than tidy.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only — no other variant is reachable, and one of its shapes is worth
/// naming because it is the only *input*-driven failure in this module: `content_type`/`status`
/// are cast `text::content_type`/`text::series_status` in the statement, so a token the Postgres
/// enum does not carry is an invalid-input error from the driver rather than a silently skipped
/// field. That is the intended trade — an upstream vocabulary this build does not model should
/// stop the batch, not half-apply it.
///
/// A `series_id` that no longer exists matches nothing and is still `Ok(())`: the sweep reads
/// its work list from `series`, so a row erased between the two is a no-op rather than an error,
/// and the whole enrichment is one transaction, so nothing lands half-applied either way.
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
            status = CASE WHEN status = 'unknown' \
                          THEN COALESCE($6::text::series_status, status) \
                          ELSE status END, \
            release_year = COALESCE(release_year, $5), \
            updated_at = now(), \
            metadata_checked_at = now() \
         WHERE id = $1",
        series_id.as_uuid(),
        enrichment.description,
        enrichment.cover_url,
        enrichment.content_type,
        enrichment.release_year,
        enrichment.status,
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
///
/// # Errors
/// [`crate::DbError::Sqlx`] only — no other variant is reachable; a `series_id` that does not
/// exist is a foreign-key violation and so a 500. A `titles` slice that is empty, or whose every
/// entry has an empty `normalized`, returns `Ok(())` without issuing a statement — silently, and
/// deliberately: a provider page listing no alternative titles is the common case, not a fault.
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
///
/// # Errors
/// [`crate::DbError::Sqlx`] only — no other variant is reachable; an unknown `series_id` is a
/// foreign-key violation. Names that slugify to nothing are dropped rather than rejected, so a
/// tag list of pure punctuation is `Ok(())` with nothing written — the two statements are
/// skipped entirely when that leaves the list empty.
///
/// The two statements are **not** wrapped in a transaction of their own; they inherit the
/// caller's `conn`, and both callers already hold one. Reaching this with an autocommit
/// connection would make a failure between them leave the `tags` rows created and unlinked —
/// harmless, since the second statement resolves ids by slug and a later run completes the link.
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
///
/// # Errors
/// [`crate::DbError::Sqlx`] only — no other variant is reachable. Identical in every respect to
/// [`add_series_tags`], including the empty-after-slugify no-op and the note about the two
/// statements sharing the caller's connection.
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
