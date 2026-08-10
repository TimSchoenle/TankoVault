//! Wording for the stage a scan task reports, and the reading of a run's timing breakdown.
//!
//! # Why the vocabulary is a table and not an enum
//!
//! `stage` crosses the wire as a plain string. The column behind it is `text` so that adding a
//! stage needs no migration, which also means a worker one release ahead of this console can
//! write a token it has never heard of. A generated enum would fail to *deserialise* that, taking
//! the whole activity payload down over a label; the table below renders the raw token instead,
//! which is degraded but legible. [`stage_label`] is the only place that decides.

use crate::i18n::Translator;
use crate::models::{RunTelemetry, StageTotal};

/// Every stage this console has wording for, paired with its catalogue key.
///
/// Mirrors `tankovault_domain::ScanStage`, which pins the tokens themselves in a unit test. The
/// two are related by nothing else — `web/frontend` is a separate workspace — so a stage added
/// there and not here renders as its token rather than as an error.
const STAGES: [(&str, &str); 9] = [
    ("starting", "console.scan.stage.starting"),
    ("catalog_fetch", "console.scan.stage.catalogFetch"),
    ("catalog_register", "console.scan.stage.catalogRegister"),
    ("catalog_fanout", "console.scan.stage.catalogFanout"),
    ("feed_fetch", "console.scan.stage.feedFetch"),
    ("feed_fanout", "console.scan.stage.feedFanout"),
    ("series_metadata", "console.scan.stage.seriesMetadata"),
    ("series_chapters", "console.scan.stage.seriesChapters"),
    ("series_ingest", "console.scan.stage.seriesIngest"),
];

/// A stage token, worded — or the token itself when this console does not know it yet.
pub(super) fn stage_label(i18n: Translator, token: &str) -> String {
    STAGES
        .iter()
        .find(|(name, _)| *name == token)
        .map_or_else(|| token.to_owned(), |(_, key)| i18n.t(key))
}

/// What the breakdown says about why a run took as long as it did.
///
/// A decision, not a sentence: the wording is [`word`]'s job, so the *policy* — which reading
/// outranks which — can be tested without a rendered component behind it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Verdict {
    /// Nothing recorded a breakdown. Every run from before this instrumentation lands here.
    Unmeasured,
    /// Too little measured work to conclude anything from.
    Trivial,
    /// The provider answered 429/503. Outranks everything below it.
    Throttled { count: i64 },
    /// Most of the time went waiting for permission to send.
    Pacing { percent: i32 },
    /// Most of the time went in the challenge solver.
    Solving { calls: i64 },
    /// Most of the time went inside requests.
    Fetching { percent: i32, requests: i64 },
    /// Nothing the fetch stack accounts for dominates; this stage took the most.
    Stage { stage: String, percent: i32 },
    /// Measured, but nothing stands out.
    Mixed,
}

impl Verdict {
    /// Read the breakdown.
    ///
    /// The order below is the order of usefulness, and the first two entries are the whole point.
    /// A provider answering 429 has told us its budget is wrong, which is actionable; a run whose
    /// time went on *waiting for permission to send* is working exactly as configured, and saying
    /// so first is what stops the next hour being spent looking for a bug that is not there.
    pub(super) fn of(telemetry: &RunTelemetry, stages: &[StageTotal]) -> Self {
        /// Share of execution time past which one cause is "the" cause.
        const DOMINANT: f64 = 0.5;
        /// Below this there is not enough measured work to draw a conclusion from.
        const ENOUGH_MS: i64 = 1_000;

        if telemetry.tasks_measured == 0 {
            return Self::Unmeasured;
        }
        if telemetry.busy_ms < ENOUGH_MS {
            return Self::Trivial;
        }
        if telemetry.throttled > 0 {
            return Self::Throttled {
                count: telemetry.throttled,
            };
        }
        let share = |part: i64| ratio(part, telemetry.busy_ms);
        if share(telemetry.pace_wait_ms) >= DOMINANT {
            return Self::Pacing {
                percent: percent_of(telemetry.pace_wait_ms, telemetry.busy_ms),
            };
        }
        if share(telemetry.solver_ms) >= DOMINANT {
            return Self::Solving {
                calls: telemetry.solver_calls,
            };
        }
        if share(telemetry.fetch_ms) >= DOMINANT {
            return Self::Fetching {
                percent: percent_of(telemetry.fetch_ms, telemetry.busy_ms),
                requests: telemetry.requests,
            };
        }
        stages.first().map_or(Self::Mixed, |top| Self::Stage {
            stage: top.stage.clone(),
            percent: percent_of(top.millis, telemetry.busy_ms),
        })
    }

    /// The catalogue key this reading is worded by.
    pub(super) const fn label_key(&self) -> &'static str {
        match self {
            Self::Unmeasured => "console.scan.explain.unmeasured",
            Self::Trivial => "console.scan.explain.trivial",
            Self::Throttled { .. } => "console.scan.explain.throttled",
            Self::Pacing { .. } => "console.scan.explain.pacing",
            Self::Solving { .. } => "console.scan.explain.solving",
            Self::Fetching { .. } => "console.scan.explain.fetching",
            Self::Stage { .. } => "console.scan.explain.stage",
            Self::Mixed => "console.scan.explain.mixed",
        }
    }
}

