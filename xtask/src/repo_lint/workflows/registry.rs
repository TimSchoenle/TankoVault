//! Owns the rules over registry calls in the publish workflow: each one retried, each image
//! reference fully named.

use super::PUBLISH_WORKFLOW;
use super::yaml::{expression, job_lines, shell_variable, step_output, workflow_env_value};
use crate::repo_lint::Finding;
use crate::repo_lint::text::is_comment;
use std::path::{Path, PathBuf};

/// **Every registry command in the publishing jobs of `release-please.yaml` runs through the retry
/// helper.** GHCR reports a throttled token request as `403 Forbidden` / `DENIED` with no
/// `Retry-After` (indistinguishable from a permissions failure), so buildx, syft and cosign all
/// give up on the first response.
///
/// A release makes eighteen `build` legs and nine `manifest` legs hit that registry inside a few
/// minutes, and `ci.yml` reads the same registry's build cache on the same push. A leg that loses
/// that race dies *after* its image was pushed, on the SBOM's pull or between the two
/// `imagetools create` calls, leaving Docker Hub carrying the release and GHCR not; in `build`
/// the by-digest push dies on `failed to fetch oauth token: denied: denied` while its seventeen
/// siblings pass. That is what makes this recur silently: it
/// looks like a flake, it is load, and the next call added outside the wrapper brings it back on
/// a release nobody is watching.
///
/// `oras` is on the list for the same reason as the rest of it: attaching the configuration
/// contract is a registry write on the publishing path, and it is the last call of the leg;
/// everything it can lose has already been pushed by the time it runs.
///
/// Scoped to those two jobs on purpose. `plan`'s `imagetools inspect` probes are *deliberately*
/// unretried: a failed probe there means "not published yet", which is a legitimate answer that a
/// retry would only make expensive.
pub(in crate::repo_lint) fn registry_calls_in_publish_retry(
    root: &Path,
) -> anyhow::Result<Vec<Finding>> {
    const RULE: &str = "registry-calls-in-publish-retry";
    /// The jobs whose every registry call is on the publishing path.
    const JOBS: [&str; 2] = ["build", "manifest"];
    /// The helper each of them is invoked through, by the name it is written and called under.
    const HELPER: &str = "registry-retry";
    /// The composite action that installs it, by the path a job references it at.
    const HELPER_ACTION: &str = "$/.github/actions/registry-retry";
    /// Commands that reach a registry, in the spellings this workflow uses.
    const REGISTRY_COMMANDS: [&str; 7] = [
        "docker buildx build",
        "docker buildx imagetools",
        "docker pull",
        "cosign sign",
        "cosign attest",
        // Two words, so the install step's download URL and its `mv` are not calls.
        "oras attach",
        "oras discover",
    ];

    let path = root.join(PUBLISH_WORKFLOW);
    let Ok(text) = std::fs::read_to_string(&path) else {
        anyhow::bail!("repo-lint: cannot read {}", path.display());
    };

    let mut findings = Vec::new();
    for job in JOBS {
        let mut calls = 0_usize;
        let mut installed = false;
        for (index, line) in job_lines(&text, job) {
            if !line.contains(HELPER) {
                let Some(command) = REGISTRY_COMMANDS
                    .iter()
                    .find(|command| line.contains(**command))
                else {
                    continue;
                };
                findings.push(Finding {
                    rule: RULE,
                    file: PathBuf::from(PUBLISH_WORKFLOW),
                    line: index,
                    detail: format!(
                        "`{command}` runs without `{HELPER}`; GHCR reports throttling as a bare \
                         403, so one unwrapped call fails a release after its images are already \
                         pushed"
                    ),
                });
                continue;
            }
            installed |= line.contains(HELPER_ACTION);
            calls += usize::from(
                REGISTRY_COMMANDS
                    .iter()
                    .any(|command| line.contains(*command)),
            );
        }

        // Neither half can be allowed to become a no-op: a renamed helper would pass the loop
        // above while wrapping nothing, and a publishing job with no registry call is not the job
        // this rule was written about.
        if !installed || calls == 0 {
            findings.push(Finding {
                rule: RULE,
                file: PathBuf::from(PUBLISH_WORKFLOW),
                line: 0,
                detail: format!(
                    "the `{job}` job installs `{HELPER}` on {installed} and makes {calls} call(s) \
                     through it, so this rule checks nothing for it; if either was renamed, \
                     rename it in xtask/src/repo_lint/workflows/registry.rs too"
                ),
            });
        }
    }

    // The jobs above only *reference* the installer, so a wrapper that stopped being written would
    // leave every call above failing with "no such file": a message about the helper's path, not
    // about the registry.
    let action = root
        .join(HELPER_ACTION.trim_start_matches("$/"))
        .join("action.yml");
    let installs = std::fs::read_to_string(&action)
        .is_ok_and(|action| action.contains(HELPER) && action.contains("chmod +x"));
    if !installs {
        findings.push(Finding {
            rule: RULE,
            file: PathBuf::from(format!("{HELPER_ACTION}/action.yml")),
            line: 0,
            detail: format!(
                "does not write an executable `{HELPER}`, so every wrapped call in \
                 {PUBLISH_WORKFLOW} resolves to a path that does not exist"
            ),
        });
    }
    Ok(findings)
}

