//! Rules tying the workspace manifest to what actually ships: every binary built, nothing
//! published that the deploy blacklist excludes, every build input classified.

use std::path::Path;

use super::Finding;
use super::text::{is_comment, walk};

/// **Every workspace binary must be listed in the Dockerfile's `SERVICE_BINS`.** The Dockerfile
/// compiles all binaries in one `cargo` invocation from a literal list, so a `[[bin]]` added to
/// the workspace without updating it produces an image that fails at the final `COPY` — only
/// once someone tries to build the service nobody knew was missing. This reads `SERVICE_BINS`
/// and every manifest's `[[bin]] name = …` and reports each direction of disagreement.
/// (`web/frontend` doesn't count: it's outside the host workspace and its `app` binary is a
/// `wasm32` artefact `dx` builds, not one any runtime stage copies.) Every workspace binary is
/// *built*; which of them may be *published* is [`deploy_blacklist_is_honoured`].
pub(super) fn dockerfile_ships_every_workspace_binary(root: &Path) -> anyhow::Result<Vec<Finding>> {
    let dockerfile = root.join("deploy/docker/Dockerfile");
    let Ok(text) = std::fs::read_to_string(&dockerfile) else {
        anyhow::bail!("repo-lint: cannot read {}", dockerfile.display());
    };

    let Some((line_number, declared)) = service_bins(&text) else {
        anyhow::bail!(
            "repo-lint: {} declares no `ARG SERVICE_BINS=\"…\"`; the builder stage cannot \
             have been left without one",
            dockerfile.display()
        );
    };

    let mut findings = Vec::new();
    let mut actual = Vec::new();
    // `exclude`d members are not workspace binaries; `target` holds build output, and a vendored
    // manifest under it would otherwise be read as a member.
    for manifest in walk(
        root,
        &["toml"],
        &["target", "web", "fuzz", "mutants.out", "mutants.out.old"],
    ) {
        if manifest.file_name().is_none_or(|name| name != "Cargo.toml") {
            continue;
        }
        let Ok(contents) = std::fs::read_to_string(&manifest) else {
            continue;
        };
        actual.extend(bin_targets(&contents));
    }

    for bin in &actual {
        if !declared.contains(bin) {
            findings.push(Finding {
                rule: "dockerfile-ships-every-workspace-binary",
                file: dockerfile.clone(),
                line: line_number,
                detail: format!(
                    "workspace binary `{bin}` is missing from SERVICE_BINS, so no image can \
                     ship it — add it here (and give it a compose service if it needs one)"
                ),
            });
        }
    }
    for bin in &declared {
        if !actual.contains(bin) {
            findings.push(Finding {
                rule: "dockerfile-ships-every-workspace-binary",
                file: dockerfile.clone(),
                line: line_number,
                detail: format!(
                    "SERVICE_BINS names `{bin}`, which is not a `[[bin]]` target in any \
                     workspace manifest — the builder stage will fail on it"
                ),
            });
        }
    }
    Ok(findings)
}

