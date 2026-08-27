//! The undo journal a merge writes, and the transaction that spends it.
//!
//! A merge is the one automatic action in the system that *destroys* a row, and until this module
//! existed it was also the one action with no way back: `merge_series` unioned the absorbed
//! series' children onto the survivor and deleted it, and nothing anywhere recorded what the
//! absorbed series had been.
//!
//! # Why a journal rather than a soft delete
//!
//! Marking the absorbed series deleted instead of deleting it would leave every read model in the
//! system responsible for filtering it out, which is a correctness obligation on code that has no
//! reason to know merges exist. The journal keeps the absorbed row's *values* without keeping the
//! row, so nothing downstream changes.
//!
//! # What "exact" means here
//!
//! Three shapes of change, and the journal records each in the form its inverse needs:
//!
//! - **Rows re-pointed** (`series_sources`, `sync_history`, …): their primary keys, so the
//!   inverse re-points exactly those and nothing that has arrived since.
//! - **Rows created on the survivor** (`series_titles`, `series_tags`, … — every union insert
//!   whose conflict action is `DO NOTHING`): captured from the insert's own `RETURNING`, which
//!   yields *only* the rows that were actually created. Deleting a union insert's whole key set
//!   would take rows the survivor already had.
//! - **Rows overwritten on the survivor** (`read_progress`, `series_sync_overrides`,
//!   `recommendation_feedback` — the three whose conflict action is `DO UPDATE`): the survivor's
//!   own values *before* the merge, because `RETURNING` cannot report what a row used to hold.
//!   Their inverse deletes the survivor's rows at the absorbed side's keys and re-inserts these,
//!   which restores an overwritten row and removes a created one in the same statement pair.
//!
//! What is *not* restored is stated in [`revert_merge`]: the recommender's derived tables, which
//! are recomputed rather than journalled.

use serde::{Deserialize, Serialize};
use serde_json::Value as Json;
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use crate::error::DbResult;

/// The journal format. Bumped when a field's meaning changes, so a stored journal written by an
/// older build is refused rather than half-applied — a partial revert is worse than none.
pub const UNDO_VERSION: u16 = 1;

/// Everything needed to put the catalogue back exactly as it was before one merge.
///
/// Serialised into `merge_decisions.undo`. Every payload is a JSON array of whole rows as
/// `to_jsonb` produced them, restored through `jsonb_populate_recordset`, so adding a column to
/// one of these tables does not silently drop it from a restore — the two round-trip through the
/// composite type rather than a hand-written column list. `series` is the one exception and is
/// listed column by column, because it carries a generated column that cannot be inserted into;
/// `crates/db/tests/repo_matching.rs` pins that list against the live table.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MergeUndo {
    /// Journal format. A restore refuses a version it does not know rather than
    /// populating the composite types from a different shape.
    pub version: u16,
    /// The series that absorbed the other.
    pub survivor_id: Uuid,
    /// The series that stopped existing, and that a revert recreates under this id.
    pub absorbed_id: Uuid,

    /// The absorbed `series` row, whole.
    pub series: Json,

    // The absorbed series' own child rows. All of these cascade away with the series row, so
    // they exist nowhere else once the merge commits.
    /// The absorbed series' alternative titles.
    pub titles: Json,
    /// Its tag links.
    pub tags: Json,
    /// Its author links.
    pub authors: Json,
    /// Its watchlist entries, across every reader.
    pub watchlist: Json,
    /// Its read-progress rows.
    pub progress: Json,
    /// Its external-tracker mappings.
    pub mappings: Json,
    /// Its per-reader metadata overrides.
    pub overrides: Json,
    /// Its notification dedup keys.
    pub dedup: Json,
    /// Its recommendation feedback rows.
    pub feedback: Json,

    // The survivor's rows at the absorbed side's keys, as they were before the merge overwrote
    // them. Only the three tables whose union insert is a `DO UPDATE`.
    /// The survivor's progress rows at those keys, before the union overwrote them.
    pub survivor_progress: Json,
    /// The survivor's overrides at those keys, before the union overwrote them.
    pub survivor_overrides: Json,
    /// The survivor's feedback at those keys, before the union overwrote them.
    pub survivor_feedback: Json,

    // Rows the merge created on the survivor, from each insert's `RETURNING`.
    /// Titles the merge added to the survivor, to delete again.
    pub inserted_titles: Json,
    /// Tag links the merge added, to delete again.
    pub inserted_tags: Json,
    /// Author links the merge added, to delete again.
    pub inserted_authors: Json,
    /// Watchlist entries the merge added, to delete again.
    pub inserted_watchlist: Json,
    /// Mappings the merge added, to delete again.
    pub inserted_mappings: Json,
    /// Dedup keys the merge added, to delete again.
    pub inserted_dedup: Json,

    // Rows re-pointed from the absorbed series to the survivor, by primary key.
    /// Provider sources re-pointed at the survivor, by primary key.
    pub moved_sources: Vec<Uuid>,
    /// Sync-history rows re-pointed, by primary key.
    pub moved_history: Vec<Uuid>,
    /// Sync conflicts re-pointed, by primary key.
    pub moved_conflicts: Vec<Uuid>,
    /// Sync decisions re-pointed, by primary key.
    pub moved_decisions: Vec<Uuid>,
    /// `sync_remote_entries` and `sync_match_blocks` are keyed by a composite rather than a
    /// surrogate id, so these travel as whole rows and their inverse matches on the key columns.
    /// The `sync_remote_entries` rows as they stood, whole.
    pub moved_remote_entries: Json,
    /// The `sync_match_blocks` rows as they stood, whole.
    pub moved_match_blocks: Json,

    /// Merge candidates this merge resolved, to reopen.
    pub resolved_candidates: Vec<Uuid>,
    /// `series_merges` rows whose `survivor_id` this merge's path compression re-pointed.
    pub recompressed: Vec<Uuid>,
}

