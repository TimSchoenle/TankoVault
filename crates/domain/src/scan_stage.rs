//! The stage vocabulary a scan task reports as it runs, and the timing breakdown it leaves
//! behind.
//!
//! `scan_tasks.state` says only queued, claimed or done. That counts a run but cannot explain
//! one: a task claimed for twenty minutes and a wedged task look identical through it. A stage
//! names which of a task's half-dozen steps it is in now, and [`StageTimings`] says where the
//! wall clock went once it is over. Together they tell "this run is slow" from "this run spent
//! 94% of its time waiting on the provider's crawl budget".

use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;
use utoipa::ToSchema;

/// Raised when a stored stage token names no stage.
#[derive(Debug, Clone, thiserror::Error)]
#[error("invalid scan stage: {0:?}")]
pub struct ParseStageError(pub String);

/// What a scan task is doing right now.
///
/// Stored as `text`, not a Postgres enum: a stage is a diagnostic label, and adding one must not
/// need a migration and a lock on `scan_tasks`. In exchange an unknown token can reach a reader,
/// and [`ScanStage::from_str`] surfaces it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ScanStage {
    /// Claimed, resolving the provider row and building its fetch stack.
    Starting,
    /// Fetching one catalogue page from the provider.
    CatalogFetch,
    /// Registering the page's series as source stubs.
    CatalogRegister,
    /// Persisting and publishing the page's per-series child tasks.
    CatalogFanout,
    /// Fetching the provider's latest-updates feed.
    FeedFetch,
    /// Persisting and publishing a child task per series the feed named.
    FeedFanout,
    /// Fetching one series' metadata page.
    SeriesMetadata,
    /// Fetching one series' chapter list.
    SeriesChapters,
    /// Writing one series' metadata and chapters into the catalogue.
    SeriesIngest,
}

impl ScanStage {
    /// Returns the token this stage is stored and published as.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Starting => "starting",
            Self::CatalogFetch => "catalog_fetch",
            Self::CatalogRegister => "catalog_register",
            Self::CatalogFanout => "catalog_fanout",
            Self::FeedFetch => "feed_fetch",
            Self::FeedFanout => "feed_fanout",
            Self::SeriesMetadata => "series_metadata",
            Self::SeriesChapters => "series_chapters",
            Self::SeriesIngest => "series_ingest",
        }
    }

    /// Returns every stage, in the order a task would pass through them.
    #[must_use]
    pub const fn all() -> &'static [Self] {
        &[
            Self::Starting,
            Self::CatalogFetch,
            Self::CatalogRegister,
            Self::CatalogFanout,
            Self::FeedFetch,
            Self::FeedFanout,
            Self::SeriesMetadata,
            Self::SeriesChapters,
            Self::SeriesIngest,
        ]
    }

    /// Returns whether this stage's wall clock is spent on the provider's side.
    ///
    /// Drives how the console explains a slow run: time in a network stage is the provider's
    /// crawl budget being spent, which is working as intended; the same wall clock in
    /// [`Self::SeriesIngest`] is our own database and is not.
    #[must_use]
    pub const fn is_network(self) -> bool {
        matches!(
            self,
            Self::CatalogFetch | Self::FeedFetch | Self::SeriesMetadata | Self::SeriesChapters
        )
    }
}

impl fmt::Display for ScanStage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for ScanStage {
    type Err = ParseStageError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::all()
            .iter()
            .copied()
            .find(|stage| stage.as_str() == s)
            .ok_or_else(|| ParseStageError(s.to_owned()))
    }
}

