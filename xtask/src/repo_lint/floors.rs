//! Rules over the committed coverage floors: that both parse, under the parser the gate uses.

use std::path::{Path, PathBuf};

use super::Finding;
use crate::coverage::{Floor, Suite, parse_optional_floor};

/// **Both coverage floors must parse, and only the integration one may be `unset`.**
///
/// The two files are read by CI jobs on different paths: `.github/coverage-floor.txt` on every
/// pull request, `.github/coverage-floor-integration.txt` only on the `full` path — the weekly
/// schedule or an explicit dispatch. So a typo in the integration floor is invisible for up to a
/// week, and the run that finds it is the expensive one: an instrumented build of the whole
/// workspace against a Docker Postgres, failing on a line it could have rejected in
/// milliseconds.
///
/// `unset` is rejected for the offline suite here for the same reason [`crate::coverage`] rejects
/// it there: that floor is the only coverage gate on the fast path, and a sentinel would disable
/// it silently while still reporting a green run.
///
/// This rule parses rather than scans, deliberately against the grain of the other rules in this
/// module — reimplementing the format as a text scan would let the check and the gate disagree,
/// which is the class of bug the whole of `repo-lint` exists to catch.
pub(super) fn coverage_floors_parse(root: &Path) -> anyhow::Result<Vec<Finding>> {
    let mut findings = Vec::new();

    for suite in [Suite::Offline, Suite::Integration] {
        let path = PathBuf::from(suite.floor_file());
        let absolute = root.join(&path);
        let contents = std::fs::read_to_string(&absolute)
            .map_err(|e| anyhow::anyhow!("{}: {e}", absolute.display()))?;

        match parse_optional_floor(&contents) {
            Err(e) => findings.push(Finding {
                rule: "coverage-floors-parse",
                file: path,
                line: value_line(&contents),
                detail: format!("does not parse: {e}"),
            }),
            Ok(Floor::Unmeasured) if matches!(suite, Suite::Offline) => findings.push(Finding {
                rule: "coverage-floors-parse",
                file: path,
                line: value_line(&contents),
                detail: "the offline floor may not be `unset`: it is the only coverage gate on \
                         the fast path, so a sentinel disables it while every run still reports \
                         green. Only the integration floor may be unset, and only until its \
                         first measured run."
                    .to_owned(),
            }),
            Ok(_) => {}
        }
    }

    Ok(findings)
}

/// The line a finding points at: the first non-comment, non-blank one, or the last line when the
/// file holds no value at all.
fn value_line(contents: &str) -> usize {
    contents
        .lines()
        .position(|line| {
            let trimmed = line.trim();
            !trimmed.is_empty() && !trimmed.starts_with('#')
        })
        .map_or_else(|| contents.lines().count().max(1), |index| index + 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_value_line_is_the_first_line_that_is_not_prose() {
        assert_eq!(value_line("# a\n\n# b\n28.1\n"), 4);
    }

    /// A file that is all comments has no value line to point at, so the finding lands on the
    /// end of the file rather than on line 0, which no editor will open.
    #[test]
    fn a_file_with_no_value_points_at_its_end() {
        assert_eq!(value_line("# only\n# prose\n"), 2);
    }
}
