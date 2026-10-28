//! Executing a merge, and resolving a series id that has since been merged away.

use crate::error::DbResult;
use crate::repo::matching::undo::MergeUndo;
use sqlx::{PgExecutor, Postgres, Transaction};
use tankovault_domain::SeriesId;
use uuid::Uuid;

/// Merges `drop_id` into `keep_id` in one transaction and deletes it
/// (`docs/ARCHITECTURE.md` §10).
///
/// Sources are re-parented; titles, tags, watchlist entries, progress, sync state and external
/// mappings are unioned onto the survivor; related merge candidates are resolved. Every
/// child-table move is `ON CONFLICT`, so a retry of a partially applied merge is safe, and read
/// progress keeps whichever side had read further.
///
/// Returns the [`MergeUndo`] journal, the exact inverse of everything this transaction did. The
/// caller persists it with [`record_merge_decisions`](super::record_merge_decisions); a merge
/// whose journal is dropped cannot be reverted.
///
/// # The read-progress merge
///
/// Both frontiers take the furthest of the two rows, and the part frontier is dropped if the
/// merged **whole** frontier covers it (`floor(part) <= whole`). That is the staleness rule
/// [`progress_set`](crate::repo::tracking::progress_set) and
/// [`progress_mark_read`](crate::repo::tracking::progress_mark_read) apply, so all three write
/// paths uphold §A.1 and never store a forbidden pair such as `(6, 4.5)`.
///
/// # Tables that must move with the merge
///
/// `series_sync_overrides`, `sync_history`, `sync_remote_entries`, `sync_decisions`,
/// `sync_match_blocks` and `notification_dedup` all reference `series`. Omitting one silently
/// destroys a user's sync exclusions or sync history, turns matched remote entries unmatched
/// through `ON DELETE SET NULL`, or brings back a title match an operator judged wrong.
///
/// The `UPDATE merge_candidates` is belt-and-braces, since both series columns cascade. What
/// `repo_matching.rs` asserts is that no *unresolved* candidate names a deleted series:
/// [`list_open_merge_candidates`](super::list_open_merge_candidates) inner-joins both sides,
/// so such a row would vanish from the queue while staying open.
///
/// # Errors
/// [`crate::DbError::Conflict`] (409) when `keep_id == drop_id`. [`crate::DbError::NotFound`]
/// (404) when either series is missing, checked with one `count(*) = 2` so a series deleted
/// between two lookups cannot slip through. Otherwise [`crate::DbError::Sqlx`], rolling the whole
/// transaction back.
pub async fn merge_series(
    pool: &sqlx::PgPool,
    keep_id: SeriesId,
    drop_id: SeriesId,
    actor: Option<tankovault_domain::UserId>,
    outcome: &str,
) -> DbResult<MergeUndo> {
    if keep_id == drop_id {
        return Err(crate::error::DbError::Conflict(
            "cannot merge a series into itself".to_owned(),
        ));
    }
    let mut tx = pool.begin().await?;
    let keep = keep_id.as_uuid();
    let drop = drop_id.as_uuid();

    // Both series must exist.
    let exists = sqlx::query_scalar!(
        "SELECT count(*) AS \"count!\" FROM series WHERE id = $1 OR id = $2",
        keep,
        drop,
    )
    .fetch_one(&mut *tx)
    .await?;
    if exists < 2 {
        return Err(crate::error::DbError::NotFound);
    }

    // Read the pre-merge state before touching anything: after the first statement below the
    // database cannot say what it was.
    let mut undo = MergeUndo::new(keep, drop);
    super::undo::capture(&mut tx, &mut undo).await?;

    move_catalogue(&mut tx, &mut undo).await?;
    fold_reader_state(&mut tx, &mut undo).await?;
    move_sync_state(&mut tx, &mut undo).await?;
    move_notification_dedup(&mut tx, &mut undo).await?;
    resolve_candidates(&mut tx, &mut undo, actor, outcome).await?;
    fold_recommendations(&mut tx, &mut undo).await?;
    forward_merged_id(&mut tx, &mut undo, actor).await?;

    // Delete the merged series; residual child rows cascade away.
    sqlx::query!("DELETE FROM series WHERE id = $1", drop)
        .execute(&mut *tx)
        .await?;

    tx.commit().await?;
    Ok(undo)
}