/// **A binary the deploy blacklist excludes must never reach a registry, and every other one
/// must.** Being built is not the same as being shipped: `xtask` is compiled into an image the
/// compose stack runs (`migrate`, `seed`), but it is the repository's task runner, and a
/// registry is not where a command that resets a database belongs. Nothing in a workflow
/// matrix records that distinction, so this reads `[workspace.metadata.deploy.exclude]` from the
/// root manifest and checks every image matrix under `.github/workflows/` against it: an excluded
/// binary in a matrix publishes what must not be published, and a name the Dockerfile does not
/// build fails at the final `COPY`.
///
/// The other direction — a deployable binary *missing* from the publish set — is no longer a
/// matrix check, because `release-please.yaml` no longer carries one. It derives its set from
/// `xtask release-plan`, which computes `SERVICE_BINS` minus this blacklist and so cannot omit a
/// service by drifting. What that moves the risk to is the derivation being quietly replaced by a
/// literal again, which would publish a hand-maintained list while every gate still passed; so
/// the publish workflow is required to contain no literal image matrix and to invoke the planner.
pub(super) fn deploy_blacklist_is_honoured(root: &Path) -> anyhow::Result<Vec<Finding>> {
    const RULE: &str = "deploy-blacklist-is-honoured";

    let manifest_path = root.join("Cargo.toml");
    let Ok(manifest) = std::fs::read_to_string(&manifest_path) else {
        anyhow::bail!("repo-lint: cannot read {}", manifest_path.display());
    };
    let excluded = deploy_exclusions(&manifest);

    let dockerfile = root.join("deploy/docker/Dockerfile");
    let Ok(text) = std::fs::read_to_string(&dockerfile) else {
        anyhow::bail!("repo-lint: cannot read {}", dockerfile.display());
    };
    let Some((_, built)) = service_bins(&text) else {
        anyhow::bail!(
            "repo-lint: {} declares no `ARG SERVICE_BINS=\"…\"`, so there is nothing to \
             judge the deploy blacklist against",
            dockerfile.display()
        );
    };

    let mut findings = Vec::new();
    for entry in &excluded {
        if !built.contains(&entry.bin) {
            findings.push(Finding {
                rule: RULE,
                file: manifest_path.clone(),
                line: entry.line,
                detail: format!(
                    "`{}` is excluded from deployment but is not in the Dockerfile's \
                     SERVICE_BINS — nothing builds it, so the entry is stale",
                    entry.bin
                ),
            });
        }
    }

    let workflows = root.join(".github/workflows");
    let publish = workflows.join("release-please.yaml");
    let Ok(entries) = std::fs::read_dir(&workflows) else {
        anyhow::bail!("repo-lint: cannot read {}", workflows.display());
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension == "yml" || extension == "yaml")
        {
            continue;
        }
        let Ok(workflow) = std::fs::read_to_string(&path) else {
            continue;
        };
        for (line, names) in image_matrices(&workflow) {
            for detail in matrix_violations(&built, &excluded, &names) {
                findings.push(Finding {
                    rule: RULE,
                    file: path.clone(),
                    line,
                    detail,
                });
            }
        }
    }

    let Ok(workflow) = std::fs::read_to_string(&publish) else {
        anyhow::bail!("repo-lint: cannot read {}", publish.display());
    };
    for (line, _) in image_matrices(&workflow) {
        findings.push(Finding {
            rule: RULE,
            file: publish.clone(),
            line,
            detail: "the publish workflow names its images literally; it must take them from \
                     `xtask release-plan`, which is what holds the published set to SERVICE_BINS \
                     minus the deploy blacklist"
                .to_owned(),
        });
    }
    if !workflow.contains("release-plan") {
        findings.push(Finding {
            rule: RULE,
            file: publish,
            line: 1,
            detail: "the publish workflow never invokes `xtask release-plan`, so nothing decides \
                     which images a release publishes"
                .to_owned(),
        });
    }
    Ok(findings)
}

/// **Every top-level path must be something `release-plan` can classify.** The planner decides
/// which images a release rebuilds from the workspace dependency graph plus a table of the inputs
/// that belong to no package (`RULES`, `xtask/src/release_plan.rs`). A new top-level entry that
/// matches neither is the one failure that is silent in the dangerous direction: the planner
/// treats it as a change to everything and says so on stderr, but nobody reads a release log that
/// published successfully. This makes the omission fail on the pull request that introduces it,
/// while the person who knows whether the new directory reaches an image is still looking.
pub(super) fn build_inputs_are_classified(root: &Path) -> anyhow::Result<Vec<Finding>> {
    const RULE: &str = "build-inputs-are-classified";

    let manifest_path = root.join("Cargo.toml");
    let Ok(manifest) = std::fs::read_to_string(&manifest_path) else {
        anyhow::bail!("repo-lint: cannot read {}", manifest_path.display());
    };
    let roots = crate::release_plan::member_roots(&manifest);
    if roots.is_empty() {
        anyhow::bail!(
            "repo-lint: {} declares no `members = [...]`, so there is nothing to judge package \
             ownership against",
            manifest_path.display()
        );
    }

    let Ok(entries) = std::fs::read_dir(root) else {
        anyhow::bail!("repo-lint: cannot read {}", root.display());
    };
    let mut findings = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        // Not part of any build context and not a repository artefact.
        if name == ".git" {
            continue;
        }
        if crate::release_plan::top_level_is_classified(&name, &roots) {
            continue;
        }
        findings.push(Finding {
            rule: RULE,
            file: entry.path(),
            line: 1,
            detail: format!(
                "`{name}` belongs to no workspace member and matches no rule in \
                 xtask/src/release_plan.rs. Add it to RULES — `Every` if a published image is \
                 built from it, `Inert` if it cannot reach one"
            ),
        });
    }
    Ok(findings)
}

