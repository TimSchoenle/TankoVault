//! `xtask coverage-ratchet` — the gate on the committed coverage floors. A floor, not a target:
//! moving logic into a module only the (excluded) integration suites cover lowers the measured
//! line coverage of the offline suite even though nothing got less tested, so lower that floor in
//! the same commit rather than treat the drop as a regression.
//!
//! **Two floors, two files, two CI jobs.** The fast `coverage` job never builds `--features
//! integration`, so it cannot see the Docker-backed suites at all and a regression in code only
//! they reach moves no number it prints. The `coverage-integration` job measures the same
//! workspace *with* those suites and is enforced against its own file. Merging the two into one
//! number would be wrong in both directions — the fast path would have to wait on Docker, and the
//! slow path's value would be diluted by a measurement that cannot see half the tests.

use std::path::{Path, PathBuf};

/// How far below the measured value a floor is expected to sit.
///
/// Not enforced — it is guidance for whoever raises a floor. Large enough to absorb
/// `proptest`'s entropy-seeded runs and a refactor that moves a few covered lines around,
/// small enough that a genuine regression still trips the gate.
pub(crate) const FLOOR_HEADROOM_PCT: f64 = 1.0;

/// The one non-numeric value a floor file may hold: "report what you measured, enforce nothing".
const UNSET_SENTINEL: &str = "unset";

/// Which measurement a floor governs.
///
/// The two are taken by different jobs over different test sets, so a value from one says nothing
/// about the other. They live in separate files for exactly that reason: one file holding both
/// numbers is one copy-paste away from enforcing the integration figure against the offline run,
/// which would fail every pull request until somebody deleted the gate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Suite {
    /// `cargo llvm-cov --workspace --all-targets`, no `integration` feature — the fast path.
    Offline,
    /// The same sweep with `--features integration`, so the Docker-backed suites are counted.
    Integration,
}

impl Suite {
    /// The committed floor for this measurement, relative to the workspace root.
    const fn floor_file(self) -> &'static str {
        match self {
            Suite::Offline => ".github/coverage-floor.txt",
            Suite::Integration => ".github/coverage-floor-integration.txt",
        }
    }

    /// Where the matching CI job writes its `llvm-cov` report. Deliberately distinct, so a local
    /// run of one suite is never judged against the report the other left in `target/`.
    const fn default_report(self) -> &'static str {
        match self {
            Suite::Offline => "target/llvm-cov/coverage.json",
            Suite::Integration => "target/llvm-cov/coverage-integration.json",
        }
    }

    /// What the number covers, so a log line cannot be mistaken for the other suite's.
    const fn label(self) -> &'static str {
        match self {
            Suite::Offline => "offline suite, no `integration` feature",
            Suite::Integration => "workspace with `--features integration`",
        }
    }

    /// The command that re-runs this gate.
    const fn command(self) -> &'static str {
        match self {
            Suite::Offline => "cargo run -p xtask -- coverage-ratchet",
            Suite::Integration => "cargo run -p xtask -- coverage-ratchet --integration",
        }
    }

    /// What to do about a drop. The offline floor has one legitimate reason to be lowered that
    /// the integration floor does not have, and offering it under the wrong measurement would
    /// invite lowering a floor for a reason that cannot apply to it.
    fn lowering_guidance(self) -> String {
        let file = self.floor_file();
        match self {
            Suite::Offline => format!(
                "Either add tests for what this change introduced, or — if the drop is because \
                 code moved into a module the *integration* suites cover, which this measurement \
                 excludes — lower the number in {file} in this same commit and say why in the \
                 message. The file is a record of a decision, not a score."
            ),
            Suite::Integration => format!(
                "This measurement already runs the Docker-backed suites, so the offline floor's \
                 escape hatch has no counterpart here. Either add tests, or — if code was deleted \
                 or a suite deliberately retired — lower the number in {file} in this same commit \
                 and say why in the message. The file is a record of a decision, not a score."
            ),
        }
    }
}

