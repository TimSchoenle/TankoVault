//! Candidate lookup for series canonicalisation (`docs/ARCHITECTURE.md` §10, step 2), the
//! merge-candidate queue an ambiguous match feeds, and the duplicate sweep.
//!
//! This layer returns raw trigram candidates and performs whatever the caller's
//! [`Canonicaliser`](tankovault_domain::matching::Canonicaliser) decides. Scoring and thresholds
//! live in `tankovault_matcher` and `tankovault_config::MatchingConfig`, so this crate links no
//! scorer.
//!
//! The candidate type is [`tankovault_domain::matching::Candidate`] itself, not a row struct
//! with a `From` impl: a conversion duplicated across the worker's ingest and `services/sync`'s
//! remote-entry resolution would let a new field silently drop out of one of the two paths.
//!
//! # Two ways a duplicate is found
//!
//! [`find_candidates`] is the *create-time* path: it runs while a scanned source is filed and
//! asks whether the series exists. It cannot see what enrichment adds later: authors, release
//! year and alternative titles.
//!
//! [`find_duplicate_pairs`] is the *standing* path. It blocks the whole catalogue on the
//! whitespace-insensitive title key (canonical against canonical, canonical against alias,
//! alias against alias) and returns every pair worth re-scoring with everything now known
//! about both sides.
//!
//! `candidates` and `pairs` find duplicates, `queue` holds the ambiguous ones for an operator,
//! `merge` executes the decision, and `keys` maintains the title keys the first two read.
//! `full_sweep` holds the claim and progress of the operator-triggered run that walks every
//! shortlist to the end. `timing` measures when two series' sources first saw the chapters they
//! share. `undo` takes a merge back, and `disentangle` takes back the shared names that caused
//! it, which a pure inverse cannot reach.

mod candidates;
mod decisions;
mod disentangle;
mod full_sweep;
mod keys;
mod merge;
mod pairs;
mod queue;
mod timing;
mod undo;

/// How many distinct series may hold one title key before it stops identifying anything.
///
/// Two stages read it. In [`find_duplicate_pairs`] a key held by `n` series contributes
/// `n·(n-1)/2` pairs, so it keeps the shortlist from going quadratic. In [`find_candidates`] it
/// stops a runaway: a source attaches on an alias hit, its aliases are filed under that series,
/// and the next source sharing any of them attaches too. A key that many works answer to is no
/// evidence about any of them.
///
/// Sixteen is far above any real duplicate cluster, one work listed by every provider, and far
/// below the thousands a mis-scraped label produces, so the exact value is not load-bearing.
///
/// Kept in step with the `HAVING count(*) > 16` in `migrations/0025_merge_sweep_progress.up.sql`,
/// which applies the same rule once, destructively.
pub(super) const MAX_KEY_FANOUT: i64 = 16;

pub use candidates::{find_candidates, find_candidates_multi};
pub use decisions::{
    MergeDecisionFilter, MergeDecisionRow, NewMergeDecision, RevertReadiness, UndoSegment,
    flag_merge_decision, get_merge_decision, list_merge_decisions, pairs_already_deferred,
    purge_deferred_decisions, record_merge_decisions, revert_merge_decision, revert_order,
};
pub use disentangle::Disentangled;
pub use full_sweep::{
    FullSweepClaim, FullSweepCounters, FullSweepState, advance_full_sweep, claim_full_sweep,
    finish_full_sweep, read_full_sweep_state,
};
pub use keys::{KeyRebuildReport, rebuild_normalized_keys};
pub use merge::{merge_series, resolve_merged_series, resolve_merged_series_batch};
pub use pairs::{
    DistinctVerdict, DuplicatePair, SeriesMatchFacts, distinct_merge_pairs, find_duplicate_pairs,
    find_orphan_twins, open_merge_pairs, pair_similarities, record_distinct_pair,
    record_distinct_pairs, series_match_facts,
};
pub use queue::{
    MergeCandidateView, NewMergeCandidate, QueueOutcome, dismiss_merge_candidate,
    list_open_merge_candidates, record_merge_candidate, record_merge_candidates, suppress_pair,
};
pub use timing::live_timing;
pub use undo::{MergeUndo, UNDO_VERSION, revert_merge};
