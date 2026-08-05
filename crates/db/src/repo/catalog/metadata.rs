//! The one write path for the series fields more than one source can supply.
//!
//! Every source folds its values in through [`merge_metadata`], which resolves each field
//! against the stored value's recorded provenance under the operator's [`MetadataPriority`]
//! and stores the winning source alongside the winning value. A writer that bypasses this and
//! `UPDATE`s a prioritised column directly re-introduces last-writer-wins for that field, which
//! is what the provenance columns exist to end.

use super::enrichment::add_series_titles;
use crate::error::DbResult;
use tankovault_domain::{
    ContentType, MetadataField, MetadataPriority, MetadataSource, SeriesId, SeriesStatus,
    normalize_title,
};

/// What one source offers for the prioritised series fields. `None` is "no opinion", and so is
/// an `Unknown` enum variant or a blank string — see `MetadataValue`.
#[derive(Debug, Default, Clone)]
pub struct MetadataCandidate<'a> {
    pub canonical_title: Option<&'a str>,
    pub description: Option<&'a str>,
    pub cover_url: Option<&'a str>,
    pub content_type: Option<ContentType>,
    pub status: Option<SeriesStatus>,
    pub release_year: Option<i32>,
}

/// The stored side of the merge: the current value of each prioritised field with the source
/// that wrote it.
struct StoredMetadata {
    canonical_title: String,
    description: Option<String>,
    cover_url: Option<String>,
    content_type: ContentType,
    status: SeriesStatus,
    release_year: Option<i32>,
    title_source: Option<MetadataSource>,
    description_source: Option<MetadataSource>,
    cover_source: Option<MetadataSource>,
    content_type_source: Option<MetadataSource>,
    status_source: Option<MetadataSource>,
    release_year_source: Option<MetadataSource>,
}

/// Provenance for a value written before 0032 recorded it.
///
/// `adapter` is the conservative reading: it lets a higher-priority source correct the row on
/// its next pass instead of freezing whatever happened to be stored, and the row converges to
/// real provenance after one pass of each source.
const fn stored_source(recorded: Option<MetadataSource>) -> MetadataSource {
    match recorded {
        Some(s) => s,
        None => MetadataSource::Adapter,
    }
}

/// The winning `(source, value)` for each prioritised field, borrowed from the incoming
/// candidate or the stored row.
struct Winners<'a> {
    title: Option<(MetadataSource, &'a str)>,
    description: Option<(MetadataSource, &'a str)>,
    cover: Option<(MetadataSource, &'a str)>,
    content_type: Option<(MetadataSource, ContentType)>,
    status: Option<(MetadataSource, SeriesStatus)>,
    release_year: Option<(MetadataSource, i32)>,
}

/// Run every field through the priority, offering the incoming value against the stored one.
///
/// The incoming candidate is listed first within a source, so re-scanning a series this source
/// already owns picks up the fresh value instead of pinning the first one it ever wrote.
fn resolve_all<'a>(
    source: MetadataSource,
    incoming: &MetadataCandidate<'a>,
    stored: &'a StoredMetadata,
    priority: &MetadataPriority,
) -> Winners<'a> {
    Winners {
        title: priority.resolve(
            MetadataField::Title,
            &[
                (source, incoming.canonical_title),
                (
                    stored_source(stored.title_source),
                    Some(stored.canonical_title.as_str()),
                ),
            ],
        ),
        description: priority.resolve(
            MetadataField::Description,
            &[
                (source, incoming.description),
                (
                    stored_source(stored.description_source),
                    stored.description.as_deref(),
                ),
            ],
        ),
        cover: priority.resolve(
            MetadataField::Cover,
            &[
                (source, incoming.cover_url),
                (
                    stored_source(stored.cover_source),
                    stored.cover_url.as_deref(),
                ),
            ],
        ),
        content_type: priority.resolve(
            MetadataField::ContentType,
            &[
                (source, incoming.content_type),
                (
                    stored_source(stored.content_type_source),
                    Some(stored.content_type),
                ),
            ],
        ),
        status: priority.resolve(
            MetadataField::Status,
            &[
                (source, incoming.status),
                (stored_source(stored.status_source), Some(stored.status)),
            ],
        ),
        release_year: priority.resolve(
            MetadataField::ReleaseYear,
            &[
                (source, incoming.release_year),
                (
                    stored_source(stored.release_year_source),
                    stored.release_year,
                ),
            ],
        ),
    }
}

