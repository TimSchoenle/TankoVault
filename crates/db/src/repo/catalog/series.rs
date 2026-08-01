//! Canonical series: the row every provider source attaches to, and the lookup-and-write half
//! of deciding which existing series a newly-scanned one *is*.

use crate::error::{DbError, DbResult};
use sqlx::{FromRow, PgExecutor};
use tankovault_domain::matching::{Canonicaliser, Decision, Query};
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

/// Resolve the canonical series for a scanned source (design §10): trigram candidate lookup,
/// then whatever the caller's [`Canonicaliser`] decides.
///
/// - **Attach** → return the existing series id; the source will hang off it.
/// - **Ambiguous** → create a new series *and* record a `merge_candidate` for operator review
///   (one-click merge/split in the console).
/// - **Create** → a new canonical series.
///
/// This function reads and writes; it does **not** decide. Scoring, the confidence thresholds
/// and how wide to look all live above this crate, behind the [`Canonicaliser`] port
/// (`tankovault_config::MatchingConfig` over `tankovault_matcher`), so the worker's ingest and
/// external sync cannot disagree about whether two series are the same and this crate links no
/// scorer (ARCH-16). It used to call `matcher::decide` here with `Thresholds::default()`
/// hardcoded.
///
/// Runs inside the ingest transaction so lookup + create are atomic for a single worker, and
/// is called once **per entry** from inside the caller's loop — each entry must resolve against
/// the series its predecessors created in that same transaction (PERF-15). Concurrent
/// first-creation of the same title across providers can still produce two series; that is the
/// case the ambiguous/merge queue and the re-scan Attach path converge.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only — no other variant is reachable. There is deliberately no
/// [`crate::DbError::NotFound`]: this function's contract is that it *always* yields a series id,
/// creating one when nothing matches, so an unrecognised title is the ordinary path rather than
/// a miss. Note also that [`Canonicaliser::canonicalise`] returns a [`Decision`] and not a
/// `Result` — a matching policy has no way to fail an ingest, which is why every error here is
/// the driver's.
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

    // No tag/author signal on the query side here: a scanned source's own tags/authors
    // aren't threaded into `SeriesUpsert` (they're written separately in `ingest_series`).
    // The bonus simply never fires — unchanged behaviour from before this field existed.
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
            // The signals travel with the row. `"ambiguous title match"` was the *only* reason
            // this queue ever recorded, for every one of its rows, which left an operator
            // triaging thousands of pairs with two titles and a percentage and no way to tell a
            // whitespace variant from a coincidence of wording. It is also what lets the
            // standing sweep re-judge a row without re-deriving how it got there.
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
///
/// # Errors
/// [`crate::DbError::Sqlx`] only — no other variant is reachable. An `id` that does not exist
/// matches nothing and is still `Ok(())`, not [`crate::DbError::NotFound`]; the only caller is
/// [`super::ingest::ingest_series`], which resolved the id one statement earlier in the same
/// transaction, so the row is guaranteed to be there. A caller that obtained the id elsewhere
/// gets no signal that it wrote nothing.
///
/// The `COALESCE`s are one-directional on purpose: a re-scan that stops reporting a description
/// or cover keeps the stored one, so a provider page that breaks does not blank the catalogue.
/// `canonical_title`, `content_type` and `status` are *not* coalesced — those the newest scan
/// owns outright.
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
///
/// # Errors
/// - [`crate::DbError::NotFound`] if no series carries this id. This is one of the few
///   repository functions that raises it rather than answering `Ok(None)`, because its callers
///   are all "render this series" paths where a miss *is* the 404 the API must return.
/// - [`crate::DbError::Sqlx`] for any driver or connection failure.
///
/// The `try_into` at the end cannot currently fail — [`Series`]'s `TryFrom<SeriesRow>` is
/// infallible today, and both enum columns are decoded by the driver against native Postgres
/// enums (so drift arrives as `Sqlx(ColumnDecode)`, see OPS-2.2c). It stays a `TryFrom` so that
/// a future field needing validation has somewhere to fail.
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