impl MergeUndo {
    /// A journal with the identifiers set and every payload empty, which the merge fills in as it
    /// goes. `Default` is deliberately not derived: a journal without its two ids is not a
    /// meaningful value.
    pub(super) fn new(survivor_id: Uuid, absorbed_id: Uuid) -> Self {
        let empty = || Json::Array(Vec::new());
        Self {
            version: UNDO_VERSION,
            survivor_id,
            absorbed_id,
            series: Json::Null,
            titles: empty(),
            tags: empty(),
            authors: empty(),
            watchlist: empty(),
            progress: empty(),
            mappings: empty(),
            overrides: empty(),
            dedup: empty(),
            feedback: empty(),
            survivor_progress: empty(),
            survivor_overrides: empty(),
            survivor_feedback: empty(),
            inserted_titles: empty(),
            inserted_tags: empty(),
            inserted_authors: empty(),
            inserted_watchlist: empty(),
            inserted_mappings: empty(),
            inserted_dedup: empty(),
            moved_sources: Vec::new(),
            moved_history: Vec::new(),
            moved_conflicts: Vec::new(),
            moved_decisions: Vec::new(),
            moved_remote_entries: empty(),
            moved_match_blocks: empty(),
            resolved_candidates: Vec::new(),
            recompressed: Vec::new(),
        }
    }

    /// How many rows the journal is carrying, for the console's "this will restore N rows".
    #[must_use]
    pub fn row_count(&self) -> usize {
        let len = |j: &Json| j.as_array().map_or(0, Vec::len);
        len(&self.titles)
            + len(&self.tags)
            + len(&self.authors)
            + len(&self.watchlist)
            + len(&self.progress)
            + len(&self.mappings)
            + len(&self.overrides)
            + len(&self.dedup)
            + len(&self.feedback)
            + len(&self.moved_remote_entries)
            + self.moved_sources.len()
            + self.moved_history.len()
            + self.moved_conflicts.len()
            + self.moved_decisions.len()
    }
}

