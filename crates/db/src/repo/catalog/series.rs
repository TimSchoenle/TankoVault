//! Canonical series: the row every provider source attaches to, and the lookup-and-write half
//! of deciding which existing series a newly-scanned one *is*.

use crate::error::{DbError, DbResult};
use sqlx::{FromRow, PgExecutor};
use tankovault_domain::matching::{Canonicaliser, Decision, Query};
use tankovault_domain::{
    ContentType, MetadataSource, MetadataValue, Series, SeriesId, SeriesStatus,
};
use time::OffsetDateTime;
use uuid::Uuid;

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

impl SeriesUpsert {
    /// This scan's offer for the prioritised fields, for [`super::merge_metadata`].
    #[must_use]
    pub fn candidate(&self) -> super::MetadataCandidate<'_> {
        super::MetadataCandidate {
            canonical_title: Some(&self.canonical_title),
            description: self.description.as_deref(),
            cover_url: self.cover_url.as_deref(),
            content_type: Some(self.content_type),
            status: Some(self.status),
            release_year: self.release_year,
        }
    }
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

/// Resolve the canonical series for a scanned source (design §10): trigram candidate lookup,
/// then whatever the caller's [`Canonicaliser`] decides — attach, flag ambiguous (new series
/// plus a merge-candidate row), or create.
///
/// Called once per entry inside the caller's transaction, so each entry resolves against series
/// its predecessors just created; concurrent cross-provider creation can still race and produce
/// two series, converged later by the merge queue.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only; always yields a series id (creates one if nothing matches),
/// never [`crate::DbError::NotFound`].
pub async fn resolve_canonical_series(
    conn: &mut sqlx::PgConnection,
    meta: &SeriesUpsert,
    canonicaliser: &dyn Canonicaliser,
) -> DbResult<SeriesId> {
    let candidates = crate::repo::matching::find_candidates(
        &mut *conn,
        &meta.normalized_title,
        canonicaliser.candidate_limit(),
    )
    .await?;

    // Tags/authors aren't threaded into `SeriesUpsert` (written separately in
    // `ingest_series`), so that scoring bonus never fires here.
    let query = Query {
        normalized_title: meta.normalized_title.clone(),
        content_type: meta.content_type,
        release_year: meta.release_year,
        tags: Vec::new(),
        authors: Vec::new(),
    };

    match canonicaliser.canonicalise(&query, &candidates) {
        Decision::Attach(id) => Ok(id),
        Decision::Ambiguous {
            candidate,
            score,
            signals,
        } => {
            let id = create_series(conn, meta).await?;
            // Signals travel with the row so an operator can tell a whitespace variant from a
            // coincidence, and the sweep can re-judge without re-deriving how it got there.
            let labels = signals.labels();
            crate::repo::matching::record_merge_candidate(
                &mut *conn,
                id,
                candidate,
                score,
                &labels,
                "ambiguous title match",
            )
            .await?;
            Ok(id)
        }
        Decision::Create => create_series(conn, meta).await,
    }
}

/// Insert a fresh canonical series from scanned metadata, returning its new id.
async fn create_series(conn: &mut sqlx::PgConnection, meta: &SeriesUpsert) -> DbResult<SeriesId> {
    let id = SeriesId::new();
    // Provenance is stamped only where the adapter actually said something: a stub carries a
    // title and nothing else, and claiming authorship of its placeholders would let them
    // outrank a real value under an adapter-first order.
    let attributed = |present: bool| present.then_some(MetadataSource::Adapter);
    sqlx::query!(
        "INSERT INTO series (id, canonical_title, normalized_title, description, \
         cover_url, content_type, status, release_year, title_source, description_source, \
         cover_source, content_type_source, status_source, release_year_source) \
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14)",
        id.as_uuid(),
        &meta.canonical_title,
        &meta.normalized_title,
        meta.description.as_deref(),
        meta.cover_url.as_deref(),
        meta.content_type as ContentType,
        meta.status as SeriesStatus,
        meta.release_year,
        attributed(meta.canonical_title.is_answer()) as Option<MetadataSource>,
        attributed(
            meta.description
                .as_deref()
                .is_some_and(MetadataValue::is_answer)
        ) as Option<MetadataSource>,
        attributed(
            meta.cover_url
                .as_deref()
                .is_some_and(MetadataValue::is_answer)
        ) as Option<MetadataSource>,
        attributed(meta.content_type.is_answer()) as Option<MetadataSource>,
        attributed(meta.status.is_answer()) as Option<MetadataSource>,
        attributed(meta.release_year.is_some()) as Option<MetadataSource>,
    )
    .execute(&mut *conn)
    .await?;
    Ok(id)
}

/// Fetch one canonical series by id.
///
/// # Errors
/// [`crate::DbError::NotFound`] if no series has this id; otherwise [`crate::DbError::Sqlx`].
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
