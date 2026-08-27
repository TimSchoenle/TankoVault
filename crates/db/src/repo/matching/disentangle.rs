//! Undoing, on the survivor, what made two series look like one.
//!
//! [`revert_merge`](super::revert_merge) is the exact inverse of a merge and nothing more, which
//! is the right contract for it and not enough on its own. A merge is *caused* by the two rows
//! sharing a name, and on the alias path the shared name is usually already the damage: attaching
//! a source files its own alternative titles under the series it joined, so a wrong attach leaves
//! the survivor answering to the other work's names and holding the sources those names pulled
//! in. Putting the absorbed row back touches neither — the aliases were on the survivor before
//! the merge, and so were the sources — so an operator who unmerges watches the chapters they
//! were complaining about stay exactly where they were, and watches the next scan re-attach
//! anything the alias still matches.
//!
//! This module is the rest of the action. It runs when an operator reverts, because that is the
//! moment the system is told the two rows are different works.
//!
//! # The shared names are the evidence, and the whole of it
//!
//! No scoring happens here. The pair's disputed names are computable exactly — the normalized
//! keys both rows answer to — and every claim below is a consequence of the operator's judgement
//! applied to that set:
//!
//! - A name both rows answer to cannot identify both, so it comes off the survivor. The restored
//!   row keeps its copy: it is the row the revert vindicated, and it is not the one that acquired
//!   names by absorbing.
//! - A source on the survivor whose provider title *is* one of those names is there because of
//!   one of them. That is the attach path's own mechanism rather than an approximation of it: an
//!   alias-routed attach is discounted to `0.75` and needs a near-identity hit on the alias to
//!   clear the threshold at all, so the sources a junk alias pulled in are the ones titled after
//!   it.
//!
//! Chapters hang off `series_sources`, so they travel with the source rows and need no statement
//! of their own. That indirection is also why the revert alone looked inert: the merge moves the
//! *absorbed* row's sources, of which a hollowed-out row has none.
//!
//! # What is deliberately left alone
//!
//! **The survivor's canonical title**, even when the restored row answers to it too. Two rows
//! colliding on a canonical title are a same-name case, not alias contamination, and stripping a
//! series of its own name to resolve it would be worse than the collision. The suppression
//! recorded alongside keeps the pair from being re-merged, and `xtask repair-series` is the tool
//! for splitting a row whose sources genuinely belong to several works.
//!
//! **Watchlist entries and read progress**, which key on `series_id` and stay with the survivor
//! even when the chapters they refer to move. Re-attributing a reader's progress across a split
//! is a judgement this layer does not make; `xtask repair-series` leaves it alone for the same
//! reason.
//!
//! **Anything scored rather than named.** A source titled `Legend of Star General (Colored)` is
//! not moved by a rule keyed on `legend of star general`. Narrow on purpose: this runs
//! automatically and without review, so it acts only where the operator's judgement settles the
//! answer outright.
//!
//! # Self-healing in the safe direction
//!
//! Deleting an alternative title is the reversible half of this. `add_series_titles` re-inserts
//! every title a provider publishes on each scan, so a name the survivor genuinely goes by comes
//! back on its next scan, while a shredded fragment or a name borrowed from another work does
//! not. The same argument `xtask repair-series` makes for dropping over-shared keys.

use serde::{Deserialize, Serialize};
use sqlx::{Postgres, Transaction};
use tankovault_domain::SeriesId;
use uuid::Uuid;

use crate::error::DbResult;

/// What disentangling one reverted pair changed.
///
/// Reported rather than merely logged: the survivor loses titles and sources without anyone
/// reviewing the list, so the operator who pressed the button is owed the counts, and the merge
/// journal keeps them beside the decision they belong to.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Disentangled {
    /// Alternative titles removed from the survivor because the restored series answers to them
    /// too.
    pub titles_removed: i64,
    /// Sources moved from the survivor back onto the restored series.
    pub sources_returned: i64,
    /// Chapters that travelled with those sources. Counted rather than moved — they hang off
    /// `series_sources` — and it is the number an operator reads as "the wrong entries are gone".
    pub chapters_returned: i64,
}

impl Disentangled {
    /// Whether anything at all changed, for a caller deciding whether to mention it.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.titles_removed == 0 && self.sources_returned == 0
    }
}

/// Strip the names two series share off the survivor, and send back the sources those names
/// attracted.
///
/// Runs inside the revert's own transaction and after the absorbed series is live again: it reads
/// the restored row's titles, so it cannot run before them. Idempotent by construction — a second
/// call finds no shared names, and therefore nothing to remove or move.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only.
pub(super) async fn disentangle(
    tx: &mut Transaction<'_, Postgres>,
    survivor: SeriesId,
    restored: SeriesId,
) -> DbResult<Disentangled> {
    let keep = survivor.as_uuid();
    let back = restored.as_uuid();

    // Every normalized key both rows answer to, canonical titles included, less the survivor's
    // own canonical title — see the module comment for why that one is never taken.
    let shared: Vec<String> = sqlx::query_scalar!(
        "WITH names AS ( \
           SELECT id AS series_id, normalized_title AS key FROM series WHERE id IN ($1, $2) \
           UNION \
           SELECT series_id, normalized FROM series_titles WHERE series_id IN ($1, $2) \
         ) \
         SELECT n.key AS \"key!\" FROM names n \
          WHERE n.series_id = $1 AND n.key <> '' \
            AND EXISTS (SELECT 1 FROM names o WHERE o.series_id = $2 AND o.key = n.key) \
            AND n.key IS DISTINCT FROM (SELECT normalized_title FROM series WHERE id = $1)",
        keep,
        back,
    )
    .fetch_all(&mut **tx)
    .await?;

    if shared.is_empty() {
        return Ok(Disentangled::default());
    }

    let titles_removed = sqlx::query!(
        "DELETE FROM series_titles WHERE series_id = $1 AND normalized = ANY($2)",
        keep,
        &shared,
    )
    .execute(&mut **tx)
    .await?
    .rows_affected();
    let titles_removed = i64::try_from(titles_removed).unwrap_or(i64::MAX);

    let moved: Vec<Uuid> = sqlx::query_scalar!(
        "SELECT id FROM series_sources \
          WHERE series_id = $1 AND provider_title IS NOT NULL \
            AND tv_normalize_title(provider_title) = ANY($2)",
        keep,
        &shared,
    )
    .fetch_all(&mut **tx)
    .await?;

    if moved.is_empty() {
        return Ok(Disentangled {
            titles_removed,
            ..Disentangled::default()
        });
    }

    // Counted before the move: once `series_id` changes, "how many chapters left the survivor" is
    // no longer a question the database can be asked.
    let chapters_returned = sqlx::query_scalar!(
        "SELECT count(*) AS \"count!\" FROM chapters WHERE series_source_id = ANY($1)",
        &moved,
    )
    .fetch_one(&mut **tx)
    .await?;

    // Re-parenting cannot collide: `series_sources` is unique on `(provider_id, source_path)`,
    // which this does not touch.
    sqlx::query!(
        "UPDATE series_sources SET series_id = $1 WHERE id = ANY($2)",
        back,
        &moved,
    )
    .execute(&mut **tx)
    .await?;

    Ok(Disentangled {
        titles_removed,
        sources_returned: i64::try_from(moved.len()).unwrap_or(i64::MAX),
        chapters_returned,
    })
}