/// Read everything about the absorbed series that the merge is about to destroy, plus the
/// survivor rows it is about to overwrite.
///
/// Runs inside the merge's own transaction and before any of its writes, so what it captures is
/// the pre-merge state by construction rather than by timing.
pub(super) async fn capture(
    tx: &mut Transaction<'_, Postgres>,
    undo: &mut MergeUndo,
) -> DbResult<()> {
    let keep = undo.survivor_id;
    let drop = undo.absorbed_id;

    // One round trip for the absorbed side. `to_jsonb` of the whole row rather than a column
    // list, so a column added to any of these tables travels without this function changing.
    let absorbed = sqlx::query!(
        "SELECT \
           (SELECT to_jsonb(s) FROM series s WHERE s.id = $1) AS series, \
           COALESCE((SELECT jsonb_agg(to_jsonb(t)) FROM series_titles t WHERE t.series_id = $1), '[]') AS \"titles!\", \
           COALESCE((SELECT jsonb_agg(to_jsonb(t)) FROM series_tags t WHERE t.series_id = $1), '[]') AS \"tags!\", \
           COALESCE((SELECT jsonb_agg(to_jsonb(t)) FROM series_authors t WHERE t.series_id = $1), '[]') AS \"authors!\", \
           COALESCE((SELECT jsonb_agg(to_jsonb(t)) FROM watchlist_entries t WHERE t.series_id = $1), '[]') AS \"watchlist!\", \
           COALESCE((SELECT jsonb_agg(to_jsonb(t)) FROM read_progress t WHERE t.series_id = $1), '[]') AS \"progress!\", \
           COALESCE((SELECT jsonb_agg(to_jsonb(t)) FROM sync_mappings t WHERE t.series_id = $1), '[]') AS \"mappings!\", \
           COALESCE((SELECT jsonb_agg(to_jsonb(t)) FROM series_sync_overrides t WHERE t.series_id = $1), '[]') AS \"overrides!\", \
           COALESCE((SELECT jsonb_agg(to_jsonb(t)) FROM notification_dedup t WHERE t.series_id = $1), '[]') AS \"dedup!\", \
           COALESCE((SELECT jsonb_agg(to_jsonb(t)) FROM recommendation_feedback t WHERE t.series_id = $1), '[]') AS \"feedback!\"",
        drop,
    )
    .fetch_one(&mut **tx)
    .await?;

    undo.series = absorbed.series.unwrap_or(Json::Null);
    undo.titles = absorbed.titles;
    undo.tags = absorbed.tags;
    undo.authors = absorbed.authors;
    undo.watchlist = absorbed.watchlist;
    undo.progress = absorbed.progress;
    undo.mappings = absorbed.mappings;
    undo.overrides = absorbed.overrides;
    undo.dedup = absorbed.dedup;
    undo.feedback = absorbed.feedback;

    // The survivor's side of the three `DO UPDATE` unions, restricted to the keys the absorbed
    // side is about to overwrite. Unrestricted would journal the survivor's entire readership on
    // every merge, for no gain: a key the absorbed side does not carry cannot be touched.
    let survivor = sqlx::query!(
        "SELECT \
           COALESCE((SELECT jsonb_agg(to_jsonb(p)) FROM read_progress p \
             WHERE p.series_id = $1 \
               AND p.user_id IN (SELECT user_id FROM read_progress WHERE series_id = $2)), '[]') \
             AS \"progress!\", \
           COALESCE((SELECT jsonb_agg(to_jsonb(o)) FROM series_sync_overrides o \
             WHERE o.series_id = $1 \
               AND (o.user_id, o.provider) IN \
                   (SELECT user_id, provider FROM series_sync_overrides WHERE series_id = $2)), '[]') \
             AS \"overrides!\", \
           COALESCE((SELECT jsonb_agg(to_jsonb(f)) FROM recommendation_feedback f \
             WHERE f.series_id = $1 \
               AND f.user_id IN (SELECT user_id FROM recommendation_feedback WHERE series_id = $2)), '[]') \
             AS \"feedback!\"",
        keep,
        drop,
    )
    .fetch_one(&mut **tx)
    .await?;

    undo.survivor_progress = survivor.progress;
    undo.survivor_overrides = survivor.overrides;
    undo.survivor_feedback = survivor.feedback;
    Ok(())
}

/// Undo one merge, restoring the absorbed series under its original id.
///
/// # Ordering
///
/// The `series` row goes back first because every other statement here has a foreign key to it,
/// and the forwarding row in `series_merges` goes last: while it exists, a reader that resolved
/// the absorbed id is still being sent to the survivor, which is the correct answer right up to
/// the moment the series is whole again.
///
/// # What is not restored
///
/// The recommender's derived tables — `series_features`, `series_embedding`, `series_prior`,
/// `series_cooccurrence`, `user_series_affinity` — cascaded away with the absorbed series and are
/// not journalled. They are *derived* from the watchlist and read progress this function does
/// restore, so re-deriving them is both cheaper than journalling them and the only way to be sure
/// they agree with the restored truth. Both series are queued for repair and every affected taste
/// profile is marked stale, exactly as the merge itself did.
///
/// # Errors
///
/// [`crate::DbError::Conflict`] when the journal was written by a different [`UNDO_VERSION`], or
/// when the absorbed id has been taken by a live series since the merge (which means something
/// other than this revert has already put it back). Otherwise [`crate::DbError::Sqlx`] from any
/// statement, which rolls the whole revert back: a half-restored series is worse than a merged
/// one, because nothing downstream is prepared for it.
pub async fn revert_merge(pool: &sqlx::PgPool, undo: &MergeUndo) -> DbResult<()> {
    let mut tx = pool.begin().await?;
    revert_merge_in(&mut tx, undo).await?;
    tx.commit().await?;
    Ok(())
}

