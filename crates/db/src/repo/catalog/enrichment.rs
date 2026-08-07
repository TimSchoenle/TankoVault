//! Metadata enrichment: the sweep's work list, folding resolved upstream metadata into a
//! series, and the alternative-title / tag / author link tables it writes.

use super::metadata::{MetadataCandidate, merge_metadata};
use crate::error::DbResult;
use sqlx::{FromRow, PgExecutor};
// `slugify` is the domain's, not a local copy: `TagBlocklist` compares against exactly the key
// this module writes into `tags.slug`, and a second implementation here is the drift that would
// make the intake guard fail open.
use tankovault_domain::{MetadataPriority, MetadataSource, SeriesId, TagBlocklist, slugify};
use time::OffsetDateTime;
use uuid::Uuid;

/// The smallest weight a tag link may carry.
///
/// `series_tags_weight_check` enforces `weight > 0`, and `AniList` publishes tags with a rank of
/// zero — a term somebody proposed and nobody upvoted. Flooring keeps the term (it is still
/// evidence, just the weakest kind) where dropping it would silently narrow the vocabulary and a
/// literal `0.0` would abort the transaction that carried it.
pub const MIN_TAG_WEIGHT: f32 = 0.01;

/// Minimal series row for the enrichment worker: enough to look the series up upstream.
///
/// Carries no current field values on purpose — [`super::merge_metadata`] reads those under the
/// row lock when the answer comes back, which is seconds of upstream latency later. Resolving
/// priority against a value read here would decide against one a concurrent scan has replaced.
pub struct SeriesEnrichmentRow {
    pub id: SeriesId,
    pub canonical_title: String,
}

/// The shared row shape behind the two enrichment work-list queries.
#[derive(FromRow)]
struct EnrichmentRow {
    id: Uuid,
    canonical_title: String,
}

