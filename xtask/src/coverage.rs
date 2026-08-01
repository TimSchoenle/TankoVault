//! `xtask coverage-ratchet` — the gate on `.github/coverage-floor.txt`. A floor, not a target:
//! moving logic into a module the (excluded) integration suites cover lowers the measured line
//! coverage even though nothing got less tested, so lower the floor in that same commit rather
//! than treat the drop as a regression.

use std::path::{Path, PathBuf};

/// How far below the measured value the floor is expected to sit.
///
/// Not enforced — it is guidance for whoever raises the floor. Large enough to absorb
/// `proptest`'s entropy-seeded runs and a refactor that moves a few covered lines around,
/// small enough that a genuine regression still trips the gate.
pub(crate) const FLOOR_HEADROOM_PCT: f64 = 1.0;

/// The committed floor, relative to the workspace root.
const FLOOR_FILE: &str = ".github/coverage-floor.txt";

/// Read the floor out of `contents`. Blank lines and `#` comments are skipped, so the file can
/// explain itself to whoever changes it.
///
/// # Errors
/// If no numeric line is present, or the first one does not parse.
pub(crate) fn parse_floor(contents: &str) -> anyhow::Result<f64> {
    for line in contents.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        return line.parse::<f64>().map_err(|e| {
            anyhow::anyhow!("the first non-comment line of {FLOOR_FILE} is not a number: {e}")
        });
    }
    anyhow::bail!("{FLOOR_FILE} contains no floor; expected a line like `24.0`")
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
    pub(crate) fn message(&self) -> String {
        match *self {
            Verdict::Ok { measured, floor } => {
                let headroom = measured - floor;
                let headline = format!(
                    "line coverage {measured:.2}% is at or above the floor of {floor:.2}% \
                     (+{headroom:.2})"
                );
                if headroom >= FLOOR_HEADROOM_PCT * 2.0 {
                    format!(
                        "{headline}\nthe floor has {headroom:.2} points of headroom; consider \
                         raising it in {FLOOR_FILE} to about {:.1}",
                        measured - FLOOR_HEADROOM_PCT
                    )
                } else {
                    headline
                }
            }
            Verdict::Regressed { measured, floor } => format!(
                "line coverage {measured:.2}% is below the floor of {floor:.2}%.\n\
                 \n\
                 Either add tests for what this change introduced, or — if the drop is because \
                 code moved into a module the *integration* suites cover, which this measurement \
                 excludes — lower the number in {FLOOR_FILE} in this same commit and say why in \
                 the message. The file is a record of a decision, not a score."
            ),
        }
    }
}

/// Run the gate: read the committed floor, read the report `llvm-cov` just wrote, compare.
///
/// # Errors
/// If either file is missing or malformed, or coverage is below the floor.
pub(crate) fn run(workspace_root: &Path, report_path: &Path) -> anyhow::Result<()> {
    let floor_path: PathBuf = workspace_root.join(FLOOR_FILE);
    let floor = parse_floor(
        &std::fs::read_to_string(&floor_path)
            .map_err(|e| anyhow::anyhow!("reading {}: {e}", floor_path.display()))?,
    )?;

    let report: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(report_path)
            .map_err(|e| anyhow::anyhow!("reading {}: {e}", report_path.display()))?,
    )?;
    let measured = line_coverage(&report)?;

    let verdict = judge(measured, floor);
    println!("{}", verdict.message());
    match verdict {
        Verdict::Ok { .. } => Ok(()),
        Verdict::Regressed { .. } => anyhow::bail!("coverage regressed below the committed floor"),
    }
}

#[cfg(test)]
mod tests {
    #![expect(
        clippy::float_cmp,
        reason = "the floor is a decimal literal parsed back out of its own text, so exact \
                  equality is the property under test — a tolerance here would let the parser \
                  round and still pass"
    )]

    use super::{Verdict, judge, line_coverage, parse_floor};

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
        let message = verdict.message();
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
        assert!(judge(30.0, 24.0).message().contains("consider raising it"));
        assert!(!judge(24.5, 24.0).message().contains("consider raising it"));
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
}