/// Moves the absorbed series' sources, titles, tags and authors onto the survivor.
async fn move_catalogue(tx: &mut Transaction<'_, Postgres>, undo: &mut MergeUndo) -> DbResult<()> {
    let keep = undo.survivor_id;
    let drop = undo.absorbed_id;
    // Sources move wholesale (their global (provider, path) uniqueness is preserved).
    undo.moved_sources = sqlx::query_scalar!(
        "UPDATE series_sources SET series_id = $1 WHERE series_id = $2 RETURNING id",
        keep,
        drop,
    )
    .fetch_all(&mut **tx)
    .await?;

    // The merged series' canonical title becomes an alternative title of the survivor.
    //
    // `RETURNING` under `DO NOTHING` yields only the rows actually created, which is exactly what
    // the undo journal needs: deleting the whole key set on revert would take titles the survivor
    // already answered to.
    let inserted_canonical = sqlx::query_scalar!(
        "INSERT INTO series_titles (series_id, title, normalized, origin) \
         SELECT $1, canonical_title, normalized_title, 'merge' FROM series WHERE id = $2 \
         ON CONFLICT (series_id, normalized) DO NOTHING \
         RETURNING to_jsonb(series_titles)",
        keep,
        drop,
    )
    .fetch_all(&mut **tx)
    .await?;
    let inserted_alternatives = sqlx::query_scalar!(
        "INSERT INTO series_titles (series_id, title, normalized, origin) \
         SELECT $1, title, normalized, 'merge' FROM series_titles WHERE series_id = $2 \
         ON CONFLICT (series_id, normalized) DO NOTHING \
         RETURNING to_jsonb(series_titles)",
        keep,
        drop,
    )
    .fetch_all(&mut **tx)
    .await?;
    undo.inserted_titles = rows_to_json(
        inserted_canonical
            .into_iter()
            .chain(inserted_alternatives)
            .collect(),
    );

    undo.inserted_tags = rows_to_json(
        sqlx::query_scalar!(
            "INSERT INTO series_tags (series_id, tag_id) \
             SELECT $1, tag_id FROM series_tags WHERE series_id = $2 \
             ON CONFLICT DO NOTHING \
             RETURNING to_jsonb(series_tags)",
            keep,
            drop,
        )
        .fetch_all(&mut **tx)
        .await?,
    );

    undo.inserted_authors = rows_to_json(
        sqlx::query_scalar!(
            "INSERT INTO series_authors (series_id, author_id) \
             SELECT $1, author_id FROM series_authors WHERE series_id = $2 \
             ON CONFLICT DO NOTHING \
             RETURNING to_jsonb(series_authors)",
            keep,
            drop,
        )
        .fetch_all(&mut **tx)
        .await?,
    );
    Ok(())
}

