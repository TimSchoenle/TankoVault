//! Owns the standing duplicate sweep: finds series the catalogue holds twice, merges the certain
//! ones, and queues the rest for review, using richer signals than were available when the series
//! were first ingested.
//!
//! Every pair the sweep acts on is journalled in `merge_decisions` with the itemised score, the
//! rule that decided it, the guards that held it back and, for a merge, the undo record that
//! takes it back. See `Decision::record`.

mod journal;
mod judgement;
mod orphans;
mod pass;
mod run;
#[cfg(test)]
mod tests;

pub(crate) use run::{SweepBudget, SweepRun, SweepScope, sweep, sweep_all_detached};