/// A committed floor, which for a measurement nobody has taken yet may be absent on purpose.
#[derive(Debug, PartialEq)]
pub(crate) enum Floor {
    /// The file holds the `unset` sentinel.
    Unmeasured,
    /// A committed percentage.
    At(f64),
}

/// The first line that is neither blank nor a `#` comment, so a floor file can explain itself to
/// whoever changes it.
fn first_value_line(contents: &str) -> Option<&str> {
    contents
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && !line.starts_with('#'))
}

/// Read a numeric floor out of `contents`.
///
/// # Errors
/// If no numeric line is present, or the first one does not parse.
pub(crate) fn parse_floor(contents: &str) -> anyhow::Result<f64> {
    let line =
        first_value_line(contents).ok_or_else(|| anyhow::anyhow!("no floor; expected `24.0`"))?;
    line.parse::<f64>()
        .map_err(|e| anyhow::anyhow!("the first non-comment line is not a number: {e}"))
}

/// Read a floor that is allowed to be the `unset` sentinel instead of a number.
///
/// Only `unset` is accepted as a word: anything else non-numeric is an error, so a typo cannot
/// read as "no floor" and quietly turn the gate off.
///
/// # Errors
/// If the first non-comment line is neither `unset` nor a number, or there is none.
pub(crate) fn parse_optional_floor(contents: &str) -> anyhow::Result<Floor> {
    if first_value_line(contents).is_some_and(|line| line.eq_ignore_ascii_case(UNSET_SENTINEL)) {
        return Ok(Floor::Unmeasured);
    }
    parse_floor(contents).map(Floor::At)
}

/// Pull the total line-coverage percentage out of `cargo llvm-cov report --json`. Line coverage,
/// not region or function coverage: region coverage is noisy under refactoring (moves when a
/// `match` gains an unexercised arm) and function coverage weights a getter like a merge engine.
///
/// # Errors
/// If the document is not the shape `llvm-cov` produces.
pub(crate) fn line_coverage(report: &serde_json::Value) -> anyhow::Result<f64> {
    report["data"][0]["totals"]["lines"]["percent"]
        .as_f64()
        .ok_or_else(|| {
            anyhow::anyhow!(
                "no `data[0].totals.lines.percent` in the llvm-cov report; run \
                 `cargo llvm-cov report --json --summary-only`"
            )
        })
}

/// The verdict, separated from the I/O so it can be tested without either.
#[derive(Debug, PartialEq)]
pub(crate) enum Verdict {
    /// At or above the floor, with the headroom that has accumulated.
    Ok { measured: f64, floor: f64 },
    /// Below the floor.
    Regressed { measured: f64, floor: f64 },
    /// No floor is committed yet. The measurement is reported so it can be committed and nothing
    /// is enforced — the one state in which this gate passes without comparing anything, and the
    /// only alternative to inventing a starting number nobody measured.
    Unmeasured { measured: f64 },
}

/// Compare `measured` against `floor`.
///
/// Equality passes. A floor is a minimum, and a change that lands exactly on it has not made
/// anything worse — failing there would make the gate depend on floating-point noise in the
/// last digit rather than on the property it is checking.
#[must_use]
pub(crate) fn judge(measured: f64, floor: f64) -> Verdict {
    if measured >= floor {
        Verdict::Ok { measured, floor }
    } else {
        Verdict::Regressed { measured, floor }
    }
}

impl Verdict {
    /// What to print. Written for the person reading a red CI log, so it says what to do rather
    /// than only what happened.
    #[must_use]
    pub(crate) fn message(&self, suite: Suite) -> String {
        let label = suite.label();
        let file = suite.floor_file();
        match *self {
            Verdict::Ok { measured, floor } => {
                let headroom = measured - floor;
                let headline = format!(
                    "line coverage {measured:.2}% ({label}) is at or above the floor of \
                     {floor:.2}% (+{headroom:.2})"
                );
                if headroom >= FLOOR_HEADROOM_PCT * 2.0 {
                    format!(
                        "{headline}\nthe floor has {headroom:.2} points of headroom; consider \
                         raising it in {file} to about {:.1}",
                        measured - FLOOR_HEADROOM_PCT
                    )
                } else {
                    headline
                }
            }
            Verdict::Regressed { measured, floor } => format!(
                "line coverage {measured:.2}% ({label}) is below the floor of {floor:.2}%.\n\
                 \n\
                 {}",
                suite.lowering_guidance()
            ),
            Verdict::Unmeasured { measured } => format!(
                "line coverage {measured:.2}% ({label}) measured; {file} holds `{UNSET_SENTINEL}`, \
                 so nothing was enforced.\n\
                 \n\
                 This is the value that file is waiting for. Replace `{UNSET_SENTINEL}` with about \
                 {:.1} — a point of margin below what was measured — record the date and the \
                 figure in its comments, and this gate starts holding. Re-run with `{}`.",
                measured - FLOOR_HEADROOM_PCT,
                suite.command()
            ),
        }
    }
}

