//! Offline re-adjudication of journalled sweep decisions.
//!
//! `merge_decisions.evidence` stores both sides' full facts for every sweep decision, so a
//! changed matcher can be measured against every historical verdict without a database. The
//! input is one row per line as exported by query A10 of `docs/MATCHING_OVERHAUL.md`; the
//! committed fixtures under `tests/fixtures/` use the same shape.

use std::collections::BTreeMap;
use std::str::FromStr as _;

use serde::Deserialize;
use tankovault_domain::matching::LiveTiming;
use tankovault_domain::{ContentType, SeriesId};

use crate::decide::adjudicate;
use crate::pair::{PairReading, PairSide, adjudicate_pair, apply_live_timing, wants_live_timing};
use crate::types::{Adjudication, MergeGuards, Thresholds};

/// One exported `merge_decisions` row, reduced to what a replay reads.
#[derive(Debug, Clone, Deserialize)]
pub struct DecisionRow {
    /// The decision's id, absent from hand-written fixtures.
    #[serde(default)]
    pub id: Option<String>,
    /// A fixture's note naming the production case it stands for. Not part of the export.
    #[serde(default)]
    pub case: Option<String>,
    /// `auto` | `review` | `distinct`, as stored.
    pub verdict: String,
    /// The adjudication slug, as stored.
    pub reason: String,
    /// The signal slugs, as stored.
    #[serde(default)]
    pub signals: Vec<String>,
    /// Both sides' facts and the pair's trigram similarity.
    pub evidence: Evidence,
    /// The thresholds and guards in force when the row was decided.
    #[serde(default)]
    pub policy: Option<Policy>,
}

/// The part of `merge_decisions.evidence` a replay reads.
#[derive(Debug, Clone, Deserialize)]
pub struct Evidence {
    /// The pair's trigram similarity as the shortlist computed it.
    #[serde(default)]
    pub trigram_similarity: f32,
    /// The left series' facts.
    pub left: SideFacts,
    /// The right series' facts.
    pub right: SideFacts,
    /// The pair's live chapter timing, when the sweep measured it.
    #[serde(default)]
    pub live_timing: Option<StoredLiveTiming>,
}

/// `evidence.live_timing` as `evidence_json` records it.
#[derive(Debug, Clone, Copy, Deserialize)]
pub struct StoredLiveTiming {
    /// Live chapters the best source pair shares.
    pub shared_live_chapters: i64,
    /// Their median discovery gap in hours.
    pub median_gap_hours: Option<f64>,
}

/// One side's facts as `evidence_json` records them.
#[derive(Debug, Clone, Deserialize)]
pub struct SideFacts {
    /// The series id; absent from hand-written fixtures.
    #[serde(default)]
    pub id: Option<SeriesId>,
    /// Display title.
    #[serde(default)]
    pub title: String,
    /// The canonical matching key.
    pub normalized_title: String,
    /// Alternative matching keys.
    #[serde(default)]
    pub alt_normalized_titles: Vec<String>,
    /// Source listing keys; absent from decisions recorded before they were.
    #[serde(default)]
    pub source_titles: Vec<String>,
    /// The `AniList` id; absent from decisions recorded before it was.
    #[serde(default)]
    pub anilist_id: Option<String>,
    /// The medium slug; anything unrecognised reads as `unknown`.
    #[serde(default)]
    pub content_type: String,
    /// Recorded year.
    #[serde(default)]
    pub release_year: Option<i32>,
    /// Genre names.
    #[serde(default)]
    pub tags: Vec<String>,
    /// Credits.
    #[serde(default)]
    pub authors: Vec<String>,
}

/// `merge_decisions.policy` as `policy_json` records it.
#[derive(Debug, Clone, Copy, Deserialize)]
pub struct Policy {
    /// Ingest attach threshold.
    pub high: f32,
    /// Review floor.
    pub low: f32,
    /// Automatic-merge threshold.
    pub auto_merge: f32,
    /// The guards switched on.
    pub guards: PolicyGuards,
}