impl From<EnrichmentRow> for SeriesEnrichmentRow {
    fn from(r: EnrichmentRow) -> Self {
        Self {
            id: SeriesId::from_uuid(r.id),
            canonical_title: r.canonical_title,
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
        "SELECT id, canonical_title FROM series \
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

/// Which of `series_ids` have not had metadata attempted since `stale_before`. One statement for
/// the whole set.
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
        "SELECT id, canonical_title FROM series \
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

/// The single row of `metadata_sweep_state`: what the enrichment sweep is doing, or last did.
#[derive(Debug, Clone, FromRow)]
pub struct MetadataSweepState {
    pub running: bool,
    pub started_at: Option<OffsetDateTime>,
    pub finished_at: Option<OffsetDateTime>,
    pub scanned: i32,
    pub enriched: i32,
    pub unresolved: i32,
    pub error: Option<String>,
}

/// How much of the catalogue the sweep has and has not reached.
#[derive(Debug, Clone, Copy, FromRow)]
pub struct MetadataSweepCoverage {
    pub series_total: i64,
    /// Series the sweep has never attempted. These lead every work list, so a figure that never
    /// falls is the signal that the sweep is not running at all.
    pub never_checked: i64,
    /// Series attempted within the last day — how much ground the recent runs actually covered.
    pub checked_last_day: i64,
}

/// Read the enrichment sweep's state.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only; the row is created by migration 0034 and cannot be absent.
pub async fn read_sweep_state<'e, E: PgExecutor<'e>>(exec: E) -> DbResult<MetadataSweepState> {
    let state = sqlx::query_as!(
        MetadataSweepState,
        "SELECT running, started_at, finished_at, scanned, enriched, unresolved, error \
         FROM metadata_sweep_state WHERE id",
    )
    .fetch_one(exec)
    .await?;
    Ok(state)
}

/// Read how far the sweep has got through the catalogue.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only.
pub async fn read_sweep_coverage<'e, E: PgExecutor<'e>>(
    exec: E,
) -> DbResult<MetadataSweepCoverage> {
    let coverage = sqlx::query_as!(
        MetadataSweepCoverage,
        "SELECT count(*) AS \"series_total!\", \
                count(*) FILTER (WHERE metadata_checked_at IS NULL) AS \"never_checked!\", \
                count(*) FILTER (WHERE metadata_checked_at > now() - interval '1 day') \
                  AS \"checked_last_day!\" \
         FROM series",
    )
    .fetch_one(exec)
    .await?;
    Ok(coverage)
}

/// Mark a sweep as started, clearing the previous run's counters.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only.
pub async fn begin_sweep<'e, E: PgExecutor<'e>>(exec: E) -> DbResult<()> {
    sqlx::query!(
        "UPDATE metadata_sweep_state \
            SET running = true, started_at = now(), finished_at = NULL, \
                scanned = 0, enriched = 0, unresolved = 0, error = NULL \
          WHERE id",
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// Publish the counters of a sweep still in flight.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only.
pub async fn record_sweep_progress<'e, E: PgExecutor<'e>>(
    exec: E,
    scanned: i32,
    enriched: i32,
    unresolved: i32,
) -> DbResult<()> {
    sqlx::query!(
        "UPDATE metadata_sweep_state \
            SET scanned = $1, enriched = $2, unresolved = $3 WHERE id",
        scanned,
        enriched,
        unresolved,
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// Close a sweep out, recording how it ended.
///
/// **Must run on the failure path too**, or `running` stays true forever and the console reports
/// a sweep in flight that no longer exists.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only.
pub async fn finish_sweep<'e, E: PgExecutor<'e>>(
    exec: E,
    scanned: i32,
    enriched: i32,
    unresolved: i32,
    error: Option<&str>,
) -> DbResult<()> {
    sqlx::query!(
        "UPDATE metadata_sweep_state \
            SET running = false, finished_at = now(), \
                scanned = $1, enriched = $2, unresolved = $3, error = $4 \
          WHERE id",
        scanned,
        enriched,
        unresolved,
        error,
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// Record that a scan's own genre chips classify this series as adult.
///
/// Writes `adult_inferred` and only ever sets it: there is no call that clears it. A provider
/// dropping the chip from one page, or an adapter selector breaking, must not reopen a gate
/// that a previous scan closed — the two are indistinguishable from here, and one of them is
/// a silent regression that shows adult series to readers who never opted in. Clearing a
/// wrong verdict is an operator action against the database, deliberately not a code path.
///
/// Guarded on the current value so a re-scan of an already-flagged series writes nothing,
/// keeping `updated_at` and the row version untouched under at-least-once delivery.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only; an erased `series_id` is a no-op `Ok(())`.
pub async fn mark_adult_inferred<'e, E: PgExecutor<'e>>(exec: E, series_id: SeriesId) -> DbResult<()> {
    sqlx::query!(
        "UPDATE series SET adult_inferred = true WHERE id = $1 AND NOT adult_inferred",
        series_id.as_uuid(),
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// A batch of metadata to fold into an existing series. `None` leaves a field untouched;
/// titles/tags/authors are additive.
pub struct MetadataEnrichment<'a> {
    /// The fields another source can also supply, resolved under the operator's priority.
    pub candidate: MetadataCandidate<'a>,
    /// Whether upstream classifies the work as adult. `None` means "no opinion", which must not
    /// clear a flag another source set — this is a content gate, and losing it is the failure
    /// that shows adult series to readers who never opted in.
    pub is_adult: Option<bool>,
    /// Upstream's average score, 0..100. Overwritten when present: a score drifts, and a stale
    /// one is worse than the current one.
    pub external_score: Option<f32>,
    pub external_popularity: Option<i32>,
    /// What the work was adapted from (`original`, `light_novel`, …), lower-cased.
    pub external_source: Option<&'a str>,
    pub alt_titles: &'a [(String, String)],
    pub tags: &'a [TagLink<'a>],
    pub authors: &'a [String],
}

/// Apply an enrichment batch to a series in one transaction: fold the prioritised fields in
/// through [`merge_metadata`], record the upstream-only signals, and additively record
/// titles/tags/authors. Idempotent.
///
/// Also stamps `metadata_checked_at`, so a successful enrichment leaves the sweep's work list
/// the same way a fruitless lookup does (see [`list_series_for_enrichment`]).
///
/// # Errors
/// [`crate::DbError::Sqlx`] only; an erased `series_id` is a no-op `Ok(())`.
pub async fn apply_enrichment(
    pool: &sqlx::PgPool,
    series_id: SeriesId,
    enrichment: &MetadataEnrichment<'_>,
    priority: &MetadataPriority,
    blocked: &TagBlocklist,
) -> DbResult<()> {
    let mut tx = pool.begin().await?;
    merge_metadata(
        &mut tx,
        series_id,
        MetadataSource::AniList,
        &enrichment.candidate,
        priority,
    )
    .await?;
    // The fields no adapter can supply, so nothing to resolve: upstream is the only source.
    sqlx::query!(
        "UPDATE series SET \
            is_adult = COALESCE($2, is_adult), \
            external_score = COALESCE($3, external_score), \
            external_popularity = COALESCE($4, external_popularity), \
            external_source = COALESCE($5, external_source), \
            metadata_checked_at = now() \
         WHERE id = $1",
        series_id.as_uuid(),
        enrichment.is_adult,
        enrichment.external_score,
        enrichment.external_popularity,
        enrichment.external_source,
    )
    .execute(&mut *tx)
    .await?;
    if !enrichment.alt_titles.is_empty() {
        add_series_titles(&mut tx, series_id, enrichment.alt_titles).await?;
    }
    if !enrichment.tags.is_empty() {
        add_series_tags(&mut tx, series_id, enrichment.tags, blocked).await?;
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

/// The axis a tag lives on, mirroring the `tags_kind_check` constraint.
///
/// `Genre` is the coarse twenty-term vocabulary every provider agrees on; `Theme` is
/// `AniList`'s ~600-term descriptive one, where the link weight carries how strongly the term
/// applies. The recommender weights the two differently, which is the whole reason the column
/// exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TagKind {
    Genre,
    Theme,
    Demographic,
    Derived,
}

impl TagKind {
    /// The token stored in `tags.kind`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Genre => "genre",
            Self::Theme => "theme",
            Self::Demographic => "demographic",
            Self::Derived => "derived",
        }
    }
}

/// Who supplied a tag link, stored in `series_tags.source`.
///
/// Precedence, not decoration: [`add_series_tags`] refuses to let a scraping adapter overwrite
/// an `AniList` link, because the adapter has only a name where `AniList` has a rank.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TagSource {
    /// A scraping adapter, which knows a name and nothing else. Its token is also spelled
    /// literally in [`add_series_tags`]' conflict predicate — a rename must change both, or the
    /// precedence rule stops matching and silently becomes last-writer-wins.
    Provider,
    AniList,
}

impl TagSource {
    /// The token stored in `series_tags.source`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Provider => "provider",
            Self::AniList => "anilist",
        }
    }
}

/// One tag attached to a series, with the strength and provenance the link table stores.
#[derive(Debug, Clone, Copy)]
pub struct TagLink<'a> {
    pub name: &'a str,
    pub kind: TagKind,
    /// `(0, 1]`. Zero is not merely discouraged — `series_tags_weight_check` rejects it, so a
    /// caller with a zero-ranked term must floor it or drop the term.
    pub weight: f32,
    pub source: TagSource,
}

impl<'a> TagLink<'a> {
    /// A plain genre from a scraping adapter: wholly present, no rank to carry.
    #[must_use]
    pub const fn genre(name: &'a str) -> Self {
        Self {
            name,
            kind: TagKind::Genre,
            weight: 1.0,
            source: TagSource::Provider,
        }
    }
}

/// Add tag links to a series (idempotent). Empty/unslugifiable names are skipped, as are terms
/// `blocked` refuses.
///
/// The guard is applied *here*, at the one statement that interns a term into the shared `tags`
/// vocabulary, rather than at each caller: a refused term must never reach the table, and a
/// filter at one of the two producers is a filter the other one does not have.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only; unknown `series_id` is a foreign-key violation. All-empty
/// names is `Ok(())`. Statements share the caller's transaction, not their own.
pub async fn add_series_tags(
    conn: &mut sqlx::PgConnection,
    series_id: SeriesId,
    tags: &[TagLink<'_>],
    blocked: &TagBlocklist,
) -> DbResult<()> {
    let mut seen = std::collections::HashSet::new();
    let mut slugs = Vec::with_capacity(tags.len());
    let mut names = Vec::with_capacity(tags.len());
    let mut kinds = Vec::with_capacity(tags.len());
    let mut weights = Vec::with_capacity(tags.len());
    let mut sources = Vec::with_capacity(tags.len());
    for tag in tags {
        let slug = slugify(tag.name);
        if slug.is_empty() || blocked.blocks(tag.name) || !seen.insert(slug.clone()) {
            continue;
        }
        slugs.push(slug);
        names.push(tag.name);
        kinds.push(tag.kind.as_str());
        // Clamped, not trusted: `series_tags_weight_check` rejects `weight <= 0`, and a whole
        // enrichment transaction failing over one badly-ranked tag loses the rest of the batch.
        weights.push(tag.weight.clamp(MIN_TAG_WEIGHT, 1.0));
        sources.push(tag.source.as_str());
    }
    if slugs.is_empty() {
        return Ok(());
    }

    // `DO NOTHING`, not `DO UPDATE`: a no-op update would still take a write lock on a
    // globally-shared `tags` row, serializing concurrent ingests of the same genre. The cost is
    // that `kind` is first-writer-wins — a term already interned as a genre stays one — which is
    // the right way round, since the coarse vocabulary is the one every provider agrees on.
    sqlx::query!(
        "INSERT INTO tags (slug, name, kind) \
         SELECT * FROM UNNEST($1::text[], $2::text[], $3::text[]) \
         ON CONFLICT (slug) DO NOTHING",
        &slugs as &[String],
        &names as &[&str],
        &kinds as &[&str],
    )
    .execute(&mut *conn)
    .await?;

    // The `WHERE` on the conflict arm is the precedence rule: a source refreshes its own links,
    // and `AniList` upgrades an adapter's bare genre to a ranked one, but the next scrape must
    // not flatten that rank back to 1.0. Without it the weight flaps between the two writers on
    // every sweep, and the digest churn re-embeds the whole catalogue for nothing.
    sqlx::query!(
        "INSERT INTO series_tags (series_id, tag_id, weight, source) \
         SELECT $1, t.id, u.weight, u.source \
         FROM UNNEST($2::text[], $3::real[], $4::text[]) AS u(slug, weight, source) \
         JOIN tags t ON t.slug = u.slug \
         ON CONFLICT (series_id, tag_id) DO UPDATE \
            SET weight = EXCLUDED.weight, source = EXCLUDED.source \
          WHERE series_tags.source = EXCLUDED.source OR series_tags.source = 'provider'",
        series_id.as_uuid(),
        &slugs as &[String],
        &weights as &[f32],
        &sources as &[&str],
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
/// link statement would then attach the same `(series_id, author_id)` pair twice.
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