/// Run the gate: read the committed floor, read the report `llvm-cov` just wrote, compare.
///
/// # Errors
/// If either file is missing or malformed, or coverage is below the floor.
pub(crate) fn run(workspace_root: &Path, suite: Suite, report_path: &Path) -> anyhow::Result<()> {
    let floor_path: PathBuf = workspace_root.join(suite.floor_file());
    let floor = parse_optional_floor(
        &std::fs::read_to_string(&floor_path)
            .map_err(|e| anyhow::anyhow!("reading {}: {e}", floor_path.display()))?,
    )
    .map_err(|e| anyhow::anyhow!("{}: {e}", floor_path.display()))?;

    let report: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(report_path)
            .map_err(|e| anyhow::anyhow!("reading {}: {e}", report_path.display()))?,
    )?;
    let measured = line_coverage(&report)?;

    let verdict = match floor {
        // The offline floor was measured on 2026-07-30 and has gated every pull request since.
        // Accepting the sentinel there would disable the only coverage gate the fast path has,
        // and it would do it silently, in a file whose whole job is to be a number.
        Floor::Unmeasured if suite == Suite::Offline => anyhow::bail!(
            "{} holds `{UNSET_SENTINEL}`; only the integration floor may be unset. Put the \
             measured percentage back.",
            floor_path.display()
        ),
        Floor::Unmeasured => Verdict::Unmeasured { measured },
        Floor::At(floor) => judge(measured, floor),
    };

    println!("{}", verdict.message(suite));
    match verdict {
        Verdict::Ok { .. } | Verdict::Unmeasured { .. } => Ok(()),
        Verdict::Regressed { .. } => anyhow::bail!("coverage regressed below the committed floor"),
    }
}

/// Parse `coverage-ratchet`'s arguments: an optional `--integration`, then an optional report
/// path that defaults per suite.
///
/// # Errors
/// On an unknown flag or a second positional argument.
pub(crate) fn parse_args(args: &[String]) -> anyhow::Result<(Suite, PathBuf)> {
    const USAGE: &str = "usage: xtask coverage-ratchet [--integration] [report.json]";

    let mut suite = Suite::Offline;
    let mut report: Option<&str> = None;
    for arg in args {
        match arg.as_str() {
            "--integration" => suite = Suite::Integration,
            flag if flag.starts_with("--") => {
                anyhow::bail!("unknown flag {flag:?}; {USAGE}");
            }
            path if report.is_none() => report = Some(path),
            extra => anyhow::bail!("unexpected argument {extra:?}; {USAGE}"),
        }
    }

    let report = report.unwrap_or_else(|| suite.default_report());
    Ok((suite, PathBuf::from(report)))
}

/// The `coverage-ratchet` entry point.
///
/// # Errors
/// Whatever [`parse_args`] or [`run`] fails with.
pub(crate) fn run_cli(workspace_root: &Path, args: &[String]) -> anyhow::Result<()> {
    let (suite, report) = parse_args(args)?;
    run(workspace_root, suite, &report)
}