/// **Every image reference the publishing jobs build names its registry.** `docker` and `cosign`
/// resolve a two-segment reference like `timschoenle/tankovault-api` against Docker Hub, because
/// that default is part of Docker's reference grammar. `oras` implements the OCI grammar, which
/// has no default: it reads `timschoenle` as the registry host and resolves *that*.
///
/// So the short form works for every tool this workflow runs except the one that attaches the
/// configuration contract. A release using it pushes and signs all nine images and then loses all
/// nine `publish` legs at `oras attach`, five retries each, to
/// `dial tcp: lookup timschoenle: server misbehaving`: a DNS error naming neither the reference
/// nor the fact that the `cosign sign` two steps above it read the same string differently.
/// Nothing is attached, no SBOM is attested, and `helm-release` never runs.
///
/// The reference is followed through the indirection the workflow writes it with: the use site
/// holds `${{ steps.names.outputs.dockerhub }}`, and the host is in the shell line that builds
/// that output.
pub(in crate::repo_lint) fn image_references_name_their_registry(
    root: &Path,
) -> anyhow::Result<Vec<Finding>> {
    const RULE: &str = "image-references-name-their-registry";

    let path = root.join(PUBLISH_WORKFLOW);
    let Ok(text) = std::fs::read_to_string(&path) else {
        anyhow::bail!("repo-lint: cannot read {}", path.display());
    };

    let mut findings = Vec::new();
    let references = image_env_references(&text);
    for (line, key, reference) in &references {
        let resolved = resolve_reference(&text, reference);
        if names_a_registry(&resolved) {
            continue;
        }
        findings.push(Finding {
            rule: RULE,
            file: PathBuf::from(PUBLISH_WORKFLOW),
            line: *line,
            detail: format!(
                "`{key}` resolves to `{resolved}`, whose first segment is a namespace rather than \
                 a registry host; `oras` reads it as the host and cannot resolve it, while every \
                 `docker` and `cosign` call on the same value succeeds"
            ),
        });
    }

    // A renamed key would leave the loop above reading nothing while looking straight at the bug.
    if references.is_empty() {
        findings.push(Finding {
            rule: RULE,
            file: PathBuf::from(PUBLISH_WORKFLOW),
            line: 0,
            detail: "no `*_IMAGE` step environment value found, so this rule checks nothing; if \
                     the keys were renamed, rename them in xtask/src/repo_lint/workflows/registry.rs too"
                .to_owned(),
        });
    }
    Ok(findings)
}

/// Every step `env:` entry naming an image, as its 1-based line number, key and value as written.
///
/// Keyed on the `_IMAGE` suffix instead of on the commands that read it: the value is set in a
/// step's `env:` and consumed several lines below, so the call is not what a reference can be
/// matched against.
fn image_env_references(text: &str) -> Vec<(usize, String, String)> {
    let mut references = Vec::new();
    for (index, line) in text.lines().enumerate() {
        if is_comment(line) {
            continue;
        }
        let Some((key, value)) = line.trim().split_once(": ") else {
            continue;
        };
        if key.ends_with("_IMAGE")
            && key
                .chars()
                .all(|character| character.is_ascii_uppercase() || character == '_')
        {
            references.push((index + 1, key.to_owned(), value.trim().to_owned()));
        }
    }
    references
}