/// The `SERVICE_BINS` value from a Dockerfile, with its 1-based line number.
///
/// Split out so the parse can be tested against the forms that must and must not be recognised,
/// without a filesystem. Shared with `release-plan`, which derives the publishable set from the
/// same declaration rather than from a list of its own.
pub(crate) fn service_bins(dockerfile: &str) -> Option<(usize, Vec<String>)> {
    for (index, line) in dockerfile.lines().enumerate() {
        let Some(value) = line.trim().strip_prefix("ARG SERVICE_BINS=") else {
            continue;
        };
        let names = value
            .trim()
            .trim_matches('"')
            .split_whitespace()
            .map(str::to_owned)
            .collect();
        return Some((index + 1, names));
    }
    None
}

/// The `name` of every `[[bin]]` target declared in one `Cargo.toml`. Tracking whether the
/// current section is `[[bin]]` is enough to keep `[package] name = …` out of the result.
fn bin_targets(manifest: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut in_bin_section = false;
    for line in manifest.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_bin_section = line.starts_with("[[bin]]");
            continue;
        }
        if !in_bin_section {
            continue;
        }
        if let Some(value) = line.strip_prefix("name") {
            let Some(value) = value.trim_start().strip_prefix('=') else {
                continue;
            };
            names.push(value.trim().trim_matches('"').to_owned());
        }
    }
    names
}

/// What one image matrix gets wrong, as the detail text of each violation.
///
/// Pure, so both directions can be tested without a workflow tree: a blacklisted name present,
/// and a name nothing builds. Completeness is deliberately not checked here — every remaining
/// literal matrix is one that narrows on purpose (`ci.yml` on pull requests), and the publish
/// workflow has no literal at all.
fn matrix_violations(built: &[String], excluded: &[Exclusion], names: &[String]) -> Vec<String> {
    let mut details = Vec::new();
    for name in names {
        if let Some(entry) = excluded.iter().find(|entry| &entry.bin == name) {
            details.push(format!(
                "image matrix names `{name}`, which the deploy blacklist excludes: {}",
                entry.reason
            ));
        } else if !built.contains(name) {
            details.push(format!(
                "image matrix names `{name}`, which the Dockerfile's SERVICE_BINS does not \
                 build — the leg would fail at the final COPY"
            ));
        }
    }
    details
}

/// One `[workspace.metadata.deploy.exclude]` entry: a binary that is built but never published.
pub(crate) struct Exclusion {
    line: usize,
    pub(crate) bin: String,
    reason: String,
}

/// The deploy blacklist declared in the root manifest.
///
/// A line-based read of one known table, like the rest of this module: the entries are
/// `<bin> = "<reason>"`, and the table ends at the next header.
pub(crate) fn deploy_exclusions(manifest: &str) -> Vec<Exclusion> {
    let mut out = Vec::new();
    let mut in_table = false;
    for (index, line) in manifest.lines().enumerate() {
        let line = line.trim();
        if line.starts_with('[') {
            in_table = line.starts_with("[workspace.metadata.deploy.exclude]");
            continue;
        }
        if !in_table || is_comment(line) {
            continue;
        }
        let Some((bin, reason)) = line.split_once('=') else {
            continue;
        };
        out.push(Exclusion {
            line: index + 1,
            bin: bin.trim().trim_matches('"').to_owned(),
            reason: reason.trim().trim_matches('"').to_owned(),
        });
    }
    out
}