/// Where one task's wall clock went, in milliseconds per stage, plus what the fetch stack spent
/// it on.
///
/// Written once when the task settles, so instrumenting a task costs one extra column on one
/// existing UPDATE.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct StageTimings {
    /// Milliseconds per stage, keyed by [`ScanStage::as_str`]. Stages the task never entered are
    /// absent, not zero: "did not happen" and "was instant" are different answers.
    #[serde(default)]
    pub stages: std::collections::BTreeMap<String, i64>,
    /// HTTP requests the task issued, across every stage.
    #[serde(default)]
    pub requests: i64,
    /// Milliseconds spent inside a request, from the moment the pacer released it.
    #[serde(default)]
    pub fetch_ms: i64,
    /// Milliseconds spent waiting for permission to send: the concurrency gate, the token rate,
    /// the crawl delay and any adaptive 429 penalty.
    ///
    /// A task at 95% pace-wait is not slow but polite, and the remedy is the provider's crawl
    /// budget, not the code.
    #[serde(default)]
    pub pace_wait_ms: i64,
    /// Milliseconds spent in the challenge solver.
    #[serde(default)]
    pub solver_ms: i64,
    /// Challenge solves the task needed.
    #[serde(default)]
    pub solver_calls: i64,
    /// Responses the provider answered with a throttling status (429/503), each of which widens
    /// this provider's spacing for every later request.
    #[serde(default)]
    pub throttled: i64,
}

impl StageTimings {
    /// Adds `millis` to `stage`'s running total.
    pub fn add_stage(&mut self, stage: ScanStage, millis: i64) {
        *self.stages.entry(stage.as_str().to_owned()).or_insert(0) += millis;
    }

    /// Returns the stage that consumed the most wall clock, with its milliseconds.
    #[must_use]
    pub fn dominant_stage(&self) -> Option<(&str, i64)> {
        self.stages
            .iter()
            .max_by_key(|(_, millis)| **millis)
            .map(|(stage, millis)| (stage.as_str(), *millis))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The tokens are what the database stores and the console renders, and nothing relates the
    /// two but this string. A renamed variant that kept its old token is invisible; a renamed
    /// token silently orphans every row already written with the old one.
    #[test]
    fn every_stage_round_trips_through_its_token() {
        for &stage in ScanStage::all() {
            assert_eq!(ScanStage::from_str(stage.as_str()).unwrap(), stage);
            let json = serde_json::to_string(&stage).unwrap();
            assert_eq!(json, format!("\"{}\"", stage.as_str()));
        }
    }

    #[test]
    fn no_two_stages_share_a_token() {
        let mut tokens: Vec<&str> = ScanStage::all().iter().map(|s| s.as_str()).collect();
        let count = tokens.len();
        tokens.sort_unstable();
        tokens.dedup();
        assert_eq!(tokens.len(), count);
    }

    #[test]
    fn an_unknown_token_is_an_error_rather_than_a_default() {
        assert!(ScanStage::from_str("polishing").is_err());
    }

    /// The ingest stage is ours; the fetch stages are the provider's. The console words a slow
    /// run differently for each, so a stage classified on the wrong side tells an operator to go
    /// looking in the wrong place.
    #[test]
    fn only_the_provider_facing_stages_count_as_network() {
        assert!(ScanStage::SeriesChapters.is_network());
        assert!(ScanStage::CatalogFetch.is_network());
        assert!(!ScanStage::SeriesIngest.is_network());
        assert!(!ScanStage::CatalogFanout.is_network());
    }

    #[test]
    fn the_dominant_stage_is_the_one_with_the_most_wall_clock() {
        let mut timings = StageTimings::default();
        timings.add_stage(ScanStage::SeriesMetadata, 400);
        timings.add_stage(ScanStage::SeriesChapters, 900);
        timings.add_stage(ScanStage::SeriesChapters, 200);
        assert_eq!(timings.dominant_stage(), Some(("series_chapters", 1_100)));
    }

    /// A stage the task never entered must not serialise as zero: the console reads an absent
    /// stage as "did not happen" and a zero as "was instant", and those are different diagnoses.
    #[test]
    fn an_unvisited_stage_is_absent_rather_than_zero() {
        let mut timings = StageTimings::default();
        timings.add_stage(ScanStage::SeriesIngest, 12);
        let encoded = serde_json::to_value(&timings).unwrap();
        assert!(encoded["stages"].get("series_metadata").is_none());
        assert_eq!(encoded["stages"]["series_ingest"], 12);
    }
}