/// A reference resolved as far as this file describes it: a `${{ steps.<id>.outputs.<name> }}`
/// becomes the value the shell line `echo "<name>=…"` writes, and the `${VAR}` that value carries
/// becomes the workflow-level `env:` entry. Anything neither names is left as written, which
/// [`names_a_registry`] then refuses instead of reading past.
fn resolve_reference(text: &str, reference: &str) -> String {
    let mut resolved = reference.to_owned();
    if let Some((span, body)) = expression(&resolved) {
        let value = body
            .strip_prefix("steps.")
            .and_then(|rest| rest.split_once(".outputs."))
            .and_then(|(step, name)| step_output(text, step, name))
            .or_else(|| {
                body.strip_prefix("env.")
                    .and_then(|name| workflow_env_value(text, name))
                    .map(|(_, value)| value)
            });
        if let Some(value) = value {
            resolved.replace_range(span, &value);
        }
    }
    // Bounded, not looped to exhaustion: one substitution can introduce the next (the
    // GHCR name is `${GHCR_REGISTRY}/${GITHUB_REPOSITORY,,}/${BIN}`), and only the host matters,
    // so a reference that is still expanding after this many passes has already been read.
    for _ in 0..4 {
        let Some((span, name)) = shell_variable(&resolved) else {
            break;
        };
        let Some((_, value)) = workflow_env_value(text, &name) else {
            break;
        };
        resolved.replace_range(span, &value);
    }
    resolved
}

