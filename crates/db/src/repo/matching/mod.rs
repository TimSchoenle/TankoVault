//! Candidate lookup for series canonicalisation (design §10, step 2), plus the merge-candidate
//! queue an ambiguous match feeds and the duplicate sweep that keeps it honest.
//!
//! This layer returns raw trigram candidates and performs whatever the caller's
//! [`Canonicaliser`](tankovault_domain::matching::Canonicaliser) decides; the scoring and the
//! thresholds live above it (`tankovault_matcher` and `tankovault_config::MatchingConfig`), so
//! it is unit-testable without a database and this crate links no scorer.
//!
//! The candidate type is [`tankovault_domain::matching::Candidate`] itself rather than a row
//! struct plus a `From` impl: a hand-written conversion duplicated across the worker's ingest
//! canonicalisation and `services/sync`'s remote-entry resolution would let adding a field
//! silently drop that signal from one of the two paths deciding whether two series are the
//! same.
//!
//! # Two ways a duplicate is found
//!
//! [`find_candidates`] is the *create-time* path: it runs while a scanned source is being filed
//! and answers "does this already exist?". It is necessarily blind to anything the catalogue
//! learns later — a series acquires its authors, its release year and its alternative titles
//! from a subsequent enrichment pass, and by then the decision has been taken.
//!
//! [`find_duplicate_pairs`] is the *standing* path, and exists because the first one is not
//! enough. It blocks the whole catalogue on the whitespace-insensitive title key (canonical
//! against canonical, canonical against alias, alias against alias) and hands back every pair
//! worth re-scoring with everything now known about both sides. On a 26k-series catalogue it
//! surfaced 59 pairs with byte-identical compact titles that the create-time path had never
//! queued at all.
//!
//! One module per stage: `candidates` and `pairs` find them, `queue` holds the ambiguous
//! ones for an operator, `merge` executes the decision, and `keys` maintains the normalized
//! title keys the first two read.

mod candidates;
mod decisions;
mod keys;
mod merge;
mod pairs;
mod queue;
mod undo;

pub use candidates::{find_candidates, find_candidates_multi};
pub use decisions::{
    MergeDecisionFilter, MergeDecisionRow, NewMergeDecision, flag_merge_decision,
    list_merge_decisions, record_merge_decision, revert_merge_decision,
};
pub use keys::{KeyRebuildReport, rebuild_normalized_keys};
pub use merge::{merge_series, resolve_merged_series, resolve_merged_series_batch};
pub use pairs::{
    DuplicatePair, SeriesMatchFacts, distinct_merge_pairs, find_duplicate_pairs, open_merge_pairs,
    pair_similarities, record_distinct_pair, series_match_facts,
};
pub use queue::{
    MergeCandidateView, QueueOutcome, dismiss_merge_candidate, list_open_merge_candidates,
    record_merge_candidate, suppress_pair,
};
pub use undo::{MergeUndo, UNDO_VERSION, revert_merge};
