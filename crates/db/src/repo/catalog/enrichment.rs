//! Metadata enrichment: the sweep's work list, folding resolved upstream metadata into a
//! series, and the alternative-title / tag / author link tables it writes.

use crate::error::DbResult;
use sqlx::{FromRow, PgExecutor};
use tankovault_domain::SeriesId;
use time::OffsetDateTime;
use uuid::Uuid;

/// Minimal series row for the enrichment worker: enough to look up upstream and feed the
/// priority resolver's current values.
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

/// One page of series for the background enrichment sweep, least-recently-attempted first.
///
/// Every row returned must get `metadata_checked_at` stamped (success, failure, or unresolved
/// alike) before the next page is asked for, or an unstamped row leads every subsequent page
/// and the sweep spins on it.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only; an exhausted sweep is an empty `Vec` — its only termination
/// signal, so a failure must not be defaulted to empty.
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

/// Which of `series_ids` have not had metadata attempted since `stale_before`, with the current
/// locally-stored values the priority resolver needs. One statement for the whole set.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only; unknown ids are absent from the result.
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

/// Record that a series was examined by an enrichment pass that had nothing to write.
///
/// Does not touch `updated_at` — a no-op lookup must not reshuffle listings ordered by it.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only; an erased `series_id` matches nothing and is still `Ok(())`.
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

/// A batch of metadata to fold into an existing series. `None` leaves a field untouched;
/// titles/tags/authors are additive.
pub struct MetadataEnrichment<'a> {
    pub description: Option<&'a str>,
    pub cover_url: Option<&'a str>,
    /// Content-type token (e.g. `manga`/`manhwa`); only fills a currently-`unknown` series.
    pub content_type: Option<&'a str>,
    /// Publication-status token; only fills a currently-`unknown` series, same reason as
    /// `content_type`.
    pub status: Option<&'a str>,
    /// Release year; only fills a series whose year is currently null.
    pub release_year: Option<i32>,
    pub alt_titles: &'a [(String, String)],
    pub tags: &'a [String],
    pub authors: &'a [String],
}

/// Apply an enrichment batch to a series in one transaction: overwrite description/cover,
/// gap-fill content type/status/year, and additively record titles/tags/authors. Idempotent.
///
/// Also stamps `metadata_checked_at`, so a successful enrichment leaves the sweep's work list
/// the same way a fruitless lookup does (see [`list_series_for_enrichment`]).
///
/// # Errors
/// [`crate::DbError::Sqlx`] only; an unrecognised `content_type`/`status` token errors rather
/// than being silently dropped. An erased `series_id` is a no-op `Ok(())`.
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
/// [`crate::DbError::Sqlx`] only; unknown `series_id` is a foreign-key violation. Empty or
/// all-empty-`normalized` titles is a silent `Ok(())`.
pub async fn add_series_titles(
    conn: &mut sqlx::PgConnection,
    series_id: SeriesId,
    titles: &[(String, String)],
) -> DbResult<()> {
    // One statement, not one per title, to avoid holding ingest-transaction locks longer.
    // De-duplicated on `normalized` first: `ON CONFLICT DO UPDATE` cannot touch one row twice.
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

/// Add genre/tag names to a series (idempotent, additive-only). Empty/unslugifiable names are
/// skipped.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only; unknown `series_id` is a foreign-key violation. All-empty
/// names is `Ok(())`. Statements share the caller's transaction, not their own.
pub async fn add_series_tags(
    conn: &mut sqlx::PgConnection,
    series_id: SeriesId,
    tags: &[String],
) -> DbResult<()> {
    let (slugs, names) = dedup_by_slug(tags);
    if slugs.is_empty() {
        return Ok(());
    }

    // `DO NOTHING`, not `DO UPDATE`: a no-op update would still take a write lock on a
    // globally-shared `tags` row, serializing concurrent ingests of the same genre.
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
/// [`crate::DbError::Sqlx`] only; identical to [`add_series_tags`].
pub async fn add_series_authors(
    conn: &mut sqlx::PgConnection,
    series_id: SeriesId,
    authors: &[String],
) -> DbResult<()> {
    let (slugs, names) = dedup_by_slug(authors);
    if slugs.is_empty() {
        return Ok(());
    }

    // Same reasoning as `add_series_tags`: avoids a write lock on a shared `authors` row.
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
/// De-duplication is required: a slug bound twice in one `UNNEST` insert is harmless, but the
/// link statement would then attach the same `(series_id, tag_id)` pair twice.
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
