//! Owns what a pass decided about one pair, and why.

use tankovault_db::repo::matching::{DuplicatePair, SeriesMatchFacts};
use tankovault_domain::matching::LiveTiming;
use tankovault_matcher::{Adjudication, PairReading};

/// Which shortlist a pair came from, recorded on its decision so a run can be read back by the
/// question it was answering instead of as one undifferentiated list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Trigger {
    New,
    Requeue,
    Recheck,
}

impl Trigger {
    pub(super) const fn as_str(self) -> &'static str {
        match self {
            Self::New => "sweep_new",
            Self::Requeue => "sweep_requeue",
            Self::Recheck => "sweep_recheck",
        }
    }
}

/// What the judging pass did about a pair, and therefore what the journal has to record.
#[derive(Debug, Clone, Copy)]
pub(super) enum Action<'a> {
    /// Merged. Journalled on the spot, not with the batch, because this is the only
    /// outcome whose record carries an undo; see [`sweep`].
    ///
    /// [`sweep`]: super::sweep
    Merged {
        keep: &'a SeriesMatchFacts,
        drop: &'a SeriesMatchFacts,
    },
    /// The automatic-merge budget was spent before this pair's turn came.
    Deferred,
    /// Offered to an operator. What that did to the queue comes from the batched upsert.
    Review,
    /// Judged apart. Whether it closed an open row comes from the batched upsert.
    Distinct,
}

/// One pair, judged.
///
/// Carries everything the queue writes *and* the journal need, so the action taken and the record
/// of that action are read out of one value instead of rebuilt independently in two places.
pub(super) struct Judged<'a> {
    pub(super) pair: DuplicatePair,
    pub(super) trigger: Trigger,
    pub(super) left: &'a SeriesMatchFacts,
    pub(super) right: &'a SeriesMatchFacts,
    pub(super) trigram: f32,
    pub(super) reading: PairReading,
    /// The pair's live chapter timing, when the reading was worth measuring it for.
    pub(super) live_timing: Option<LiveTiming>,
    pub(super) verdict: Adjudication,
    pub(super) signals: Vec<&'static str>,
    /// The console sentence, for the pairs going to review.
    pub(super) reason: Option<String>,
    pub(super) action: Action<'a>,
}
