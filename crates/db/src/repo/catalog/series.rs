//! Canonical series: the row every provider source attaches to, and the matcher-backed
//! decision about which existing series a newly-scanned one *is*.

use crate::error::{DbError, DbResult};
use sqlx::{FromRow, PgExecutor};
use tankovault_config::MatchingConfig;
use tankovault_domain::{ContentType, Series, SeriesId, SeriesStatus};
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