/// Whether the reference's first path segment is a registry host instead of a namespace: a host
/// carries a dot or a port, and `localhost` is the one that carries neither.
///
/// An unexpanded `$` in that segment is not a host either. The rule cannot see what it expands
/// to, and a registry decided somewhere this scan does not read is the shape the bug arrived in.
fn names_a_registry(reference: &str) -> bool {
    let host = reference.split('/').next().unwrap_or_default();
    !host.contains('$') && (host.contains('.') || host.contains(':') || host == "localhost")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The bug this pins: a release published seven of nine images. GHCR reports a throttled
    /// token request as `403 Forbidden` / `DENIED` with no `Retry-After`, which is the same
    /// response as a permissions failure, so nothing retried it: `publish api` died on the SBOM's
    /// pull and `publish bootstrap` between the two `imagetools create` calls, both *after* their
    /// images had been pushed, one second apart, while the seven legs that had finished their
    /// registry work moments earlier all passed.
    ///
    /// The same failure one job earlier: a by-digest push in `build` written as a
    /// `docker/build-push-action` step cannot be wrapped, and `bootstrap (arm64)` died on
    /// `failed to fetch oauth token: denied: denied` while seventeen sibling legs pushed to the
    /// same registry and passed. So the rule reads both publishing jobs and counts
    /// `docker buildx build` as a registry call.
    ///
    /// The rule has to stay inside those jobs: `plan` probes the registry with the same
    /// `imagetools` command and is deliberately unretried, because a failed probe there is the
    /// legitimate answer "not published yet".
    #[test]
    fn a_registry_call_added_without_the_retry_helper_is_caught() {
        let workflow = "\
jobs:\n  \
plan:\n    \
steps:\n      \
- run: docker buildx imagetools inspect \"$IMAGE:$TAG\"\n  \
manifest:\n    \
steps:\n      \
- uses: $/.github/actions/registry-retry\n      \
# a comment naming docker pull is prose, not a call\n      \
- run: \"${RUNNER_TEMP}/registry-retry\" docker buildx imagetools create --tag \"$TAG\"\n      \
- run: docker pull \"$IMAGE:$TAG\"\n  \
summary:\n    \
steps:\n      \
- run: cosign sign --yes \"$IMAGE@$DIGEST\"\n";

        let lines = job_lines(workflow, "manifest");
        // `plan`'s probe above and `summary`'s call below are outside the job, and the comment is
        // not a call, so the one unwrapped `docker pull` is all that is left to report.
        let unwrapped: Vec<usize> = lines
            .iter()
            .filter(|(_, line)| line.contains("docker pull"))
            .map(|(index, _)| *index)
            .collect();
        assert_eq!(unwrapped, vec![10]);
        // The installer is a shared composite action, and the line referencing it is what the
        // rule reads as "installed": a job that calls the helper without installing it is the
        // no-op the guard reports.
        assert!(
            lines
                .iter()
                .any(|(_, line)| line.contains("$/.github/actions/registry-retry"))
        );
        assert!(!lines.iter().any(|(_, line)| line.contains("cosign sign")));
        assert!(!lines.iter().any(|(_, line)| line.contains("inspect")));
    }

    /// The by-digest shape specifically: an unwrapped `docker buildx build` in the `build` job.
    /// It pushes to both registries by digest, so it is a registry call like any other, though a
    /// command list naming only `imagetools`, `pull`, `sign` and `attest` misses it.
    #[test]
    fn an_unwrapped_by_digest_push_is_a_registry_call() {
        let workflow = "\
jobs:\n  \
build:\n    \
steps:\n      \
- run: docker buildx build --output type=image,push=true .\n  \
manifest:\n    \
steps:\n      \
- run: \"${RUNNER_TEMP}/registry-retry\" docker buildx build .\n";

        let build = job_lines(workflow, "build");
        assert!(
            build
                .iter()
                .any(|(_, line)| line.contains("docker buildx build")
                    && !line.contains("registry-retry"))
        );
        // The same command wrapped in the next job is not this job's, and is not a finding there.
        let manifest = job_lines(workflow, "manifest");
        assert!(
            manifest
                .iter()
                .filter(|(_, line)| line.contains("docker buildx build"))
                .all(|(_, line)| line.contains("registry-retry"))
        );
    }

    /// The bug this pins: a release built its Docker Hub references as
    /// `timschoenle/tankovault-<bin>`, with no registry in front of the namespace. Docker's
    /// reference grammar defaults that to Docker Hub, so every `docker` and `cosign` call made
    /// with the same string succeeded: the images were pushed, the manifest lists assembled and
    /// every digest signed. `oras` implements the OCI grammar, which has no default: it read
    /// `timschoenle` as the registry host, and all nine `publish` legs died on
    /// `dial tcp: lookup timschoenle: server misbehaving` while attaching the configuration
    /// contract.
    ///
    /// So the resolution is what has to be tested, not the spelling at the use site: the value
    /// there is an expression, and both the registry and the namespace are somewhere else.
    #[test]
    fn a_docker_hub_reference_without_its_registry_is_caught() {
        let workflow = "\
env:\n  \
GHCR_REGISTRY: ghcr.io\n  \
DOCKERHUB_NAMESPACE: timschoenle\n\
\n\
jobs:\n  \
manifest:\n    \
steps:\n      \
- id: digests\n        \
run: |\n          \
echo \"dockerhub=sha256:1bab5b1d\"\n      \
- name: resolve image names\n        \
id: names\n        \
run: |\n          \
echo \"dockerhub=${DOCKERHUB_NAMESPACE}/tankovault-${BIN}\"\n          \
echo \"ghcr=${GHCR_REGISTRY}/${GITHUB_REPOSITORY,,}/${BIN}\"\n      \
- env:\n          \
DOCKERHUB_IMAGE: ${{ steps.names.outputs.dockerhub }}\n          \
GHCR_IMAGE: ${{ steps.names.outputs.ghcr }}\n";

        let resolved: Vec<(String, String)> = image_env_references(workflow)
            .iter()
            .map(|(_, key, value)| (key.clone(), resolve_reference(workflow, value)))
            .collect();
        assert_eq!(
            resolved,
            vec![
                (
                    "DOCKERHUB_IMAGE".to_owned(),
                    "timschoenle/tankovault-${BIN}".to_owned()
                ),
                (
                    "GHCR_IMAGE".to_owned(),
                    "ghcr.io/${GITHUB_REPOSITORY,,}/${BIN}".to_owned()
                ),
            ]
        );
        // The bug and its sibling that never had it: only the tag is left unexpanded on the GHCR
        // name, and the host is in front of it. Neither took the digest the step above writes
        // under the same output name, which would have passed, since a digest carries a colon.
        assert!(!names_a_registry(&resolved[0].1));
        assert!(names_a_registry(&resolved[1].1));

        let fixed = workflow.replace("dockerhub=", "dockerhub=docker.io/");
        let references = image_env_references(&fixed);
        assert!(names_a_registry(&resolve_reference(
            &fixed,
            &references[0].2
        )));

        // A port is a registry host and so is `localhost`; a variable this scan cannot expand is
        // neither, because what it holds is exactly what the rule cannot see.
        assert!(names_a_registry("localhost:5000/tankovault-api"));
        assert!(!names_a_registry("${REGISTRY}/tankovault-api"));
    }
}