/// Every image matrix in one workflow, as its 1-based line number and the names it lists.
///
/// Two literal forms carry one: a `strategy.matrix` leg (`bin: [api, worker]`) and the JSON
/// array `ci.yml` echoes into `$GITHUB_OUTPUT` (`images=["api","render"]`). Comment lines are
/// skipped so the prose describing a matrix is not read as one.
fn image_matrices(workflow: &str) -> Vec<(usize, Vec<String>)> {
    let mut out = Vec::new();
    for (index, line) in workflow.lines().enumerate() {
        if is_comment(line) {
            continue;
        }
        let line = line.trim();
        let Some(rest) = line.strip_prefix("bin: [").or_else(|| {
            line.split_once("images=[")
                .map(|(_, after_marker)| after_marker)
        }) else {
            continue;
        };
        let Some((body, _)) = rest.split_once(']') else {
            continue;
        };
        let names = body
            .split(',')
            .map(|name| name.trim().trim_matches(['"', '\'']).to_owned())
            .filter(|name| !name.is_empty())
            .collect();
        out.push((index + 1, names));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn service_bins_is_read_off_the_arg_line() {
        let dockerfile = "FROM scratch AS builder\n\
             # ARG SERVICE_BINS=\"decoy\"\n\
             ARG SERVICE_BINS=\"api worker xtask\"\n\
             RUN true\n";
        let (line, names) = service_bins(dockerfile).expect("the ARG is present");
        // Line 3, not the commented decoy on line 2: the finding has to point at the
        // declaration an author would edit.
        assert_eq!(line, 3);
        assert_eq!(names, ["api", "worker", "xtask"]);

        assert!(service_bins("FROM scratch\nARG BIN\n").is_none());
    }

    #[test]
    fn bin_targets_are_told_apart_from_the_package_name() {
        // The real shape of a service manifest. `[package] name` and `[[bin]] name` differ on
        // purpose here (`tankovault-api` vs `api`) and confusing them is the whole hazard.
        let manifest = "[package]\n\
             name = \"tankovault-api\"\n\
             version.workspace = true\n\
             \n\
             [[bin]]\n\
             name = \"api\"\n\
             path = \"src/main.rs\"\n\
             \n\
             [dependencies]\n\
             name-resolver = \"1\"\n";
        assert_eq!(bin_targets(manifest), ["api"]);

        // A library-only crate declares no binary at all.
        assert_eq!(
            bin_targets("[package]\nname = \"tankovault-domain\"\n"),
            Vec::<String>::new()
        );
    }

    #[test]
    fn the_deploy_blacklist_is_read_off_its_own_table() {
        let manifest = "[workspace]\n\
             members = [\"xtask\"]\n\
             \n\
             [workspace.metadata.deploy.exclude]\n\
             # xtask = \"decoy\"\n\
             xtask = \"a task runner, not a service\"\n\
             \n\
             [workspace.package]\n\
             version = \"0.1.0\"\n";
        let excluded = deploy_exclusions(manifest);
        assert_eq!(excluded.len(), 1);
        assert_eq!(excluded[0].bin, "xtask");
        assert_eq!(excluded[0].reason, "a task runner, not a service");
        // Line 6, not the commented decoy on line 5.
        assert_eq!(excluded[0].line, 6);

        // `[workspace.package] version` is a `key = "value"` line in a different table; reading
        // it as an exclusion would blacklist a version number.
        assert!(deploy_exclusions("[workspace.package]\nversion = \"0.1.0\"\n").is_empty());
    }

    /// Both literal forms an image matrix takes, and the prose about them that must not read as
    /// one — the whole rule is a text scan over files that also *describe* their matrices.
    #[test]
    fn image_matrices_are_read_in_both_forms_but_not_out_of_comments() {
        let workflow = "    strategy:\n\
             \x20     matrix:\n\
             \x20       bin: [api, worker, render]\n\
             \x20 # bin: [xtask]\n\
             \x20         run: echo 'images=[\"api\",\"render\"]' >> \"$GITHUB_OUTPUT\"\n\
             \x20         bin: ${{ fromJSON(needs.plan.outputs.images) }}\n";
        let matrices = image_matrices(workflow);
        assert_eq!(matrices.len(), 2);
        assert_eq!(
            matrices[0],
            (3, vec!["api".into(), "worker".into(), "render".into()])
        );
        assert_eq!(matrices[1], (5, vec!["api".into(), "render".into()]));
    }

    /// The rule exists because `xtask` — a task runner with a `reset` command — was in both
    /// release matrices and would have been pushed to GHCR and Docker Hub under a version tag.
    #[test]
    fn a_matrix_may_narrow_but_may_not_name_what_must_not_ship() {
        let built = ["api".to_owned(), "render".to_owned(), "xtask".to_owned()];
        let excluded = vec![Exclusion {
            line: 9,
            bin: "xtask".to_owned(),
            reason: "a task runner, not a service".to_owned(),
        }];
        let check = |names: &[&str]| {
            matrix_violations(
                &built,
                &excluded,
                &names.iter().map(|n| (*n).to_owned()).collect::<Vec<_>>(),
            )
        };

        assert!(check(&["api", "render"]).is_empty());

        let blacklisted = check(&["api", "render", "xtask"]);
        assert_eq!(blacklisted.len(), 1);
        assert!(blacklisted[0].contains("a task runner, not a service"));

        // A subset is `ci.yml` narrowing its build set on a pull request, which is deliberate.
        // Completeness of the *published* set is `release-plan`'s, not a matrix's.
        assert!(
            check(&["api"]).is_empty(),
            "a literal matrix may be any subset"
        );

        // A name no runtime stage could copy.
        assert_eq!(check(&["api", "render", "typo"]).len(), 1);
    }
}