/// [`revert_merge`], inside a transaction the caller owns.
///
/// The operator-facing revert has more to do than the inverse — it suppresses the pair and
/// disentangles the survivor — and all of it has to land or none of it. Splitting the commit out
/// is what lets those share one transaction; the pool-taking form above is the whole operation
/// for a caller that only wants the inverse.
///
/// # Errors
/// As [`revert_merge`].
#[expect(
    clippy::too_many_lines,
    reason = "one statement per table, in the order the foreign keys require. Splitting it to \
              satisfy a line count would hide that order, which is the only thing about this \
              function that is easy to get wrong"
)]
pub(super) async fn revert_merge_in(
    tx: &mut Transaction<'_, Postgres>,
    undo: &MergeUndo,
) -> DbResult<()> {
    if undo.version != UNDO_VERSION {
        return Err(crate::error::DbError::Conflict(format!(
            "undo journal version {} cannot be applied by this build (expected {UNDO_VERSION})",
            undo.version,
        )));
    }
    let Json::Object(_) = &undo.series else {
        return Err(crate::error::DbError::Conflict(
            "undo journal carries no absorbed series row".to_owned(),
        ));
    };

    let keep = undo.survivor_id;
    let drop = undo.absorbed_id;

    let live = sqlx::query_scalar!(
        "SELECT count(*) AS \"count!\" FROM series WHERE id = $1",
        drop,
    )
    .fetch_one(&mut **tx)
    .await?;
    if live > 0 {
        return Err(crate::error::DbError::Conflict(
            "the absorbed series already exists; this merge has already been reverted".to_owned(),
        ));
    }

    // The absorbed series itself. Named column by column because `search_vec` is generated and
    // cannot be inserted into; `jsonb_populate_record` still supplies the values, so the types
    // round-trip through the composite type rather than through a hand-written cast.
    sqlx::query!(
        "INSERT INTO series (id, canonical_title, normalized_title, description, cover_url, \
                             content_type, status, release_year, created_at, updated_at, \
                             metadata_checked_at, is_adult, external_score, external_popularity, \
                             external_source, title_source, description_source, cover_source, \
                             content_type_source, status_source, release_year_source) \
         SELECT id, canonical_title, normalized_title, description, cover_url, \
                content_type, status, release_year, created_at, updated_at, \
                metadata_checked_at, is_adult, external_score, external_popularity, \
                external_source, title_source, description_source, cover_source, \
                content_type_source, status_source, release_year_source \
           FROM jsonb_populate_record(NULL::series, $1)",
        undo.series,
    )
    .execute(&mut **tx)
    .await?;

    // Sources go back by id, so a source added to the survivor since the merge stays put.
    sqlx::query!(
        "UPDATE series_sources SET series_id = $1 WHERE id = ANY($2)",
        drop,
        &undo.moved_sources,
    )
    .execute(&mut **tx)
    .await?;

    // Rows the merge created on the survivor, and only those.
    sqlx::query!(
        "DELETE FROM series_titles t USING jsonb_populate_recordset(NULL::series_titles, $1) x \
         WHERE t.series_id = x.series_id AND t.normalized = x.normalized",
        undo.inserted_titles,
    )
    .execute(&mut **tx)
    .await?;
    sqlx::query!(
        "DELETE FROM series_tags t USING jsonb_populate_recordset(NULL::series_tags, $1) x \
         WHERE t.series_id = x.series_id AND t.tag_id = x.tag_id",
        undo.inserted_tags,
    )
    .execute(&mut **tx)
    .await?;
    sqlx::query!(
        "DELETE FROM series_authors t USING jsonb_populate_recordset(NULL::series_authors, $1) x \
         WHERE t.series_id = x.series_id AND t.author_id = x.author_id",
        undo.inserted_authors,
    )
    .execute(&mut **tx)
    .await?;
    sqlx::query!(
        "DELETE FROM watchlist_entries t \
           USING jsonb_populate_recordset(NULL::watchlist_entries, $1) x \
         WHERE t.series_id = x.series_id AND t.user_id = x.user_id",
        undo.inserted_watchlist,
    )
    .execute(&mut **tx)
    .await?;
    sqlx::query!(
        "DELETE FROM sync_mappings t USING jsonb_populate_recordset(NULL::sync_mappings, $1) x \
         WHERE t.series_id = x.series_id AND t.provider = x.provider",
        undo.inserted_mappings,
    )
    .execute(&mut **tx)
    .await?;
    sqlx::query!(
        "DELETE FROM notification_dedup t \
           USING jsonb_populate_recordset(NULL::notification_dedup, $1) x \
         WHERE t.series_id = x.series_id AND t.user_id = x.user_id \
           AND t.chapter_number = x.chapter_number",
        undo.inserted_dedup,
    )
    .execute(&mut **tx)
    .await?;

    // The three overwritten tables: clear the survivor at every key the absorbed side carried,
    // then put back exactly what the survivor held there. A key the survivor did not hold is
    // simply not re-inserted, which is how the created rows are removed by the same pair.
    sqlx::query!(
        "DELETE FROM read_progress p \
         WHERE p.series_id = $1 \
           AND p.user_id IN (SELECT user_id \
                             FROM jsonb_populate_recordset(NULL::read_progress, $2))",
        keep,
        undo.progress,
    )
    .execute(&mut **tx)
    .await?;
    sqlx::query!(
        "INSERT INTO read_progress \
         SELECT * FROM jsonb_populate_recordset(NULL::read_progress, $1)",
        undo.survivor_progress,
    )
    .execute(&mut **tx)
    .await?;

    sqlx::query!(
        "DELETE FROM series_sync_overrides o \
         WHERE o.series_id = $1 \
           AND (o.user_id, o.provider) IN \
               (SELECT user_id, provider \
                FROM jsonb_populate_recordset(NULL::series_sync_overrides, $2))",
        keep,
        undo.overrides,
    )
    .execute(&mut **tx)
    .await?;
    sqlx::query!(
        "INSERT INTO series_sync_overrides \
         SELECT * FROM jsonb_populate_recordset(NULL::series_sync_overrides, $1)",
        undo.survivor_overrides,
    )
    .execute(&mut **tx)
    .await?;

    sqlx::query!(
        "DELETE FROM recommendation_feedback f \
         WHERE f.series_id = $1 \
           AND f.user_id IN (SELECT user_id \
                             FROM jsonb_populate_recordset(NULL::recommendation_feedback, $2))",
        keep,
        undo.feedback,
    )
    .execute(&mut **tx)
    .await?;
    sqlx::query!(
        "INSERT INTO recommendation_feedback \
         SELECT * FROM jsonb_populate_recordset(NULL::recommendation_feedback, $1)",
        undo.survivor_feedback,
    )
    .execute(&mut **tx)
    .await?;

    // The absorbed series' own children, which cascaded away with it.
    sqlx::query!(
        "INSERT INTO series_titles SELECT * FROM jsonb_populate_recordset(NULL::series_titles, $1)",
        undo.titles,
    )
    .execute(&mut **tx)
    .await?;
    sqlx::query!(
        "INSERT INTO series_tags SELECT * FROM jsonb_populate_recordset(NULL::series_tags, $1)",
        undo.tags,
    )
    .execute(&mut **tx)
    .await?;
    sqlx::query!(
        "INSERT INTO series_authors SELECT * FROM jsonb_populate_recordset(NULL::series_authors, $1)",
        undo.authors,
    )
    .execute(&mut **tx)
    .await?;
    sqlx::query!(
        "INSERT INTO watchlist_entries \
         SELECT * FROM jsonb_populate_recordset(NULL::watchlist_entries, $1)",
        undo.watchlist,
    )
    .execute(&mut **tx)
    .await?;
    sqlx::query!(
        "INSERT INTO read_progress SELECT * FROM jsonb_populate_recordset(NULL::read_progress, $1)",
        undo.progress,
    )
    .execute(&mut **tx)
    .await?;
    sqlx::query!(
        "INSERT INTO sync_mappings SELECT * FROM jsonb_populate_recordset(NULL::sync_mappings, $1)",
        undo.mappings,
    )
    .execute(&mut **tx)
    .await?;
    sqlx::query!(
        "INSERT INTO series_sync_overrides \
         SELECT * FROM jsonb_populate_recordset(NULL::series_sync_overrides, $1)",
        undo.overrides,
    )
    .execute(&mut **tx)
    .await?;
    sqlx::query!(
        "INSERT INTO notification_dedup \
         SELECT * FROM jsonb_populate_recordset(NULL::notification_dedup, $1)",
        undo.dedup,
    )
    .execute(&mut **tx)
    .await?;
    sqlx::query!(
        "INSERT INTO recommendation_feedback \
         SELECT * FROM jsonb_populate_recordset(NULL::recommendation_feedback, $1)",
        undo.feedback,
    )
    .execute(&mut **tx)
    .await?;

    // Re-pointed rows, by the primary keys the merge recorded.
    sqlx::query!(
        "UPDATE sync_history SET series_id = $1 WHERE id = ANY($2)",
        drop,
        &undo.moved_history,
    )
    .execute(&mut **tx)
    .await?;
    sqlx::query!(
        "UPDATE sync_remote_entries e SET series_id = $1 \
           FROM jsonb_populate_recordset(NULL::sync_remote_entries, $2) x \
         WHERE e.user_id = x.user_id AND e.provider = x.provider \
           AND e.external_id = x.external_id AND e.series_id = $3",
        drop,
        undo.moved_remote_entries,
        keep,
    )
    .execute(&mut **tx)
    .await?;
    sqlx::query!(
        "UPDATE sync_conflicts SET series_id = $1 WHERE id = ANY($2)",
        drop,
        &undo.moved_conflicts,
    )
    .execute(&mut **tx)
    .await?;
    sqlx::query!(
        "UPDATE sync_decisions SET series_id = $1 WHERE id = ANY($2)",
        drop,
        &undo.moved_decisions,
    )
    .execute(&mut **tx)
    .await?;
    sqlx::query!(
        "UPDATE sync_match_blocks b SET series_id = $1 \
           FROM jsonb_populate_recordset(NULL::sync_match_blocks, $2) x \
         WHERE b.series_id = $3 AND b.provider = x.provider AND b.external_id = x.external_id",
        drop,
        undo.moved_match_blocks,
        keep,
    )
    .execute(&mut **tx)
    .await?;

    // The candidates this merge closed go back to open. Guarded on the outcome so a pair an
    // operator has dismissed *since* the merge is not silently reopened by the revert.
    sqlx::query!(
        "UPDATE merge_candidates \
            SET resolved = false, outcome = NULL, resolved_by = NULL, resolved_at = NULL, \
                updated_at = now() \
          WHERE id = ANY($1) AND outcome IN ('merged', 'auto_merged')",
        &undo.resolved_candidates,
    )
    .execute(&mut **tx)
    .await?;

    // Undo the path compression, then remove the forwarding address itself — in that order, so
    // there is no instant in which an alias points at a series that no longer forwards.
    sqlx::query!(
        "UPDATE series_merges SET survivor_id = $1 WHERE merged_id = ANY($2)",
        drop,
        &undo.recompressed,
    )
    .execute(&mut **tx)
    .await?;
    sqlx::query!(
        "DELETE FROM series_merges WHERE merged_id = $1 AND survivor_id = $2",
        drop,
        keep,
    )
    .execute(&mut **tx)
    .await?;

    // Both series' feature digests have changed again, and every affected taste profile is
    // derived from a watchlist this transaction has just moved.
    sqlx::query!(
        "INSERT INTO rec_repair_queue (series_id, reason) \
         VALUES ($1, 'merge_reverted'), ($2, 'merge_reverted') \
         ON CONFLICT (series_id) DO NOTHING",
        keep,
        drop,
    )
    .execute(&mut **tx)
    .await?;
    sqlx::query!(
        "UPDATE user_taste_profile p SET stale = true \
          WHERE EXISTS (SELECT 1 FROM watchlist_entries w \
                        WHERE w.user_id = p.user_id AND w.series_id IN ($1, $2))",
        keep,
        drop,
    )
    .execute(&mut **tx)
    .await?;

    Ok(())
}