/// Fold one source's metadata into `series_id` under the configured priority, recording which
/// source won each field.
///
/// Runs on the caller's connection so it can join an ingest or enrichment transaction, and
/// takes the row's lock before reading: resolving against a value read outside the transaction
/// would decide against a value a concurrent scan has already replaced.
///
/// Title candidates that lose are kept as alternative titles rather than dropped — a name the
/// work is published under is matching evidence whoever supplied it, and `series_titles` is
/// what a later cross-provider scan looks the series up by.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only; a `series_id` erased mid-transaction is a no-op `Ok(())`.
pub async fn merge_metadata(
    conn: &mut sqlx::PgConnection,
    series_id: SeriesId,
    source: MetadataSource,
    incoming: &MetadataCandidate<'_>,
    priority: &MetadataPriority,
) -> DbResult<()> {
    let Some(stored) = sqlx::query_as!(
        StoredMetadata,
        r#"SELECT canonical_title,
                  description,
                  cover_url,
                  content_type AS "content_type: ContentType",
                  status AS "status: SeriesStatus",
                  release_year,
                  title_source AS "title_source: MetadataSource",
                  description_source AS "description_source: MetadataSource",
                  cover_source AS "cover_source: MetadataSource",
                  content_type_source AS "content_type_source: MetadataSource",
                  status_source AS "status_source: MetadataSource",
                  release_year_source AS "release_year_source: MetadataSource"
           FROM series WHERE id = $1 FOR UPDATE"#,
        series_id.as_uuid(),
    )
    .fetch_optional(&mut *conn)
    .await?
    else {
        return Ok(());
    };

    let Winners {
        title,
        description,
        cover,
        content_type,
        status,
        release_year,
    } = resolve_all(source, incoming, &stored, priority);

    // Only when the title actually moves: `normalized_title` is what trigram candidate lookup
    // reads, and rewriting it on every scan for an unchanged title is pure index churn.
    let renamed = title.is_some_and(|(_, t)| t != stored.canonical_title.as_str());

    sqlx::query!(
        "UPDATE series SET \
            canonical_title = COALESCE($2, canonical_title), \
            normalized_title = COALESCE($3, normalized_title), \
            title_source = COALESCE($4, title_source), \
            description = COALESCE($5, description), \
            description_source = COALESCE($6, description_source), \
            cover_url = COALESCE($7, cover_url), \
            cover_source = COALESCE($8, cover_source), \
            content_type = COALESCE($9, content_type), \
            content_type_source = COALESCE($10, content_type_source), \
            status = COALESCE($11, status), \
            status_source = COALESCE($12, status_source), \
            release_year = COALESCE($13, release_year), \
            release_year_source = COALESCE($14, release_year_source), \
            updated_at = now() \
         WHERE id = $1",
        series_id.as_uuid(),
        title.map(|(_, t)| t),
        title.filter(|_| renamed).map(|(_, t)| normalize_title(t)),
        title.map(|(s, _)| s) as Option<MetadataSource>,
        description.map(|(_, v)| v),
        description.map(|(s, _)| s) as Option<MetadataSource>,
        cover.map(|(_, v)| v),
        cover.map(|(s, _)| s) as Option<MetadataSource>,
        content_type.map(|(_, v)| v) as Option<ContentType>,
        content_type.map(|(s, _)| s) as Option<MetadataSource>,
        status.map(|(_, v)| v) as Option<SeriesStatus>,
        status.map(|(s, _)| s) as Option<MetadataSource>,
        release_year.map(|(_, v)| v),
        release_year.map(|(s, _)| s) as Option<MetadataSource>,
    )
    .execute(&mut *conn)
    .await?;

    let winning_title = title.as_ref().map(|(_, t)| *t);
    let losing: Vec<(String, String)> = [
        incoming.canonical_title,
        Some(stored.canonical_title.as_str()),
    ]
    .into_iter()
    .flatten()
    .filter(|t| Some(*t) != winning_title)
    .map(|t| (t.to_owned(), normalize_title(t)))
    .collect();
    if !losing.is_empty() {
        add_series_titles(&mut *conn, series_id, &losing).await?;
    }
    Ok(())
}
