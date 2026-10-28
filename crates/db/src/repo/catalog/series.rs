//! Canonical series: the row every provider source attaches to, and the lookup-and-write half
//! of deciding which existing series a newly-scanned one *is*.

use crate::error::{DbError, DbResult};
use sqlx::{FromRow, PgExecutor};
use std::collections::{HashMap, HashSet};
use tankovault_domain::matching::{Canonicaliser, Decision, Query};
use tankovault_domain::{
    ContentType, MetadataSource, MetadataValue, Series, SeriesId, SeriesStatus,
};
use time::OffsetDateTime;
use uuid::Uuid;

/// Canonical-series metadata to upsert (from an adapter's `fetch_series`).
pub struct SeriesUpsert {
    /// The title to present the work under.
    pub canonical_title: String,
    /// That title under [`tankovault_domain::normalize_title`], which is the matching key.
    pub normalized_title: String,
    /// Synopsis, `None` when the adapter found none.
    pub description: Option<String>,
    /// Cover image link, `None` when the adapter found none.
    pub cover_url: Option<String>,
    /// Medium, `Unknown` when the provider states none.
    pub content_type: ContentType,
    /// Publication status, `Unknown` when the provider states none.
    pub status: SeriesStatus,
    /// Year of first publication, `None` when the provider states none.
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

/// Which series a scanned source resolved to, and whether resolving it created that series.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Resolution {
    /// The series the source belongs to.
    pub series_id: SeriesId,
    /// `true` when this call inserted the series. A caller whose source then lands elsewhere
    /// (a concurrent registration won) owns that series and must not leave it behind empty.
    pub created: bool,
}

/// Resolves which canonical series a scanned source belongs to (`docs/ARCHITECTURE.md` §10).
///
/// A trigram candidate lookup, then whatever the caller's [`Canonicaliser`] decides: attach to
/// an existing series, create one and file a merge candidate against the near miss, or create
/// one outright.
///
/// Called once per entry inside the caller's transaction, so each entry resolves against series
/// its predecessors just created; concurrent cross-provider creation can still race and produce
/// two series, converged later by the merge queue.
///
/// Always yields a series id, creating one if nothing matches.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only; never [`crate::DbError::NotFound`].
pub async fn resolve_canonical_series(
    conn: &mut sqlx::PgConnection,
    meta: &SeriesUpsert,
    canonicaliser: &dyn Canonicaliser,
) -> DbResult<Resolution> {
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

    let created = |series_id| Resolution {
        series_id,
        created: true,
    };
    match canonicaliser.canonicalise(&query, &candidates) {
        Decision::Attach(series_id) => Ok(Resolution {
            series_id,
            created: false,
        }),
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
            Ok(created(id))
        }
        Decision::Create => create_series(conn, meta).await.map(created),
    }
}

/// Inserts a fresh canonical series from scanned metadata, returning its new id.
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

/// Fetches one canonical series by id.
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

/// Returns one series, but only if the adult gate lets this reader see it.
///
/// A gated series a reader has not opted into is [`DbError::NotFound`], the same as an id that
/// never existed. A distinct "exists but forbidden" answer would turn the detail route into an
/// oracle confirming which ids are adult to anyone willing to enumerate.
///
/// Separate from [`get_series`], whose other callers serve no reader: the scan worker, the sync
/// engine and the notification decorator need the row whoever may look at it.
///
/// # Errors
/// [`DbError::NotFound`] when no such series exists *or* it is gated for this reader;
/// [`crate::DbError::Sqlx`] otherwise.
pub async fn get_series_visible<'e, E: PgExecutor<'e>>(
    exec: E,
    id: SeriesId,
    include_adult: bool,
) -> DbResult<Series> {
    let row = sqlx::query_as!(
        SeriesRow,
        "SELECT id, canonical_title, normalized_title, description, cover_url, \
         content_type AS \"content_type: ContentType\", status AS \"status: SeriesStatus\", \
         release_year, created_at, updated_at \
         FROM series WHERE id = $1 AND (NOT adult_gated OR $2)",
        id.as_uuid(),
        include_adult,
    )
    .fetch_optional(exec)
    .await?;
    row.ok_or(DbError::NotFound)?.try_into()
}

/// Returns which of `ids` are adult-gated, for the badge on rows a reader can already see.
///
/// Only asked about series that passed the gate, so this labels what is on screen. It is not a
/// gate and must not be used as one.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only; an empty `ids` is an empty set.
pub async fn adult_gated_many<'e, E: PgExecutor<'e>>(
    exec: E,
    ids: &[SeriesId],
) -> DbResult<HashSet<SeriesId>> {
    if ids.is_empty() {
        return Ok(HashSet::new());
    }
    let uuids: Vec<Uuid> = ids.iter().map(|id| id.as_uuid()).collect();
    let rows: Vec<Uuid> = sqlx::query_scalar!(
        "SELECT id FROM series WHERE id = ANY($1) AND adult_gated",
        &uuids,
    )
    .fetch_all(exec)
    .await?;
    Ok(rows.into_iter().map(SeriesId::from_uuid).collect())
}

/// The two fields a list row needs to name a series it links to.
pub struct SeriesDisplay {
    /// The canonical title.
    pub title: String,
    /// The cover link, `None` when no provider supplied one.
    pub cover_url: Option<String>,
}

/// Returns the title and cover of each of `ids` in one lookup, so a page of rows that reference
/// series by id is named in one query. Ids that match no row are absent from the map.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only; an empty `ids` is an empty map.
pub async fn series_display_many<'e, E: PgExecutor<'e>>(
    exec: E,
    ids: &[SeriesId],
) -> DbResult<HashMap<SeriesId, SeriesDisplay>> {
    if ids.is_empty() {
        return Ok(HashMap::new());
    }
    let uuids: Vec<Uuid> = ids.iter().map(|id| id.as_uuid()).collect();
    let rows = sqlx::query!(
        "SELECT id, canonical_title, cover_url FROM series WHERE id = ANY($1)",
        &uuids,
    )
    .fetch_all(exec)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| {
            (
                SeriesId::from_uuid(r.id),
                SeriesDisplay {
                    title: r.canonical_title,
                    cover_url: r.cover_url,
                },
            )
        })
        .collect())
}
