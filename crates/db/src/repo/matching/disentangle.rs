//! Undoing, on the survivor, what made two series look like one.
//!
//! [`revert_merge`](super::revert_merge) is the exact inverse of a merge and nothing more. A
//! merge is *caused* by the two rows sharing a name, and on the alias path the shared name is
//! already the damage: attaching a source files its alternative titles under the series it
//! joined, so a wrong attach leaves the survivor answering to the other work's names and holding
//! the sources those names pulled in. The inverse touches neither, so without this the chapters
//! stay where they were and the next scan re-attaches anything the alias still matches. It runs
//! when an operator reverts, the moment the system learns the two rows are different works.
//!
//! # The shared names are the whole evidence
//!
//! No scoring happens here. The disputed names are exactly the normalized keys both rows
//! answer to:
//!
//! - A name both rows answer to cannot identify both, so it comes off the survivor. The
//!   restored row keeps its copy, since it did not acquire names by absorbing.
//! - A source on the survivor whose provider title *is* one of those names is there because of
//!   it. An alias-routed attach is discounted to `0.75` and needs a near-identity hit on the
//!   alias to clear the threshold, so the sources a junk alias pulled in are the ones titled
//!   after it.
//!
//! Chapters hang off `series_sources`, so they travel with the source rows.
//!
//! # What is left alone
//!
//! **The survivor's canonical title**, even when the restored row answers to it too. Two rows
//! sharing a canonical title are a same-name case, and stripping a series of its own name would
//! be worse than the collision. The recorded suppression keeps the pair apart, and
//! `xtask repair-series` splits a row whose sources belong to several works.
//!
//! **Watchlist entries and read progress**, which key on `series_id` and stay with the
//! survivor. Re-attributing a reader's progress across a split is a judgement this layer does
//! not make.
//!
//! **Anything scored, not named.** A source titled `Legend of Star General (Colored)` is not
//! moved by a rule keyed on `legend of star general`. This runs without review, so it acts only
//! where the operator's judgement settles the answer outright.
//!
//! Deleting an alternative title is reversible: `add_series_titles` re-inserts every title a
//! provider publishes on each scan, so a name the survivor really goes by comes back, while a
//! fragment or a name borrowed from another work does not.

use serde::{Deserialize, Serialize};
use sqlx::{Postgres, Transaction};
use tankovault_domain::SeriesId;
use uuid::Uuid;

use crate::error::DbResult;

/// What disentangling one reverted pair changed.
///
/// Reported, not only logged: the survivor loses titles and sources without review, so the
/// operator gets the counts and the merge journal keeps them beside the decision.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Disentangled {
    /// Alternative titles removed from the survivor because the restored series answers to them
    /// too.
    pub titles_removed: i64,
    /// Sources moved from the survivor back onto the restored series.
    pub sources_returned: i64,
    /// Chapters that travelled with those sources, the number an operator reads as "the wrong
    /// entries are gone". They hang off `series_sources`, so they are counted, not moved.
    pub chapters_returned: i64,
}

impl Disentangled {
    /// Returns whether anything at all changed, for a caller deciding whether to mention it.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.titles_removed == 0 && self.sources_returned == 0
    }
}

/// Strips the names two series share off the survivor, and sends back the sources those names
/// attracted.
///
/// Runs inside the revert's transaction, after the absorbed series is live again, since it reads
/// the restored row's titles. Idempotent: a second call finds no shared names.
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
    // own canonical title, which the module docs explain is never taken.
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

    // Counted before the move: once `series_id` changes, the database cannot say how many
    // chapters left the survivor.
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
