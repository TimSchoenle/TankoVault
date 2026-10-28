//! Owns the rule that release-please tags the merged release before it proposes the next one.

use super::PUBLISH_WORKFLOW;
use super::yaml::{action_steps, job_at, job_lines};
use crate::repo_lint::Finding;
use crate::repo_lint::text::is_comment;
use std::path::{Path, PathBuf};

/// **release-please tags a release in one pass and proposes the next one in another, with the
/// draft's publish in between.** The action does both halves in a single process by default, and
/// the two disagree about when a tag exists: `draft: true` means the release the tagging half
/// creates carries no git tag until `desktop-release` publishes it, while the pull-request half
/// resolves "the previous release" by looking that tag up.
///
/// Run together, the pull-request half looks for a tag its own predecessor has not created yet,
/// finds nothing, and falls back to the version in `.release-please-manifest.json` with **no base
/// commit to bound the commit walk**. The version is therefore right and the range is the entire
/// history of the repository: the run logs `No latest release found`, considers every
/// commit, and opens a changelog re-listing every commit ever merged with a major bump (from
/// re-counting `!` commits released long before) for a `main` with nothing new on it.
///
/// Nothing else reports this. The action succeeds, the release publishes normally, and
/// `auto-merge-release-please.yml` merges the bogus pull request on schedule.
///
/// Both halves are checked, not just the ordering: an invocation that skips neither is the
/// bug restored, and one that skips both is a workflow that silently stops releasing.
pub(in crate::repo_lint) fn release_please_tags_before_it_proposes(
    root: &Path,
) -> anyhow::Result<Vec<Finding>> {
    const RULE: &str = "release-please-tags-before-it-proposes";
    const ACTION: &str = "googleapis/release-please-action";
    const SKIP_PULL_REQUEST: &str = "skip-github-pull-request: true";
    const SKIP_RELEASE: &str = "skip-github-release: true";
    /// The one line that clears the draft bit, and so the one that creates the tag.
    const PUBLISHES_THE_DRAFT: &str = "--draft=false";

    let path = root.join(PUBLISH_WORKFLOW);
    let Ok(text) = std::fs::read_to_string(&path) else {
        anyhow::bail!("repo-lint: cannot read {}", path.display());
    };

    let mut findings = Vec::new();
    let mut tagging = Vec::new();
    let mut proposing = Vec::new();
    for (line, body) in action_steps(&text, ACTION) {
        let skips_pull_request = body.iter().any(|step| step.contains(SKIP_PULL_REQUEST));
        let skips_release = body.iter().any(|step| step.contains(SKIP_RELEASE));
        match (skips_pull_request, skips_release) {
            (true, false) => tagging.push(line),
            (false, true) => proposing.push(line),
            (false, false) => findings.push(Finding {
                rule: RULE,
                file: PathBuf::from(PUBLISH_WORKFLOW),
                line,
                detail: format!(
                    "this `{ACTION}` step neither skips the release nor skips the pull request, \
                     so it opens one against a draft it has just created and has not tagged; the \
                     changelog is then the whole history and the bump is a major"
                ),
            }),
            (true, true) => findings.push(Finding {
                rule: RULE,
                file: PathBuf::from(PUBLISH_WORKFLOW),
                line,
                detail: format!("this `{ACTION}` step skips both halves and does nothing"),
            }),
        }
    }

    if tagging.len() != 1 || proposing.len() != 1 {
        findings.push(Finding {
            rule: RULE,
            file: PathBuf::from(PUBLISH_WORKFLOW),
            line: 0,
            detail: format!(
                "expected exactly one `{ACTION}` step with `{SKIP_PULL_REQUEST}` and one with \
                 `{SKIP_RELEASE}`, found {} and {}",
                tagging.len(),
                proposing.len()
            ),
        });
        return Ok(findings);
    }

    let publish = text
        .lines()
        .position(|line| !is_comment(line) && line.contains(PUBLISHES_THE_DRAFT))
        .map(|index| index + 1);
    let Some(publish) = publish else {
        anyhow::bail!(
            "repo-lint: no line in {} runs `{PUBLISHES_THE_DRAFT}`, so nothing publishes the draft \
             and this rule cannot tell which job creates the tag",
            path.display()
        );
    };

    let (Some(publisher), Some(proposer)) = (job_at(&text, publish), job_at(&text, proposing[0]))
    else {
        anyhow::bail!(
            "repo-lint: cannot place the draft publish or the release pull request in a job of {}",
            path.display()
        );
    };

    let waits = job_lines(&text, &proposer)
        .iter()
        .any(|(_, line)| line.trim_start().starts_with("needs:") && line.contains(&publisher));
    if !waits {
        findings.push(Finding {
            rule: RULE,
            file: PathBuf::from(PUBLISH_WORKFLOW),
            line: proposing[0],
            detail: format!(
                "`{proposer}` does not list `{publisher}` in its `needs:`, so the release pull \
                 request can be built before `{PUBLISHES_THE_DRAFT}` has created the tag it \
                 resolves the previous release from"
            ),
        });
    }
    Ok(findings)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The bug this pins: `release-please.yaml` invoked the action once, so it created the draft
    /// release (which carries no git tag until `desktop-release` publishes it) and then, in the
    /// same process, looked for that tag to work out what the *next* release contained. It found
    /// nothing (`No latest release found for path: .`), took the version from the manifest and
    /// the commit range from the beginning of history, and opened a changelog re-listing the whole
    /// repository with a major bump from re-counting `!` commits released long before, for a
    /// `main` with no new commits at all.
    ///
    /// Nothing downstream would have caught it: the action succeeded, the release published
    /// normally, and `auto-merge-release-please.yml` merges release pull requests on a schedule.
    #[test]
    fn a_release_please_step_that_does_not_split_its_two_halves_is_caught() {
        const ACTION: &str = "googleapis/release-please-action";

        // The broken shape: one invocation, doing both halves, before the tag exists.
        let single = [
            "jobs:",
            "  release-please:",
            "    steps:",
            "      - uses: googleapis/release-please-action@0000000 # v5",
            "        with:",
            "          config-file: release-please-config.json",
        ]
        .join("\n");
        let steps = action_steps(&single, ACTION);
        assert_eq!(steps.len(), 1);
        assert!(!steps[0].1.iter().any(|line| line.contains("skip-github")));

        // The fix: tag, publish the draft, then propose; one half each, in that order.
        let split = [
            "jobs:",
            "  release-please:",
            "    steps:",
            "      - uses: googleapis/release-please-action@0000000 # v5",
            "        with:",
            "          skip-github-pull-request: true",
            "  desktop-release:",
            "    needs: [release-please]",
            "    steps:",
            "      - run: gh release edit \"$TAG\" --draft=false",
            "  release-pr:",
            "    needs: [release-please, desktop-release]",
            "    steps:",
            "      - uses: googleapis/release-please-action@0000000 # v5",
            "        with:",
            "          # `skip-github-pull-request: true` is the tagging pass, not this one",
            "          skip-github-release: true",
        ]
        .join("\n");
        let steps = action_steps(&split, ACTION);
        assert_eq!(steps.len(), 2);

        let skips = |body: &[&str], key: &str| body.iter().any(|line| line.contains(key));
        assert!(skips(&steps[0].1, "skip-github-pull-request: true"));
        assert!(!skips(&steps[0].1, "skip-github-release: true"));
        // The step body stops at the next job, so the tagging pass cannot borrow the other half's
        // input, and a comment naming that input is prose, not a second skip.
        assert!(skips(&steps[1].1, "skip-github-release: true"));
        assert!(!skips(&steps[1].1, "skip-github-pull-request: true"));

        // The ordering the rule is actually about: the pull-request pass sits in a job that waits
        // for the one clearing the draft bit, which is what creates the tag it resolves against.
        let publish = split
            .lines()
            .position(|line| line.contains("--draft=false"))
            .unwrap()
            + 1;
        assert_eq!(job_at(&split, publish).as_deref(), Some("desktop-release"));
        assert_eq!(job_at(&split, steps[1].0).as_deref(), Some("release-pr"));
        assert!(job_lines(&split, "release-pr").iter().any(|(_, line)| {
            line.trim_start().starts_with("needs:") && line.contains("desktop-release")
        }));
    }
}
