//! Owns the rule that the build epoch is declared once and reaches every image build.

use super::yaml::{is_job_header, workflow_env_value};
use crate::repo_lint::Finding;
use crate::repo_lint::text::is_comment;
use std::path::{Path, PathBuf};

/// **Every workflow that builds an image declares one constant `SOURCE_DATE_EPOCH`, at workflow
/// level, and nothing else names it.** buildx propagates the variable from its environment into
/// the solve as `build-arg:SOURCE_DATE_EPOCH`, so its value is part of the cache key of every
/// stage in `deploy/docker/Dockerfile`. There is no channel that reaches the exporter (where
/// `rewrite-timestamp=true` needs it) without also reaching the build.
///
/// So a *derived* epoch cannot be reproducibility for free; it is a guaranteed cache miss, and it
/// fails in both directions. Passed as `--build-arg SOURCE_DATE_EPOCH=$(git log -1 --format=%ct)`
/// it reaches the warm-up and both publishing builds, which agree with each other and with nothing
/// `ci.yml` wrote, so the warm-up cold-compiles the workspace on every release (31 minutes). Set as
/// a step `env:` on the publishing build alone (on the belief that an environment value stops at
/// the exporter) it inverts the failure: the warm-up is a 28-second import and each of the eighteen
/// legs recompiles the whole workspace instead, 24 minutes apiece. Both spellings are the same wire
/// format, which is why this rule reads the value instead of the syntax.
///
/// The three checks are one rule because any of them alone is satisfiable while broken: agreeing
/// on `${{ steps.epoch.outputs.value }}` in both files is textual agreement and per-commit drift,
/// and a correct workflow-level constant is undone by one step that overrides it.
pub(in crate::repo_lint) fn the_build_epoch_is_one_constant(
    root: &Path,
) -> anyhow::Result<Vec<Finding>> {
    const RULE: &str = "build-epoch-is-one-constant";
    const WORKFLOWS: &str = ".github/workflows";
    /// Present in a workflow that builds images, and in no other.
    const BUILD_ACTION: &str = "docker/build-push-action";
    const EPOCH: &str = "SOURCE_DATE_EPOCH";

    let dir = root.join(WORKFLOWS);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        anyhow::bail!("repo-lint: cannot read {}", dir.display());
    };

    let mut findings = Vec::new();
    let mut declared: Vec<(String, usize, String)> = Vec::new();
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
        if !text
            .lines()
            .any(|line| !is_comment(line) && line.contains(BUILD_ACTION))
        {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        let file = PathBuf::from(format!("{WORKFLOWS}/{name}"));
        let declaration = workflow_env_value(&text, EPOCH);

        match &declaration {
            None => findings.push(Finding {
                rule: RULE,
                file: file.clone(),
                line: 0,
                detail: format!(
                    "builds images but declares no workflow-level `{EPOCH}`; buildx passes \
                     whatever it finds in the environment as `build-arg:{EPOCH}`, so a workflow \
                     without one writes and reads cache keys that no other workflow can match"
                ),
            }),
            Some((line, value)) if value.contains("${{") => findings.push(Finding {
                rule: RULE,
                file: file.clone(),
                line: *line,
                detail: format!(
                    "`{EPOCH}` is `{value}`; it is part of every stage's cache key, so it has to \
                     be a literal constant — an expression makes each run look up a key no other \
                     run wrote"
                ),
            }),
            Some((line, value)) => declared.push((name.clone(), *line, value.clone())),
        }

        // Anything naming the epoch outside its one declaration overrides it for that step
        // (`build-args:` and a step `env:` alike), which is the failure the constant exists to
        // prevent.
        let declared_line = declaration.map(|(line, _)| line).unwrap_or_default();
        for (index, line) in text.lines().enumerate() {
            if index + 1 == declared_line || is_comment(line) || !line.contains(EPOCH) {
                continue;
            }
            // Removing it is the one exception, and only in a job that runs no buildx: there the
            // value reaches no cache key, while leaving it set breaks every packager that sets
            // its own timestamps. Inside a job that *does* build, an `unset` is the same
            // regression by subtraction (buildx then passes no `build-arg` at all, so the build
            // misses every layer a build carrying the constant exported), and is still reported.
            if line.trim() == format!("unset {EPOCH}")
                && !job_builds_images(&text, index, BUILD_ACTION)
            {
                continue;
            }
            findings.push(Finding {
                rule: RULE,
                file: file.clone(),
                line: index + 1,
                detail: format!(
                    "`{EPOCH}` is set a second time here; the workflow-level constant is the only \
                     declaration allowed, because a build carrying a different value reuses no \
                     layer any other build cached"
                ),
            });
        }
    }

    // Cross-file agreement, reported once against the first file in name order, not n times.
    declared.sort();
    if let Some(((_, _, first), rest)) = declared.split_first()
        && let Some((name, line, value)) = rest.iter().find(|(_, _, value)| value != first)
    {
        findings.push(Finding {
            rule: RULE,
            file: PathBuf::from(format!("{WORKFLOWS}/{name}")),
            line: *line,
            detail: format!(
                "`{EPOCH}` is `{value}` here and `{first}` in {}; the two workflows write and \
                 read the same GHCR cache tags, so a build under either value misses every layer \
                 the other exported",
                declared
                    .iter()
                    .map(|(name, _, _)| name.as_str())
                    .find(|other| *other != name)
                    .unwrap_or("the other workflow")
            ),
        });
    }
    Ok(findings)
}