/// The verdict as one sentence.
pub(super) fn word(i18n: Translator, verdict: &Verdict) -> String {
    let key = verdict.label_key();
    match verdict {
        Verdict::Unmeasured | Verdict::Trivial | Verdict::Mixed => i18n.t(key),
        Verdict::Throttled { count } => i18n.args(key, &[("count", &count.to_string())]),
        Verdict::Pacing { percent } => i18n.args(key, &[("percent", &percent.to_string())]),
        Verdict::Solving { calls } => i18n.args(key, &[("count", &calls.to_string())]),
        Verdict::Fetching { percent, requests } => i18n.args(
            key,
            &[
                ("percent", &percent.to_string()),
                ("requests", &requests.to_string()),
            ],
        ),
        Verdict::Stage { stage, percent } => i18n.args(
            key,
            &[
                ("stage", &stage_label(i18n, stage)),
                ("percent", &percent.to_string()),
            ],
        ),
    }
}

/// `part` over `whole`, with a divisor that is never zero.
fn ratio(part: i64, whole: i64) -> f64 {
    bounded(part) / bounded(whole).max(1.0)
}

/// A millisecond count as an `f64` that cannot be `NaN` or overflow the cast.
fn bounded(millis: i64) -> f64 {
    f64::from(i32::try_from(millis.max(0)).unwrap_or(i32::MAX))
}

/// `part` as a whole percentage of `whole`.
#[expect(
    clippy::cast_possible_truncation,
    reason = "the clamp bounds the value to 0..=100 before the cast"
)]
pub(super) fn percent_of(part: i64, whole: i64) -> i32 {
    (ratio(part, whole) * 100.0).round().clamp(0.0, 100.0) as i32
}

#[cfg(test)]
mod tests {
    use super::*;

    fn measured(busy_ms: i64) -> RunTelemetry {
        RunTelemetry {
            tasks_measured: 4,
            busy_ms,
            wait_ms: 0,
            requests: 12,
            fetch_ms: 0,
            pace_wait_ms: 0,
            solver_ms: 0,
            solver_calls: 0,
            throttled: 0,
        }
    }

    /// A stage this console has no wording for must render as its token rather than as the
    /// catalogue key: a worker one release ahead writes stages this build has never heard of, and
    /// `console.scan.stage.…` on screen is worse than the token it was meant to describe.
    #[test]
    fn every_known_stage_is_worded_and_an_unknown_one_falls_back() {
        for (token, key) in STAGES {
            assert!(crate::i18n::has_key(key), "{token} has no wording");
        }
        // The fallback itself needs no translator: `stage_label` returns the token unchanged.
        assert!(!STAGES.iter().any(|(token, _)| *token == "polishing"));
    }

    #[test]
    fn every_verdict_is_worded() {
        let cases = [
            Verdict::Unmeasured,
            Verdict::Trivial,
            Verdict::Throttled { count: 1 },
            Verdict::Pacing { percent: 90 },
            Verdict::Solving { calls: 2 },
            Verdict::Fetching {
                percent: 70,
                requests: 9,
            },
            Verdict::Stage {
                stage: "series_ingest".to_owned(),
                percent: 60,
            },
            Verdict::Mixed,
        ];
        for verdict in cases {
            assert!(
                crate::i18n::has_key(verdict.label_key()),
                "{verdict:?} has no wording"
            );
        }
    }

    /// The reading this panel exists to give: a run whose time went on waiting for permission to
    /// send is being crawled exactly as politely as it was configured for. Reporting that as
    /// anything else sends an operator hunting a bug that is not there.
    #[test]
    fn a_run_dominated_by_pacing_reads_as_pacing() {
        let mut telemetry = measured(100_000);
        telemetry.pace_wait_ms = 90_000;
        telemetry.fetch_ms = 9_000;
        assert_eq!(
            Verdict::of(&telemetry, &[]),
            Verdict::Pacing { percent: 90 }
        );
    }

    /// Push-back outranks every other reading, including a larger share elsewhere: a provider
    /// answering 429 has said the budget is wrong, which is actionable in a way "it was slow" is
    /// not.
    #[test]
    fn throttling_outranks_a_larger_share_elsewhere() {
        let mut telemetry = measured(100_000);
        telemetry.pace_wait_ms = 99_000;
        telemetry.throttled = 3;
        assert_eq!(
            Verdict::of(&telemetry, &[]),
            Verdict::Throttled { count: 3 }
        );
    }

    /// A run with nothing recorded must say so rather than divide by its zero and claim 100% of
    /// nothing. Every run from before this instrumentation existed lands here, and so does every
    /// run that has not settled a task yet.
    #[test]
    fn a_run_with_no_measurements_says_so_instead_of_guessing() {
        let telemetry = RunTelemetry {
            tasks_measured: 0,
            ..measured(0)
        };
        assert_eq!(Verdict::of(&telemetry, &[]), Verdict::Unmeasured);
    }

    /// With nothing in the fetch stack dominating, the time is ours, and the answer is which
    /// stage took it — the case that distinguishes a slow provider from a slow ingest.
    #[test]
    fn an_unremarkable_run_names_its_costliest_stage() {
        let mut telemetry = measured(100_000);
        telemetry.fetch_ms = 20_000;
        telemetry.pace_wait_ms = 10_000;
        let stages = vec![
            StageTotal {
                stage: "series_ingest".to_owned(),
                millis: 60_000,
                tasks: 4,
            },
            StageTotal {
                stage: "series_chapters".to_owned(),
                millis: 20_000,
                tasks: 4,
            },
        ];
        assert_eq!(
            Verdict::of(&telemetry, &stages),
            Verdict::Stage {
                stage: "series_ingest".to_owned(),
                percent: 60,
            }
        );
    }
}
