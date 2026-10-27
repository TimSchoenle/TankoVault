//! Each workflow's concurrency group belongs to that workflow alone.

use crate::repo_lint::Finding;
use crate::repo_lint::text::is_comment;
use std::path::{Path, PathBuf};

/// **No concurrency group may be declared by more than one workflow.** GitHub keeps at most *one*
/// pending run per `concurrency.group`. A run entering a group that already has one running and
/// one pending does not queue behind them — it cancels the pending run outright. So a shared group
/// silently drops a run on every event that fires its members twice, and which run is dropped is
/// decided by run-creation order: nothing a reader of the workflow files can see, and nothing that
/// shows up as a failure. The dropped run is reported as `cancelled`, which is what a run that was
/// superseded on purpose also looks like.
///
/// `auto-fix.yaml`, `auto-format.yaml` and `update-lockfile.yaml` shared `pr-autocommit-<pr>`
/// until 2026-08-04. On release 1.2.1, `release-please` force-pushed its release commit while
/// `auto-fix` was three minutes into a 33-minute run; `auto-format` took the pending slot and the
/// lockfile sync was cancelled one second after it was created. `Cargo.lock`,
/// `web/frontend/Cargo.lock` and `openapi.json` reached `main` still recording 1.2.0 against a
/// 1.2.1 manifest, and every `--locked` build failed — the release images included.
///
/// Two members was the first answer and it was the same bug with a smaller constant. On release
/// 1.3.0 (#76) release-please force-pushed twice in 33 seconds; `auto-fix` was cancelled on both
/// events, `auto-format` — its one group-mate — survived, and `test (workspace)` failed with
/// `openapi.json is out of date`. Workflows that must not lose a run therefore share nothing:
/// `auto-fix.yaml` is now the single member of `pr-autocommit-<pr>`, and one is what this rule
/// allows.
pub(in crate::repo_lint) fn concurrency_groups_hold_one_workflow(
    root: &Path,
) -> anyhow::Result<Vec<Finding>> {
    const RULE: &str = "concurrency-groups-hold-one-workflow";
    const WORKFLOWS: &str = ".github/workflows";

    let dir = root.join(WORKFLOWS);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        anyhow::bail!("repo-lint: cannot read {}", dir.display());
    };

    let mut by_group: std::collections::BTreeMap<String, Vec<(String, usize)>> =
        std::collections::BTreeMap::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension == "yml" || extension == "yaml")
        {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            anyhow::bail!("repo-lint: cannot read {}", path.display());
        };
        let Some((line, group)) = workflow_concurrency_group(&text) else {
            continue;
        };
        // `${{ github.workflow }}` expands to the workflow's own name, so a group containing it
        // is per-workflow however identical the source text looks. `ci.yml`, `security.yml` and
        // `release-please.yaml` all declare `${{ github.workflow }}-${{ github.ref }}` and share
        // nothing.
        if group.contains("github.workflow") {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        by_group.entry(group).or_default().push((name, line));
    }

    let mut findings = Vec::new();
    for (group, mut members) in by_group {
        if members.len() < 2 {
            continue;
        }
        members.sort();
        let names: Vec<&str> = members.iter().map(|(name, _)| name.as_str()).collect();
        let (first, line) = &members[0];
        findings.push(Finding {
            rule: RULE,
            file: PathBuf::from(format!("{WORKFLOWS}/{first}")),
            line: *line,
            detail: format!(
                "`{group}` is declared by {} workflows ({}); GitHub queues one pending run per \
                 group and cancels the pending one when another arrives, so an event that fires \
                 them more than once loses a run without reporting a failure",
                members.len(),
                names.join(", ")
            ),
        });
    }
    Ok(findings)
}

/// The top-level `concurrency.group` of one workflow, and the line it is on.
///
/// Top-level only: a `concurrency:` nested under a job scopes to that job's own runs, which is
/// not what the rule above is about. Column zero is what distinguishes them.
fn workflow_concurrency_group(text: &str) -> Option<(usize, String)> {
    let mut inside = false;
    for (index, line) in text.lines().enumerate() {
        if is_comment(line) || line.trim().is_empty() {
            continue;
        }
        if !line.starts_with([' ', '\t']) {
            if line.trim_end() == "concurrency:" {
                inside = true;
                continue;
            }
            // Any other unindented key ends the block, whether or not it carried a `group:`.
            inside = false;
            continue;
        }
        if inside && let Some(value) = line.trim().strip_prefix("group:") {
            return Some((index + 1, value.trim().trim_matches(['"', '\'']).to_owned()));
        }
    }
    None
}