/// Whether the job the given line index falls in runs `build_action`.
///
/// Textual, like the rest of this module: a job is a two-space-indented key under `jobs:`, and
/// everything nested inside one is indented further, so the nearest such key at or above the line
/// is its job and the block runs to the next one. A line that sits above the first job header
/// counts as building, so an unrecognised layout is judged conservatively instead of waved
/// through.
fn job_builds_images(text: &str, line: usize, build_action: &str) -> bool {
    let Some(start) = text
        .lines()
        .take(line + 1)
        .enumerate()
        .filter(|(_, candidate)| is_job_header(candidate))
        .map(|(index, _)| index)
        .last()
    else {
        return true;
    };

    text.lines()
        .skip(start + 1)
        .take_while(|candidate| !is_job_header(candidate))
        .any(|candidate| !is_comment(candidate) && candidate.contains(build_action))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repo_lint::tempdir;

    /// The bug this pins, in both of its shapes. Passed as a build arg,
    /// `SOURCE_DATE_EPOCH=$(git log -1 --format=%ct)` left the release warm-up nothing `ci.yml`
    /// wrote to import, so it cold-compiled the workspace every time. The same value in a step
    /// `env:` on the publishing build looks as if it reaches only the exporter, but buildx passes
    /// it as `build-arg:SOURCE_DATE_EPOCH` either way, so all eighteen legs recompiled instead: 24 minutes each, against a `builder` the
    /// warm-up had just imported in 28 seconds.
    ///
    /// The reader has to find the top-level declaration and *only* that one: a job- or step-level
    /// `env:` is the override the rule reports, so reading one as the declaration would make the
    /// rule report nothing while looking straight at the bug.
    #[test]
    fn only_a_top_level_literal_epoch_counts_as_the_declaration() {
        let workflow = "env:\n  RUSTFLAGS: \"-D warnings\"\n  SOURCE_DATE_EPOCH: \"0\"\n\njobs:\n";
        assert_eq!(
            workflow_env_value(workflow, "SOURCE_DATE_EPOCH"),
            Some((3, "0".to_owned()))
        );

        // The derived shape: workflow level, but not a literal. Read, and reported by the caller.
        let derived = "env:\n  SOURCE_DATE_EPOCH: ${{ steps.epoch.outputs.value }}\n";
        assert_eq!(
            workflow_env_value(derived, "SOURCE_DATE_EPOCH"),
            Some((2, "${{ steps.epoch.outputs.value }}".to_owned()))
        );

        // A step `env:` is nested, so it is not the declaration: the caller reports it as the
        // override it is.
        let step =
            "jobs:\n  build:\n    steps:\n      - env:\n          SOURCE_DATE_EPOCH: \"7\"\n";
        assert_eq!(workflow_env_value(step, "SOURCE_DATE_EPOCH"), None);

        // An unindented key ends the block; a later top-level `env:` is a different workflow's
        // shape, but a key after `jobs:` must not be read as one.
        let after = "env:\n  A: b\n\njobs:\n  x:\n    SOURCE_DATE_EPOCH: \"9\"\n";
        assert_eq!(workflow_env_value(after, "SOURCE_DATE_EPOCH"), None);

        // A comment describing the rule (including this one's own rationale in both workflows)
        // is not a declaration.
        let documented = "env:\n  # SOURCE_DATE_EPOCH: \"0\" was derived once; never again\n";
        assert_eq!(workflow_env_value(documented, "SOURCE_DATE_EPOCH"), None);

        // A prefix match is not a match: a different key must not satisfy the rule.
        let prefixed = "env:\n  SOURCE_DATE_EPOCH_NOTE: \"0\"\n";
        assert_eq!(workflow_env_value(prefixed, "SOURCE_DATE_EPOCH"), None);
    }

    /// The bug this pins: the desktop job in `release-please.yaml` inherited the workflow-level
    /// `SOURCE_DATE_EPOCH`, and appimagetool passes a `-mkfs-time` of its own to mksquashfs,
    /// which refuses a timestamp from the command line and the environment at once. The
    /// `AppImage` leg died as `linuxdeploy failed with exit code Some(1)` and took every
    /// installer for the release with it. The fix removes the variable in that one job, so this
    /// rule may not treat every line naming the epoch outside its declaration as an override.
    ///
    /// The exception has to stay job-scoped. Ahead of a buildx step an `unset` is the
    /// cache-key bug written backwards: buildx then passes no `build-arg` at all, so the
    /// build reuses no layer that a build carrying the constant exported.
    #[test]
    fn an_unset_is_allowed_only_in_a_job_that_builds_no_image() {
        const HEADER: &str = "env:\n  SOURCE_DATE_EPOCH: \"0\"\n\njobs:\n";
        const BUILD: &str = "  build:\n    steps:\n      - uses: docker/build-push-action@v6\n";

        let root = tempdir("build-epoch-unset");
        std::fs::create_dir_all(root.join(".github/workflows")).unwrap();
        let write = |body: &str| {
            std::fs::write(root.join(".github/workflows/release.yaml"), body).unwrap();
        };

        // The shape of the fix: removed in the job that bundles the desktop client, which runs
        // no buildx and so writes no cache key.
        write(&format!(
            "{HEADER}  desktop:\n    steps:\n      - run: |\n          unset \
             SOURCE_DATE_EPOCH\n          dx bundle\n{BUILD}"
        ));
        assert!(the_build_epoch_is_one_constant(&root).unwrap().is_empty());

        // The same line one job over, where it would cost every layer of the image build below.
        write(&format!(
            "{HEADER}  build:\n    steps:\n      - run: |\n          unset \
             SOURCE_DATE_EPOCH\n      - uses: docker/build-push-action@v6\n"
        ));
        let findings = the_build_epoch_is_one_constant(&root).unwrap();
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].line, 8);

        // The exception is `unset` and nothing adjacent to it: a second value is still a second
        // declaration, in any job.
        write(&format!(
            "{HEADER}  desktop:\n    steps:\n      - run: export SOURCE_DATE_EPOCH=1\n{BUILD}"
        ));
        assert_eq!(the_build_epoch_is_one_constant(&root).unwrap().len(), 1);
    }
}