#[cfg(test)]
mod tests {
    #![expect(
        clippy::float_cmp,
        reason = "the floor is a decimal literal parsed back out of its own text, so exact \
                  equality is the property under test — a tolerance here would let the parser \
                  round and still pass"
    )]

    use super::{
        Floor, Suite, Verdict, judge, line_coverage, parse_args, parse_floor, parse_optional_floor,
    };

    /// `parse_args` takes owned strings because `std::env::args` yields them.
    fn args(raw: &[&str]) -> Vec<String> {
        raw.iter().map(|a| (*a).to_owned()).collect()
    }

    #[test]
    fn the_floor_file_may_explain_itself() {
        let contents = "# what this is\n#\n# and why\n\n24.0\n";
        assert_eq!(parse_floor(contents).expect("parses"), 24.0);
    }

    /// A file with no number must be an error, not a floor of zero. A floor of zero passes
    /// forever, which is the one outcome a ratchet must never silently produce.
    #[test]
    fn a_floor_file_with_no_number_is_an_error() {
        assert!(parse_floor("# nothing here\n\n").is_err());
        assert!(parse_floor("").is_err());
        assert!(parse_floor("about twenty per cent\n").is_err());
    }

    /// Equality passes. A change landing exactly on the floor has made nothing worse, and
    /// failing there would make the gate turn on the last digit of a float.
    #[test]
    fn landing_exactly_on_the_floor_passes() {
        assert_eq!(
            judge(24.0, 24.0),
            Verdict::Ok {
                measured: 24.0,
                floor: 24.0
            }
        );
    }

    #[test]
    fn a_drop_below_the_floor_fails_and_says_what_to_do() {
        let verdict = judge(23.9, 24.0);
        assert!(matches!(verdict, Verdict::Regressed { .. }));
        let message = verdict.message(Suite::Offline);
        assert!(message.contains("coverage-floor.txt"), "{message}");
        assert!(
            message.contains("integration"),
            "the message has to name the one legitimate reason to lower the floor, or the \
             obvious response to a red gate is to lower it for the wrong reason: {message}"
        );
    }

    /// Accumulated headroom is reported, so the floor rises deliberately rather than never.
    /// A ratchet that only ever holds its original line stops being a ratchet.
    #[test]
    fn substantial_headroom_suggests_raising_the_floor() {
        assert!(
            judge(30.0, 24.0)
                .message(Suite::Offline)
                .contains("consider raising it")
        );
        assert!(
            !judge(24.5, 24.0)
                .message(Suite::Offline)
                .contains("consider raising it")
        );
    }

    /// The report shape is `llvm-cov`'s, and reading it wrong must be an error rather than a
    /// zero — a zero would pass every floor and the gate would be inert.
    #[test]
    fn the_report_shape_is_checked_rather_than_defaulted() {
        let good = serde_json::json!({
            "data": [{ "totals": { "lines": { "percent": 24.85 } } }]
        });
        let read = line_coverage(&good).expect("reads");
        assert!(
            (read - 24.85).abs() < 1e-9,
            "the percentage must come through unrounded: got {read}"
        );

        assert!(line_coverage(&serde_json::json!({})).is_err());
        assert!(line_coverage(&serde_json::json!({ "data": [] })).is_err());
        assert!(
            line_coverage(&serde_json::json!({
                "data": [{ "totals": { "regions": { "percent": 29.0 } } }]
            }))
            .is_err(),
            "a report carrying only region coverage must not be read as line coverage"
        );
    }

    /// The integration floor starts as a sentinel because nobody can measure it without Docker
    /// and an instrumented build; the alternative is a number somebody guessed, which is the
    /// failure this whole file exists to avoid.
    #[test]
    fn a_floor_may_be_explicitly_unset() {
        assert_eq!(
            parse_optional_floor("# explains itself\n\nunset\n").expect("parses"),
            Floor::Unmeasured
        );
        assert_eq!(
            parse_optional_floor("UNSET\n").expect("parses"),
            Floor::Unmeasured
        );
        assert_eq!(
            parse_optional_floor("62.5\n").expect("parses"),
            Floor::At(62.5)
        );
    }

    /// Only the exact word disables enforcement. Any other prose is an error, so `# TBD`,
    /// `not yet` or a mistyped sentinel fails loudly instead of reading as "no floor" — a
    /// sentinel that matches loosely is a gate anyone can switch off with a typo.
    #[test]
    fn nothing_but_the_sentinel_reads_as_unset() {
        for contents in ["not yet\n", "tbd\n", "unset it\n", "un-set\n", ""] {
            assert!(
                parse_optional_floor(contents).is_err(),
                "{contents:?} must not read as an unset floor"
            );
        }
    }

    /// An unset floor reports the value to commit rather than passing silently, or the sentinel
    /// would be a permanent no-op nobody ever noticed was still there.
    #[test]
    fn an_unmeasured_floor_reports_the_value_to_commit() {
        let message = Verdict::Unmeasured { measured: 63.42 }.message(Suite::Integration);
        assert!(message.contains("63.42"), "{message}");
        assert!(
            message.contains("coverage-floor-integration.txt"),
            "the message must name the file to edit: {message}"
        );
        assert!(
            message.contains("62.4"),
            "a suggested value one point below the measurement is the whole point of the \
             report: {message}"
        );
    }

    /// The two floors must never be read from, written to, or reported against the same place.
    /// They measure different test sets, so one value substituted for the other is either a gate
    /// that cannot pass or a gate that cannot fail.
    #[test]
    fn the_two_suites_share_no_paths() {
        assert_ne!(Suite::Offline.floor_file(), Suite::Integration.floor_file());
        assert_ne!(
            Suite::Offline.default_report(),
            Suite::Integration.default_report()
        );
        let offline = judge(23.9, 24.0).message(Suite::Offline);
        let integration = judge(60.0, 62.0).message(Suite::Integration);
        assert!(
            !offline.contains("coverage-floor-integration.txt"),
            "{offline}"
        );
        assert!(
            integration.contains("coverage-floor-integration.txt"),
            "{integration}"
        );
    }

    /// The offline floor's "the integration suites cover it" escape hatch cannot apply to a
    /// measurement that *is* the integration suites; printing it there would license lowering
    /// the floor for a reason that is never true of it.
    #[test]
    fn the_integration_message_does_not_offer_the_offline_escape_hatch() {
        let message = judge(60.0, 62.0).message(Suite::Integration);
        assert!(
            !message.contains("which this measurement excludes"),
            "{message}"
        );
    }

    /// The bare invocation must keep meaning what it meant before the second floor existed:
    /// the CI step is `cargo run -p xtask -- coverage-ratchet` with no arguments at all, and a
    /// change of default there would silently judge the offline run against the wrong file.
    #[test]
    fn the_bare_invocation_is_still_the_offline_suite() {
        let (suite, report) = parse_args(&args(&[])).expect("parses");
        assert_eq!(suite, Suite::Offline);
        assert_eq!(report.to_str(), Some("target/llvm-cov/coverage.json"));

        let (suite, report) = parse_args(&args(&["custom.json"])).expect("parses");
        assert_eq!(suite, Suite::Offline);
        assert_eq!(report.to_str(), Some("custom.json"));
    }

    #[test]
    fn the_integration_flag_selects_the_other_floor_and_report() {
        let (suite, report) = parse_args(&args(&["--integration"])).expect("parses");
        assert_eq!(suite, Suite::Integration);
        assert_eq!(
            report.to_str(),
            Some("target/llvm-cov/coverage-integration.json")
        );

        // Order-independent, because a run: block is edited by hand.
        for raw in [
            ["--integration", "custom.json"],
            ["custom.json", "--integration"],
        ] {
            let (suite, report) = parse_args(&args(&raw)).expect("parses");
            assert_eq!(suite, Suite::Integration);
            assert_eq!(report.to_str(), Some("custom.json"));
        }
    }

    /// A mistyped flag must fail rather than be swallowed as a report path: `--integrations`
    /// silently measuring the offline suite is the exact bug the second floor exists to prevent.
    #[test]
    fn an_unknown_flag_is_rejected() {
        assert!(parse_args(&args(&["--integrations"])).is_err());
        assert!(parse_args(&args(&["--all-features"])).is_err());
        assert!(parse_args(&args(&["a.json", "b.json"])).is_err());
    }
}
