//! Executing a merge, and resolving a series id that has since been merged away.

use crate::error::DbResult;
use sqlx::PgExecutor;
use tankovault_domain::SeriesId;
use uuid::Uuid;

/// Transactionally merge `drop_id` into `keep_id` (design §10 operator merge): re-parent
/// the merged series' sources, union its titles and tags, migrate user watchlist/progress,
/// sync state and external mappings, resolve any related merge candidates, then delete it. All
/// child-table moves are idempotent (`ON CONFLICT`), and read-progress keeps the furthest
/// point.
///
/// # The read-progress merge
///
/// Both frontiers take the furthest of the two rows, and the part frontier is then dropped if the
/// merged **whole** frontier covers it — the same staleness rule
/// [`progress_set`](crate::repo::tracking::progress_set) and
/// [`progress_mark_read`](crate::repo::tracking::progress_mark_read) apply (`floor(part) <=
/// whole`), so all three write paths uphold §A.1 identically.
///
/// Getting this wrong produces a `(whole, part)` pair §A.1 forbids (e.g. `(6, 4.5)`) that every
/// read model is entitled to assume cannot occur.
///
/// # Tables that must move with the merge
///
/// `series_sync_overrides`, `sync_history`, `sync_remote_entries` and `notification_dedup` all
/// reference `series`; omitting any of them silently destroys a user's per-series sync
/// exclusions and visible sync history, and orphans remote tracker entries matched to the
/// absorbed series (`ON DELETE SET NULL` turns them *unmatched*, re-resolved from scratch on
/// the next pull).
///
/// # Merge candidates
///
/// The `UPDATE merge_candidates` below is belt-and-braces: both of that table's series columns are
/// `ON DELETE CASCADE`, so every row naming `drop_id` is removed by the `DELETE FROM series` that
/// follows regardless. What matters — and what `repo_matching.rs` asserts — is that no *unresolved*
/// candidate is left naming a series that no longer exists, because
/// [`list_open_merge_candidates`] inner-joins both sides and such a row would silently vanish from
/// the operator's queue while staying open in the table.
///
/// # Errors
/// [`crate::DbError::Conflict`] — a 409 — when `keep_id == drop_id`, checked before the
/// transaction opens. [`crate::DbError::NotFound`] — a 404 — when either series is missing,
/// which is one `count(*) = 2` check rather than two lookups so a series deleted between them
/// cannot slip through. Otherwise [`crate::DbError::Sqlx`] from any statement in the
/// transaction, which rolls back whole: a partial merge would leave sources re-parented to a
/// series whose titles and progress had not moved, so there is no partial-success return.
// A straight-line sequence of per-table union inserts reads more clearly as one function
// than split across arbitrary helpers just to dodge the line-count lint.
#[expect(
    clippy::too_many_lines,
    reason = "one straight-line sequence of per-table union inserts; splitting it to satisfy \
              a line count would hide the order the tables must be moved in"
)]
pub async fn merge_series(
    pool: &sqlx::PgPool,
    keep_id: SeriesId,
    drop_id: SeriesId,
    actor: Option<tankovault_domain::UserId>,
    outcome: &str,
) -> DbResult<()> {
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

    // Sources move wholesale (their global (provider, path) uniqueness is preserved).
    sqlx::query!(
        "UPDATE series_sources SET series_id = $1 WHERE series_id = $2",
        keep,
        drop,
    )
    .execute(&mut *tx)
    .await?;

    // The merged series' canonical title becomes an alternative title of the survivor.
    sqlx::query!(
        "INSERT INTO series_titles (series_id, title, normalized) \
         SELECT $1, canonical_title, normalized_title FROM series WHERE id = $2 \
         ON CONFLICT (series_id, normalized) DO NOTHING",
        keep,
        drop,
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query!(
        "INSERT INTO series_titles (series_id, title, normalized) \
         SELECT $1, title, normalized FROM series_titles WHERE series_id = $2 \
         ON CONFLICT (series_id, normalized) DO NOTHING",
        keep,
        drop,
    )
    .execute(&mut *tx)
    .await?;

    sqlx::query!(
        "INSERT INTO series_tags (series_id, tag_id) \
         SELECT $1, tag_id FROM series_tags WHERE series_id = $2 \
         ON CONFLICT DO NOTHING",
        keep,
        drop,
    )
    .execute(&mut *tx)
    .await?;

    sqlx::query!(
        "INSERT INTO series_authors (series_id, author_id) \
         SELECT $1, author_id FROM series_authors WHERE series_id = $2 \
         ON CONFLICT DO NOTHING",
        keep,
        drop,
    )
    .execute(&mut *tx)
    .await?;

    sqlx::query!(
        "INSERT INTO watchlist_entries (user_id, series_id, status, notify, added_at) \
         SELECT user_id, $1, status, notify, added_at FROM watchlist_entries WHERE series_id = $2 \
         ON CONFLICT (user_id, series_id) DO NOTHING",
        keep,
        drop,
    )
    .execute(&mut *tx)
    .await?;

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
    .execute(&mut *tx)
    .await?;

    sqlx::query!(
        "INSERT INTO sync_mappings (series_id, provider, external_id) \
         SELECT $1, provider, external_id FROM sync_mappings WHERE series_id = $2 \
         ON CONFLICT (series_id, provider) DO NOTHING",
        keep,
        drop,
    )
    .execute(&mut *tx)
    .await?;

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
    .execute(&mut *tx)
    .await?;

    // The user-visible sync log. Re-pointed rather than unioned: these rows have their own
    // primary key and no uniqueness to collide on.
    sqlx::query!(
        "UPDATE sync_history SET series_id = $1 WHERE series_id = $2",
        keep,
        drop,
    )
    .execute(&mut *tx)
    .await?;

    // Remote tracker entries already matched to the absorbed series. Without this the FK's
    // `ON DELETE SET NULL` turns them into *unmatched* entries, and the next pull re-resolves
    // them from the title — which is the same guess that produced the duplicate in the first
    // place.
    sqlx::query!(
        "UPDATE sync_remote_entries SET series_id = $1 WHERE series_id = $2",
        keep,
        drop,
    )
    .execute(&mut *tx)
    .await?;

    // Unresolved conflicts move where they can. The partial unique index only admits one open
    // conflict per (user, series, provider, field), so a collision means the survivor already
    // has an open conflict about the same field and the absorbed one is redundant.
    sqlx::query!(
        "UPDATE sync_conflicts SET series_id = $1 WHERE series_id = $2 \
         AND (resolved_at IS NOT NULL \
              OR NOT EXISTS (SELECT 1 FROM sync_conflicts c2 \
                             WHERE c2.user_id = sync_conflicts.user_id \
                               AND c2.series_id = $1 \
                               AND c2.provider = sync_conflicts.provider \
                               AND c2.field = sync_conflicts.field \
                               AND c2.resolved_at IS NULL))",
        keep,
        drop,
    )
    .execute(&mut *tx)
    .await?;

    // Notification suppression. Not moving these re-notifies every watcher of the survivor for
    // every chapter the absorbed series had already announced — which, on an automatic merge, is
    // a mail-out nobody asked for.
    sqlx::query!(
        "INSERT INTO notification_dedup (user_id, series_id, chapter_number, created_at) \
         SELECT user_id, $1, chapter_number, created_at FROM notification_dedup WHERE series_id = $2 \
         ON CONFLICT (user_id, series_id, chapter_number) DO NOTHING",
        keep,
        drop,
    )
    .execute(&mut *tx)
    .await?;

    // Resolve every open candidate that referenced the vanishing series.
    sqlx::query!(
        "UPDATE merge_candidates \
         SET resolved = true, outcome = $3, resolved_by = $2, resolved_at = now(), \
             updated_at = now() \
         WHERE (series_id = $1 OR candidate_id = $1) AND NOT resolved",
        drop,
        actor.map(tankovault_domain::UserId::as_uuid),
        outcome,
    )
    .execute(&mut *tx)
    .await?;

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
    .execute(&mut *tx)
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
    .execute(&mut *tx)
    .await?;

    // The survivor absorbed the loser's tags and authors, so its feature digest has changed and
    // its embedding is stale. Queued rather than recomputed here: re-embedding needs the
    // projection basis, which is the builder's, and a merge must not block on it.
    //
    // The *loser's* model rows need nothing — they cascade with the series row below, which is
    // what makes a merged series unreachable from the index in the same transaction that
    // deletes it rather than at the next build.
    sqlx::query!(
        "INSERT INTO rec_repair_queue (series_id, reason) VALUES ($1, 'merged') \
         ON CONFLICT (series_id) DO NOTHING",
        keep,
    )
    .execute(&mut *tx)
    .await?;

    // Where this series went. Written before the DELETE, so the forwarding record and the
    // disappearance are one atomic fact: there is no instant in which the row is gone and
    // nothing says where to look instead.
    //
    // Path compression, not a chain. When B is absorbed into C, every alias already pointing at
    // B is re-pointed at C in the same statement, so the map stays exactly one hop deep forever
    // and resolution is a single lookup. The alternative — walking A→B→C at read time — is both
    // slower and able to spin on a cycle. Cycles cannot form here: the survivor always exists
    // and the merged id is always deleted, so no id is ever on both sides.
    //
    // Compression runs before the insert. After it, the freshly written row would be a candidate
    // for its own rewrite the next time this predicate changed.
    sqlx::query!(
        "UPDATE series_merges SET survivor_id = $1 WHERE survivor_id = $2",
        keep,
        drop,
    )
    .execute(&mut *tx)
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
    .execute(&mut *tx)
    .await?;

    // Delete the merged series; residual child rows cascade away.
    sqlx::query!("DELETE FROM series WHERE id = $1", drop)
        .execute(&mut *tx)
        .await?;

    tx.commit().await?;
    Ok(())
}

/// Where a series went, if it was merged away.
///
/// Returns `None` for an id that is either still live or was never known — the caller cannot
/// distinguish those and does not need to: both mean "no forwarding address".
///
/// One lookup, never a walk. [`merge_series`] path-compresses on write, so the map is exactly
/// one hop deep and a recursive resolution here would be dead code that only appears correct.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only; an unknown id is `Ok(None)`, not [`crate::DbError::NotFound`].
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

/// Resolve many ids at once, returning only those that actually moved.
///
/// The batch form exists because the request path resolves a reader's seeds together — a
/// per-seed round trip would be twenty-five queries to discover that, usually, none of them
/// moved.
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