/// Folds the readers' watchlist entries and read progress into the survivor's.
async fn fold_reader_state(
    tx: &mut Transaction<'_, Postgres>,
    undo: &mut MergeUndo,
) -> DbResult<()> {
    let keep = undo.survivor_id;
    let drop = undo.absorbed_id;
    // `sync_excluded` and `updated_at` travel with the entry. They did not, and the omission was
    // silent in exactly the way that matters: a reader who had switched a series out of tracker
    // sync found it switched back on by a merge they never saw, with the blanket flag reset to
    // its default while the per-provider override beside it was carefully preserved.
    undo.inserted_watchlist = rows_to_json(
        sqlx::query_scalar!(
            "INSERT INTO watchlist_entries \
                (user_id, series_id, status, notify, added_at, sync_excluded, updated_at) \
             SELECT user_id, $1, status, notify, added_at, sync_excluded, updated_at \
                FROM watchlist_entries WHERE series_id = $2 \
             ON CONFLICT (user_id, series_id) DO NOTHING \
             RETURNING to_jsonb(watchlist_entries)",
            keep,
            drop,
        )
        .fetch_all(&mut **tx)
        .await?,
    );

    sqlx::query!(
        "INSERT INTO read_progress \
            (user_id, series_id, last_read_whole_number, last_read_part_number, updated_at) \
         SELECT user_id, $1, last_read_whole_number, last_read_part_number, updated_at \
            FROM read_progress WHERE series_id = $2 \
         ON CONFLICT (user_id, series_id) DO UPDATE \
            SET last_read_whole_number = \
                    GREATEST(read_progress.last_read_whole_number, EXCLUDED.last_read_whole_number), \
                last_read_part_number = CASE \
                    WHEN floor(GREATEST(COALESCE(read_progress.last_read_part_number, 0), \
                                        COALESCE(EXCLUDED.last_read_part_number, 0))) \
                         <= GREATEST(read_progress.last_read_whole_number, \
                                     EXCLUDED.last_read_whole_number) \
                    THEN NULL \
                    ELSE GREATEST(COALESCE(read_progress.last_read_part_number, 0), \
                                  COALESCE(EXCLUDED.last_read_part_number, 0)) END, \
                updated_at = now()",
        keep,
        drop,
    )
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Moves the absorbed series' tracker-sync state: mappings, overrides, the sync log, decisions,
/// match blocks, remote entries and open conflicts.
async fn move_sync_state(tx: &mut Transaction<'_, Postgres>, undo: &mut MergeUndo) -> DbResult<()> {
    let keep = undo.survivor_id;
    let drop = undo.absorbed_id;
    undo.inserted_mappings = rows_to_json(
        sqlx::query_scalar!(
            "INSERT INTO sync_mappings (series_id, provider, external_id) \
             SELECT $1, provider, external_id FROM sync_mappings WHERE series_id = $2 \
             ON CONFLICT (series_id, provider) DO NOTHING \
             RETURNING to_jsonb(sync_mappings)",
            keep,
            drop,
        )
        .fetch_all(&mut **tx)
        .await?,
    );

    // A user's decision to exclude a series from a tracker is theirs, not the catalogue's, and
    // must survive the catalogue deciding two rows were one. `excluded` is kept if *either*
    // row excluded, because the conservative reading of "do not sync this" is to keep not
    // syncing it.
    sqlx::query!(
        "INSERT INTO series_sync_overrides (user_id, series_id, provider, excluded) \
         SELECT user_id, $1, provider, excluded FROM series_sync_overrides WHERE series_id = $2 \
         ON CONFLICT (user_id, series_id, provider) DO UPDATE \
            SET excluded = series_sync_overrides.excluded OR EXCLUDED.excluded",
        keep,
        drop,
    )
    .execute(&mut **tx)
    .await?;

    // The user-visible sync log. Re-pointed, not unioned: these rows have their own primary key
    // and no uniqueness to collide on.
    undo.moved_history = sqlx::query_scalar!(
        "UPDATE sync_history SET series_id = $1 WHERE series_id = $2 RETURNING id",
        keep,
        drop,
    )
    .fetch_all(&mut **tx)
    .await?;

    // The operator-facing decision journal, likewise. Its foreign key is `SET NULL`, so omitting
    // this would detach the rows from the series they describe: the record survives and stops
    // being findable.
    undo.moved_decisions = sqlx::query_scalar!(
        "UPDATE sync_decisions SET series_id = $1 WHERE series_id = $2 RETURNING id",
        keep,
        drop,
    )
    .fetch_all(&mut **tx)
    .await?;

    // Title matches an operator has already judged wrong. Not moving these lets the next
    // reconciliation re-make a match a human has explicitly rejected.
    undo.moved_match_blocks = rows_to_json(
        sqlx::query_scalar!(
            "UPDATE sync_match_blocks SET series_id = $1 WHERE series_id = $2 \
             RETURNING to_jsonb(sync_match_blocks)",
            keep,
            drop,
        )
        .fetch_all(&mut **tx)
        .await?,
    );

    // Remote tracker entries matched to the absorbed series. Without this the foreign key's
    // `ON DELETE SET NULL` makes them unmatched, and the next pull re-resolves them from the
    // title, the same guess that produced the duplicate.
    undo.moved_remote_entries = rows_to_json(
        sqlx::query_scalar!(
            "UPDATE sync_remote_entries SET series_id = $1 WHERE series_id = $2 \
             RETURNING to_jsonb(sync_remote_entries)",
            keep,
            drop,
        )
        .fetch_all(&mut **tx)
        .await?,
    );

    // Unresolved conflicts move where they can. The partial unique index only admits one open
    // conflict per (user, series, provider, field), so a collision means the survivor already
    // has an open conflict about the same field and the absorbed one is redundant.
    undo.moved_conflicts = sqlx::query_scalar!(
        "UPDATE sync_conflicts SET series_id = $1 WHERE series_id = $2 \
         AND (resolved_at IS NOT NULL \
              OR NOT EXISTS (SELECT 1 FROM sync_conflicts c2 \
                             WHERE c2.user_id = sync_conflicts.user_id \
                               AND c2.series_id = $1 \
                               AND c2.provider = sync_conflicts.provider \
                               AND c2.field = sync_conflicts.field \
                               AND c2.resolved_at IS NULL)) \
         RETURNING id",
        keep,
        drop,
    )
    .fetch_all(&mut **tx)
    .await?;
    Ok(())
}

/// Moves the notification suppression rows, so no reader is notified twice for one chapter.
async fn move_notification_dedup(
    tx: &mut Transaction<'_, Postgres>,
    undo: &mut MergeUndo,
) -> DbResult<()> {
    let keep = undo.survivor_id;
    let drop = undo.absorbed_id;
    // Notification suppression. Without this every watcher of the survivor is re-notified of
    // every chapter the absorbed series already announced.
    undo.inserted_dedup = rows_to_json(
        sqlx::query_scalar!(
            "INSERT INTO notification_dedup (user_id, series_id, chapter_number, created_at) \
             SELECT user_id, $1, chapter_number, created_at FROM notification_dedup WHERE series_id = $2 \
             ON CONFLICT (user_id, series_id, chapter_number) DO NOTHING \
             RETURNING to_jsonb(notification_dedup)",
            keep,
            drop,
        )
        .fetch_all(&mut **tx)
        .await?,
    );
    Ok(())
}

/// Resolves every open merge candidate that names the absorbed series.
async fn resolve_candidates(
    tx: &mut Transaction<'_, Postgres>,
    undo: &mut MergeUndo,
    actor: Option<tankovault_domain::UserId>,
    outcome: &str,
) -> DbResult<()> {
    let drop = undo.absorbed_id;
    // Resolve every open candidate that referenced the vanishing series.
    undo.resolved_candidates = sqlx::query_scalar!(
        "UPDATE merge_candidates \
         SET resolved = true, outcome = $3, resolved_by = $2, resolved_at = now(), \
             updated_at = now() \
         WHERE (series_id = $1 OR candidate_id = $1) AND NOT resolved \
         RETURNING id",
        drop,
        actor.map(tankovault_domain::UserId::as_uuid),
        outcome,
    )
    .fetch_all(&mut **tx)
    .await?;
    Ok(())
}

/// Folds the readers' recommendation feedback, and queues the derived recommendation state to be
/// rebuilt from the folded rows.
async fn fold_recommendations(
    tx: &mut Transaction<'_, Postgres>,
    undo: &mut MergeUndo,
) -> DbResult<()> {
    let keep = undo.survivor_id;
    let drop = undo.absorbed_id;
    // A reader's refusal is theirs, not the catalogue's, and must survive the catalogue deciding
    // two rows were one. Folded **before** the delete, because the cascade would otherwise take
    // it: without this, "never show me this again" is silently undone by an automatic merge the
    // reader never saw. Same rule, and the same reasoning, as `series_sync_overrides` above.
    //
    // `hide_forever` wins either way: a stronger refusal must not be softened by a weaker one on
    // the other side of the merge.
    sqlx::query!(
        "INSERT INTO recommendation_feedback (user_id, series_id, verdict, created_at) \
         SELECT user_id, $1, verdict, created_at FROM recommendation_feedback \
          WHERE series_id = $2 \
         ON CONFLICT (user_id, series_id) DO UPDATE \
            SET verdict = CASE \
                  WHEN recommendation_feedback.verdict = 'hide_forever' \
                    OR EXCLUDED.verdict = 'hide_forever' THEN 'hide_forever' \
                  ELSE EXCLUDED.verdict END",
        keep,
        drop,
    )
    .execute(&mut **tx)
    .await?;

    // Affinity and the taste profile are *derived* from the watchlist and read progress, which
    // this transaction has already folded correctly. Merging the derived rows by hand is how the
    // two diverge; marking the profiles stale makes them be recomputed from the folded truth.
    sqlx::query!(
        "UPDATE user_taste_profile p SET stale = true \
          WHERE EXISTS (SELECT 1 FROM user_series_affinity a \
                        WHERE a.user_id = p.user_id AND a.series_id IN ($1, $2))",
        keep,
        drop,
    )
    .execute(&mut **tx)
    .await?;

    // The survivor absorbed the loser's tags and authors, so its embedding is stale. Queued,
    // not recomputed: re-embedding needs the builder's projection basis, and a merge must not
    // block on it.
    //
    // The loser's model rows cascade with the series row below, so a merged series leaves the
    // index in the transaction that deletes it.
    sqlx::query!(
        "INSERT INTO rec_repair_queue (series_id, reason) VALUES ($1, 'merged') \
         ON CONFLICT (series_id) DO NOTHING",
        keep,
    )
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Records where the absorbed series went, re-pointing every earlier alias at the survivor.
async fn forward_merged_id(
    tx: &mut Transaction<'_, Postgres>,
    undo: &mut MergeUndo,
    actor: Option<tankovault_domain::UserId>,
) -> DbResult<()> {
    let keep = undo.survivor_id;
    let drop = undo.absorbed_id;
    // Where this series went. Written before the DELETE, so the forwarding record and the
    // disappearance are one atomic fact.
    //
    // Path compression, not a chain: when B is absorbed into C, every alias pointing at B is
    // re-pointed at C, so the map stays one hop deep and resolution is one lookup. A walk of
    // A→B→C at read time would be slower and could spin on a cycle. No cycle can form, since the
    // survivor exists and the merged id is deleted.
    //
    // Compression runs before the insert, so the new row is never a candidate for its own
    // rewrite.
    undo.recompressed = sqlx::query_scalar!(
        "UPDATE series_merges SET survivor_id = $1 WHERE survivor_id = $2 RETURNING merged_id",
        keep,
        drop,
    )
    .fetch_all(&mut **tx)
    .await?;

    // `DO UPDATE`, not `DO NOTHING`: if an id that already has a forwarding address is somehow
    // merged again, the address must name where it went *this* time. `DO NOTHING` would keep a
    // stale one.
    sqlx::query!(
        "INSERT INTO series_merges (merged_id, survivor_id, merged_by) \
         VALUES ($1, $2, $3) \
         ON CONFLICT (merged_id) DO UPDATE \
            SET survivor_id = EXCLUDED.survivor_id, \
                merged_at   = now(), \
                merged_by   = EXCLUDED.merged_by",
        drop,
        keep,
        actor.map(tankovault_domain::UserId::as_uuid),
    )
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Collects a `RETURNING to_jsonb(t)` result into the JSON array shape the undo journal stores.
fn rows_to_json(rows: Vec<Option<serde_json::Value>>) -> serde_json::Value {
    serde_json::Value::Array(rows.into_iter().flatten().collect())
}

/// Returns where a series went, if it was merged away.
///
/// `None` for an id that is live or was never known; both mean no forwarding address. One
/// lookup, never a walk, since [`merge_series`] path-compresses on write.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only.
pub async fn resolve_merged_series<'e, E: PgExecutor<'e>>(
    exec: E,
    merged_id: SeriesId,
) -> DbResult<Option<SeriesId>> {
    let survivor = sqlx::query_scalar!(
        "SELECT survivor_id FROM series_merges WHERE merged_id = $1",
        merged_id.as_uuid(),
    )
    .fetch_optional(exec)
    .await?;
    Ok(survivor.map(SeriesId::from_uuid))
}

/// Resolves many ids at once, returning only those that moved.
///
/// The request path resolves a reader's seeds together, where a round trip per seed would be
/// twenty-five queries to learn that usually none moved.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only.
pub async fn resolve_merged_series_batch<'e, E: PgExecutor<'e>>(
    exec: E,
    merged_ids: &[SeriesId],
) -> DbResult<Vec<(SeriesId, SeriesId)>> {
    let ids: Vec<Uuid> = merged_ids.iter().copied().map(SeriesId::as_uuid).collect();
    let rows = sqlx::query!(
        "SELECT merged_id, survivor_id FROM series_merges WHERE merged_id = ANY($1)",
        &ids,
    )
    .fetch_all(exec)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| {
            (
                SeriesId::from_uuid(r.merged_id),
                SeriesId::from_uuid(r.survivor_id),
            )
        })
        .collect())
}