/// The guard switches inside [`Policy`].
#[derive(Debug, Clone, Copy, Deserialize)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "a mirror of the stored policy document, one bool per guard key, as MergeGuards is"
)]
pub struct PolicyGuards {
    /// The numeric veto.
    pub numeric_conflict: bool,
    /// The author guard.
    pub author_conflict: bool,
    /// The year guard.
    pub year_conflict: bool,
    /// The medium guard.
    pub type_conflict: bool,
}

impl Policy {
    /// The stored policy as matcher thresholds.
    #[must_use]
    pub const fn thresholds(self) -> Thresholds {
        Thresholds {
            high: self.high,
            low: self.low,
            auto_merge: self.auto_merge,
            guards: MergeGuards {
                numeric_conflict: self.guards.numeric_conflict,
                author_conflict: self.guards.author_conflict,
                year_conflict: self.guards.year_conflict,
                type_conflict: self.guards.type_conflict,
            },
        }
    }
}

impl SideFacts {
    fn side(&self) -> PairSide<'_> {
        PairSide {
            series_id: self.id.unwrap_or_default(),
            normalized_title: &self.normalized_title,
            alt_normalized_titles: &self.alt_normalized_titles,
            source_titles: &self.source_titles,
            anilist_id: self.anilist_id.as_deref(),
            content_type: ContentType::from_str(&self.content_type).unwrap_or(ContentType::Unknown),
            release_year: self.release_year,
            tags: &self.tags,
            authors: &self.authors,
        }
    }
}

/// One row re-judged by the current matcher.
#[derive(Debug, Clone)]
pub struct Replayed {
    /// The winning reading under the current matcher.
    pub reading: PairReading,
    /// The current matcher's verdict.
    pub adjudication: Adjudication,
}

impl Replayed {
    /// The current verdict's stored slug.
    #[must_use]
    pub const fn verdict(&self) -> &'static str {
        self.adjudication.verdict.as_str()
    }
}

/// Parse one exported line.
///
/// # Errors
/// The line is not a JSON object of the [`DecisionRow`] shape.
pub fn parse_row(line: &str) -> Result<DecisionRow, serde_json::Error> {
    serde_json::from_str(line)
}

/// Re-judge `row` with the current matcher, under the policy the row was decided with (the
/// defaults when it recorded none).
#[must_use]
pub fn replay(row: &DecisionRow) -> Replayed {
    let thresholds = row
        .policy
        .map_or_else(Thresholds::default, Policy::thresholds);
    let (mut reading, mut adjudication) = adjudicate_pair(
        row.evidence.left.side(),
        row.evidence.right.side(),
        row.evidence.trigram_similarity,
        thresholds,
    );
    if let Some(stored) = row.evidence.live_timing
        && wants_live_timing(&reading, &adjudication)
    {
        apply_live_timing(
            &mut reading,
            LiveTiming {
                shared_live_chapters: stored.shared_live_chapters,
                median_gap_hours: stored.median_gap_hours,
            },
        );
        adjudication = adjudicate(reading.assessment, thresholds);
    }
    Replayed {
        reading,
        adjudication,
    }
}

/// Counts of stored verdict → replayed verdict.
#[derive(Debug, Clone, Default)]
pub struct Tally {
    transitions: BTreeMap<(String, &'static str), u64>,
}

impl Tally {
    /// Record one replayed row.
    pub fn add(&mut self, row: &DecisionRow, replayed: &Replayed) {
        *self
            .transitions
            .entry((row.verdict.clone(), replayed.verdict()))
            .or_default() += 1;
    }

    /// Every `(stored, replayed, count)`, ordered by stored then replayed verdict.
    pub fn transitions(&self) -> impl Iterator<Item = (&str, &'static str, u64)> {
        self.transitions
            .iter()
            .map(|((old, new), count)| (old.as_str(), *new, *count))
    }

    /// Rows replayed in total.
    #[must_use]
    pub fn total(&self) -> u64 {
        self.transitions.values().sum()
    }

    /// Rows whose replayed verdict equals the stored one.
    #[must_use]
    pub fn unchanged(&self) -> u64 {
        self.transitions
            .iter()
            .filter(|((old, new), _)| old == new)
            .map(|(_, count)| count)
            .sum()
    }
}
