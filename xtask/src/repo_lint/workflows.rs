//! Rules over `.github/workflows`: the Postgres major the tests run, advisory ignores,
//! concurrency groups, and the OIDC token the release signs with.

use std::path::{Path, PathBuf};

use super::Finding;
use super::text::{const_str, is_comment, toml_string_array};

/// **The integration harness must run production's Postgres image *and* major.** The query
/// planner is a major-version artefact, so `crates/db/tests/repo_query_plans.rs` — which asserts
/// that the trigram searches reach their GIN indexes instead of scanning the whole catalogue —
/// proves nothing about production the moment the two majors diverge, and proves it silently,
/// because no output of a green run names a version. Nothing else connects them: the harness tag
/// is a Rust `const`, the deployed image is a digest-pinned line in the compose file, and a bump
/// to either one alone is a perfectly ordinary-looking change.
///
/// The **image name** is checked for a second reason, added when migration 0027 made pgvector a
/// hard dependency. A harness on `pgvector/pgvector` with production on stock `postgres` is the
/// dangerous direction: every test passes, because the tests can run `CREATE EXTENSION vector`,
/// and the deployment then fails on its first migration. The reverse at least fails loudly and
/// immediately. Only the silent direction needs a rule, but comparing the names catches both.
///
/// There are **three** copies of this decision, not two, and the third is the one nothing pointed
/// at: CI's own service containers. The `sqlx offline cache` job pins a Postgres of its own and
/// applies every migration to it. On the wrong image that job dies at `CREATE EXTENSION vector`
/// having verified nothing, and it reports a missing extension rather than a wrong pin — so the
/// obvious reading is "the migration is broken", not "this job runs a different database than
/// production does". Every `image:` line under `.github/workflows/` naming a Postgres is held to
/// the compose file too.
pub(super) fn tests_run_the_production_postgres_major(root: &Path) -> anyhow::Result<Vec<Finding>> {
    const RULE: &str = "tests-run-the-production-postgres-major";
    const HARNESS: &str = "crates/test-support/src/lib.rs";
    const COMPOSE: &str = "deploy/docker-compose.yml";

    /// The Postgres distributions the compose file may pin, as `image: <name>:<tag>`.
    ///
    /// Enumerated rather than pattern-matched: an arbitrary `image:` line must not be able to
    /// satisfy a rule about which Postgres runs. Stock `postgres` stays listed so the rule keeps
    /// working if the pgvector move is ever reverted.
    const POSTGRES_IMAGES: &[&str] = &["postgres", "pgvector/pgvector"];

    let harness_path = root.join(HARNESS);
    let Ok(harness) = std::fs::read_to_string(&harness_path) else {
        anyhow::bail!("repo-lint: cannot read {}", harness_path.display());
    };
    let Some((tag_line, tag)) = const_str(&harness, "POSTGRES_TAG") else {
        anyhow::bail!(
            "repo-lint: {} declares no `const POSTGRES_TAG: &str = \"…\"`; nothing else records \
             which Postgres major the integration suites run",
            harness_path.display()
        );
    };
    let harness_major = major_of(&tag).unwrap_or(tag.as_str()).to_owned();
    let Some((_, harness_image)) = const_str(&harness, "POSTGRES_IMAGE") else {
        anyhow::bail!(
            "repo-lint: {} declares no `const POSTGRES_IMAGE: &str = \"…\"`; nothing else \
             records which Postgres distribution the integration suites run, and stock \
             `postgres` cannot apply migration 0027",
            harness_path.display()
        );
    };

    let compose_path = root.join(COMPOSE);
    let Ok(compose) = std::fs::read_to_string(&compose_path) else {
        anyhow::bail!("repo-lint: cannot read {}", compose_path.display());
    };
    let deployed = compose.lines().enumerate().find_map(|(index, line)| {
        let trimmed = line.trim_start();
        if is_comment(trimmed) {
            return None;
        }
        let (name, tag) = image_reference(trimmed)?;
        POSTGRES_IMAGES
            .contains(&name)
            .then(|| Some((index + 1, name.to_owned(), major_of(tag)?.to_owned())))?
    });
    let Some((compose_line, compose_image, compose_major)) = deployed else {
        anyhow::bail!(
            "repo-lint: {} pins none of {POSTGRES_IMAGES:?} as `image: <name>:<tag>`; the rule \
             cannot tell which Postgres production runs",
            compose_path.display()
        );
    };

    let mut findings =
        workflow_postgres_findings(root, RULE, &compose_image, &compose_major, compose_line);

    if harness_major != compose_major {
        findings.push(Finding {
            rule: RULE,
            file: PathBuf::from(HARNESS),
            line: tag_line,
            detail: format!(
                "tests run Postgres {harness_major}, production runs {compose_major} \
                 ({COMPOSE}:{compose_line}); plan assertions are only evidence when they match"
            ),
        });
    }
    if harness_image != compose_image {
        findings.push(Finding {
            rule: RULE,
            file: PathBuf::from(HARNESS),
            line: tag_line,
            detail: format!(
                "tests run `{harness_image}`, production runs `{compose_image}` \
                 ({COMPOSE}:{compose_line}); migration 0027 needs `CREATE EXTENSION vector`, so \
                 a harness with the extension and a deployment without it is a green suite and \
                 a failed migration"
            ),
        });
    }
    Ok(findings)
}

/// **The two advisory-ignore lists must be the same list.** `cargo deny check advisories` and
/// `cargo audit` read the same `RustSec` database and neither reads the other's configuration:
/// cargo-deny takes its exceptions from `deny.toml`, cargo-audit from `.cargo/audit.toml`.
/// Drift is silent in the direction that matters. An entry present only in `deny.toml` leaves
/// `cargo-audit` failing on an advisory that has already been reviewed and accepted — a gate
/// nobody can distinguish from a real finding, so the next real finding is merged past. An
/// entry present only in `.cargo/audit.toml` is worse: the advisory is suppressed for the job
/// that reports it and never recorded where the dated justification is meant to live.
pub(super) fn advisory_ignores_agree(root: &Path) -> anyhow::Result<Vec<Finding>> {
    const RULE: &str = "advisory-ignores-agree";
    const DENY: &str = "deny.toml";
    const AUDIT: &str = ".cargo/audit.toml";

    let deny_path = root.join(DENY);
    let audit_path = root.join(AUDIT);
    let (Ok(deny_text), Ok(audit_text)) = (
        std::fs::read_to_string(&deny_path),
        std::fs::read_to_string(&audit_path),
    ) else {
        anyhow::bail!(
            "repo-lint: cannot read {} and {}",
            deny_path.display(),
            audit_path.display()
        );
    };

    // An absent list is not an empty one: cargo-deny and cargo-audit both default to ignoring
    // nothing, so a missing key would read as "no exceptions" and this rule would pass while
    // the gate it protects is unconfigured.
    let Some((deny_line, deny_ignores)) = toml_string_array(&deny_text, "[advisories]", "ignore")
    else {
        anyhow::bail!(
            "repo-lint: {} declares no `[advisories] ignore = [...]`; if the list is genuinely \
             empty, write it as `ignore = []` so this rule can tell that from a deleted key",
            deny_path.display()
        );
    };
    let Some((audit_line, audit_ignores)) =
        toml_string_array(&audit_text, "[advisories]", "ignore")
    else {
        anyhow::bail!(
            "repo-lint: {} declares no `[advisories] ignore = [...]`; see {}",
            audit_path.display(),
            deny_path.display()
        );
    };

    let mut findings = Vec::new();
    for id in &deny_ignores {
        if !audit_ignores.contains(id) {
            findings.push(Finding {
                rule: RULE,
                file: PathBuf::from(AUDIT),
                line: audit_line,
                detail: format!(
                    "`{DENY}` accepts `{id}` and this list does not — `cargo-audit` stays red \
                     on an advisory that has already been reviewed"
                ),
            });
        }
    }
    for id in &audit_ignores {
        if !deny_ignores.contains(id) {
            findings.push(Finding {
                rule: RULE,
                file: PathBuf::from(DENY),
                line: deny_line,
                detail: format!(
                    "`{AUDIT}` suppresses `{id}` and this list does not — the exception is \
                     applied without the dated justification this file exists to hold"
                ),
            });
        }
    }
    Ok(findings)
}

/// Every Postgres a workflow pins for itself, held to the compose file.
///
/// Split out of [`tests_run_the_production_postgres_major`] only for length; the reasoning lives
/// in that function's documentation.
fn workflow_postgres_findings(
    root: &Path,
    rule: &'static str,
    compose_image: &str,
    compose_major: &str,
    compose_line: usize,
) -> Vec<Finding> {
    const COMPOSE: &str = "deploy/docker-compose.yml";
    const POSTGRES_IMAGES: &[&str] = &["postgres", "pgvector/pgvector"];

    let mut findings = Vec::new();
    for entry in std::fs::read_dir(root.join(".github/workflows"))
        .into_iter()
        .flatten()
        .flatten()
    {
        let path = entry.path();
        if !path
            .extension()
            .is_some_and(|ext| ext == "yml" || ext == "yaml")
        {
            continue;
        }
        let Ok(workflow) = std::fs::read_to_string(&path) else {
            continue;
        };
        let workflow_name = path.file_name().map_or_else(
            || path.display().to_string(),
            |n| n.to_string_lossy().into(),
        );

        for (index, line) in workflow.lines().enumerate() {
            let trimmed = line.trim_start();
            if is_comment(trimmed) {
                continue;
            }
            let Some((name, tag)) = image_reference(trimmed) else {
                continue;
            };
            if !POSTGRES_IMAGES.contains(&name) {
                continue;
            }
            if name != compose_image || major_of(tag).is_none_or(|major| major != compose_major) {
                findings.push(Finding {
                    rule,
                    file: PathBuf::from(format!(".github/workflows/{workflow_name}")),
                    line: index + 1,
                    detail: format!(
                        "this job runs `{name}:{tag}`, production runs `{compose_image}`                          {compose_major} ({COMPOSE}:{compose_line}); a job that applies the                          migrations against a different Postgres verifies nothing about the one                          that runs them"
                    ),
                });
            }
        }
    }
    findings
}

/// Split a compose `image: <repository>:<tag>[@<digest>]` line into repository and tag.
///
/// Split out so the parse can be tested without a filesystem — and it needs testing, because
/// both separators appear twice in a real pinned line. The digest is dropped first: `sha256:…`
/// carries the same `:` as the tag, so taking the last colon of the whole reference lands inside
/// the hex and reads it as a version. Only then is the repository split off from the right,
/// which is what lets a registry host with a port (`ghcr.io:443/…`) keep working.
fn image_reference(line: &str) -> Option<(&str, &str)> {
    let rest = line.strip_prefix("image: ")?;
    let reference = rest.split_once('@').map_or(rest, |(before, _)| before);
    reference.rsplit_once(':')
}

/// The major version leading a Postgres image tag: `18` from `18-alpine`, from
/// `18.4-alpine@sha256:…`, and from a bare `18`.
///
/// Split out so the shapes a digest pin and a Renovate bump produce can be tested without a
/// filesystem — the `@sha256:` suffix is the one that looks like it would not parse.
fn major_of(tag: &str) -> Option<&str> {
    let major = tag.trim_matches('"').split(['-', '.', '@']).next()?.trim();
    // `pgvector/pgvector` tags its images `pg<major>`; the stock image uses the bare major. The
    // prefix is stripped rather than special-cased at the call site so both spellings compare as
    // the same number — which is the only thing this rule is actually about.
    let major = major.strip_prefix("pg").unwrap_or(major);
    (!major.is_empty() && major.bytes().all(|b| b.is_ascii_digit())).then_some(major)
}

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
pub(super) fn concurrency_groups_hold_one_workflow(root: &Path) -> anyhow::Result<Vec<Finding>> {
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

/// **The cosign OIDC token file is written with `printf '%s'`, never a newline-appending
/// redirect.** `release-please.yaml` mints one OIDC token per `publish` leg and hands cosign the
/// *path* to it, because a token in `argv` is readable from `/proc`. cosign reads that path with
/// `os.ReadFile` and puts the bytes straight into Fulcio's `Authorization` header — it trims
/// nothing. So a single trailing newline, which is what `jq -r`, `echo` and every other ordinary
/// redirect leave behind, makes Go's `net/http` reject the request before it is sent.
///
/// The failure names neither the file nor the newline: it is
/// `net/http: invalid header field value for "Authorization"`, raised at the Fulcio POST, and the
/// mint step ahead of it passes. Release 1.3.1 published all nine images unsigned that way, and
/// the two steps that consume the token — `sign` and `attest` — fail identically, so nothing in
/// the run points back at how the file was written.
///
/// Requiring at least one write keeps the rule from quietly becoming a no-op if the file is
/// renamed: a rename has to touch this constant too.
pub(super) fn the_oidc_token_carries_no_newline(root: &Path) -> anyhow::Result<Vec<Finding>> {
    const RULE: &str = "oidc-token-carries-no-newline";

    let path = root.join(PUBLISH_WORKFLOW);
    let Ok(text) = std::fs::read_to_string(&path) else {
        anyhow::bail!("repo-lint: cannot read {}", path.display());
    };

    let writes = oidc_token_writes(&text);
    let mut findings: Vec<Finding> = writes
        .iter()
        .filter(|(_, safe)| !safe)
        .map(|(line, _)| Finding {
            rule: RULE,
            file: PathBuf::from(PUBLISH_WORKFLOW),
            line: *line,
            detail: format!(
                "`{OIDC_TOKEN_FILE}` is written by a redirect that is not `{OIDC_TOKEN_WRITER}`; \
                 cosign sends the file's bytes as an `Authorization` header without trimming \
                 them, so a trailing newline fails every signature with `invalid header field \
                 value`"
            ),
        })
        .collect();

    if writes.is_empty() {
        findings.push(Finding {
            rule: RULE,
            file: PathBuf::from(PUBLISH_WORKFLOW),
            line: 0,
            detail: format!(
                "no line writes `{OIDC_TOKEN_FILE}`, so this rule checks nothing; if the token \
                 file was renamed, rename it in xtask/src/repo_lint/workflows.rs too"
            ),
        });
    }
    Ok(findings)
}

/// The workflow that mints the OIDC token and signs with it.
const PUBLISH_WORKFLOW: &str = ".github/workflows/release-please.yaml";
/// The file the minted token is written to, and the only safe way to write it.
const OIDC_TOKEN_FILE: &str = "cosign-oidc-token";
const OIDC_TOKEN_WRITER: &str = "printf '%s'";

/// Every line of `text` that redirects into the token file, paired with whether it writes the
/// token the one way that appends nothing.
///
/// A mention with no `>` is a *read* (`--identity-token "$token"`) and is not a write, so it is
/// not reported — which is why the rule cannot simply require `printf` on every matching line.
fn oidc_token_writes(text: &str) -> Vec<(usize, bool)> {
    text.lines()
        .enumerate()
        .filter(|(_, line)| {
            !is_comment(line) && line.contains(OIDC_TOKEN_FILE) && line.contains('>')
        })
        .map(|(index, line)| (index + 1, line.contains(OIDC_TOKEN_WRITER)))
        .collect()
}

/// **Every workflow that builds an image declares one constant `SOURCE_DATE_EPOCH`, at workflow
/// level, and nothing else names it.** buildx propagates the variable from its environment into
/// the solve as `build-arg:SOURCE_DATE_EPOCH`, so its value is part of the cache key of every
/// stage in `deploy/docker/Dockerfile`. There is no channel that reaches the exporter — where
/// `rewrite-timestamp=true` needs it — without also reaching the build.
///
/// So a *derived* epoch cannot be reproducibility for free; it is a guaranteed cache miss. The
/// repository has now paid for that twice, in opposite directions. Passed as
/// `--build-arg SOURCE_DATE_EPOCH=$(git log -1 --format=%ct)` it went to the warm-up and both
/// publishing builds, which agreed with each other and with nothing `ci.yml` wrote: the warm-up
/// cold-compiled the workspace on every release, 31 minutes. Moved to a step `env:` on the
/// publishing build alone — on the belief that an environment value stops at the exporter — it
/// inverted the failure: the warm-up became a 28-second import and each of the eighteen legs
/// recompiled the whole workspace instead, 24 minutes apiece (release v1.5.2). Both spellings are
/// the same wire format, which is why this rule reads the value rather than the syntax.
///
/// The three checks are one rule because any of them alone is satisfiable while broken: agreeing
/// on `${{ steps.epoch.outputs.value }}` in both files is textual agreement and per-commit drift,
/// and a correct workflow-level constant is undone by one step that overrides it.
pub(super) fn the_build_epoch_is_one_constant(root: &Path) -> anyhow::Result<Vec<Finding>> {
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

        // Anything naming the epoch outside its one declaration overrides it for that step —
        // `build-args:` and a step `env:` alike — which is the failure the constant exists to
        // prevent.
        let declared_line = declaration.map(|(line, _)| line).unwrap_or_default();
        for (index, line) in text.lines().enumerate() {
            if index + 1 == declared_line || is_comment(line) || !line.contains(EPOCH) {
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

    // Cross-file agreement, reported once against the first file in name order rather than n times.
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

/// The value of one key in a workflow's **top-level** `env:` block, and the line it is on.
///
/// Top-level only: an `env:` nested under a job or a step is the override this rule reports, so
/// reading one as the declaration would make the rule report nothing. Column zero is what
/// distinguishes them.
fn workflow_env_value(text: &str, key: &str) -> Option<(usize, String)> {
    let mut inside = false;
    for (index, line) in text.lines().enumerate() {
        if is_comment(line) || line.trim().is_empty() {
            continue;
        }
        if !line.starts_with([' ', '\t']) {
            inside = line.trim_end() == "env:";
            continue;
        }
        if inside
            && let Some(value) = line.trim().strip_prefix(key).and_then(|rest| {
                rest.strip_prefix(':')
                    .map(|value| value.trim().trim_matches(['"', '\'']).to_owned())
            })
        {
            return Some((index + 1, value));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repo_lint::tempdir;
    use std::fmt::Write as _;

    /// The bug this pins: release 1.3.1 minted the cosign OIDC token with
    /// `jq -er '.value' > "$RUNNER_TEMP/cosign-oidc-token"`. cosign reads that path with
    /// `os.ReadFile` and puts the bytes into Fulcio's `Authorization` header untrimmed, so the
    /// newline `jq` appends made Go's `net/http` refuse the request —
    /// `invalid header field value for "Authorization"`, raised at the POST and naming neither
    /// the newline nor the file. All nine images published unsigned.
    ///
    /// The rule has to tell a write from a read: the two steps that *consume* the token name the
    /// same file, and flagging those would make the rule unsatisfiable.
    #[test]
    fn a_newline_appending_write_of_the_oidc_token_is_caught() {
        // The shape that shipped 1.3.1.
        let broken = "           | jq -er '.value' > \"${RUNNER_TEMP}/cosign-oidc-token\"; then";
        assert_eq!(oidc_token_writes(broken), vec![(1, false)]);
        // `echo` is the other reflex, and appends the same newline.
        let echoed = "          echo \"$token\" > \"${RUNNER_TEMP}/cosign-oidc-token\"";
        assert_eq!(oidc_token_writes(echoed), vec![(1, false)]);

        // The fix.
        let fixed = "          printf '%s' \"$token\" > \"${RUNNER_TEMP}/cosign-oidc-token\"";
        assert_eq!(oidc_token_writes(fixed), vec![(1, true)]);

        // Reads are not writes: no `>`, so neither line is judged at all.
        let read = "          token=\"${RUNNER_TEMP}/cosign-oidc-token\"\n          \
                    cosign sign --yes --identity-token \"$token\" \"$IMAGE@$DIGEST\"";
        assert!(oidc_token_writes(read).is_empty());

        // A comment may describe the broken form — including this rule's own rationale in the
        // workflow — without tripping the rule.
        let documented = "          # never `jq -er '.value' > \"$RUNNER_TEMP/cosign-oidc-token\"`";
        assert!(oidc_token_writes(documented).is_empty());

        // The no-op guard: a renamed file leaves nothing to check, which the rule reports rather
        // than passing silently.
        assert!(oidc_token_writes("          printf '%s' \"$token\" > \"$other\"").is_empty());
    }

    /// The bug this pins, in both of the shapes it has taken. Release 1.5.1 passed
    /// `SOURCE_DATE_EPOCH=$(git log -1 --format=%ct)` as a build arg, so the release warm-up could
    /// import nothing `ci.yml` wrote and cold-compiled the workspace every time. Release 1.5.2
    /// moved the same value to a step `env:` on the publishing build, believing an environment
    /// value reaches only the exporter; buildx passes it as `build-arg:SOURCE_DATE_EPOCH` either
    /// way, so all eighteen legs recompiled instead — 24 minutes each, against a `builder` the
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

        // The 1.5.2 shape: workflow level, but derived. Read, and reported by the caller.
        let derived = "env:\n  SOURCE_DATE_EPOCH: ${{ steps.epoch.outputs.value }}\n";
        assert_eq!(
            workflow_env_value(derived, "SOURCE_DATE_EPOCH"),
            Some((2, "${{ steps.epoch.outputs.value }}".to_owned()))
        );

        // A step `env:` is nested, so it is not the declaration — the caller reports it as the
        // override it is.
        let step = "jobs:\n  build:\n    steps:\n      - env:\n          SOURCE_DATE_EPOCH: \"7\"\n";
        assert_eq!(workflow_env_value(step, "SOURCE_DATE_EPOCH"), None);

        // An unindented key ends the block; a later top-level `env:` is a different workflow's
        // shape, but a key after `jobs:` must not be read as one.
        let after = "env:\n  A: b\n\njobs:\n  x:\n    SOURCE_DATE_EPOCH: \"9\"\n";
        assert_eq!(workflow_env_value(after, "SOURCE_DATE_EPOCH"), None);

        // A comment describing the rule — including this one's own rationale in both workflows —
        // is not a declaration.
        let documented = "env:\n  # SOURCE_DATE_EPOCH: \"0\" was derived once; never again\n";
        assert_eq!(workflow_env_value(documented, "SOURCE_DATE_EPOCH"), None);

        // A prefix match is not a match: a different key must not satisfy the rule.
        let prefixed = "env:\n  SOURCE_DATE_EPOCH_NOTE: \"0\"\n";
        assert_eq!(workflow_env_value(prefixed, "SOURCE_DATE_EPOCH"), None);
    }

    /// The compose file pins images by digest, so the tag the rule has to read is
    /// `18-alpine@sha256:…` rather than anything as tidy as `18`.
    #[test]
    fn a_postgres_major_survives_the_shapes_a_pin_produces() {
        assert_eq!(major_of("18-alpine"), Some("18"));
        assert_eq!(
            major_of("18-alpine@sha256:9a8afca54e7861fd90fab5fdf4c42477a6b1cb7d29"),
            Some("18")
        );
        assert_eq!(major_of("18.4-alpine"), Some("18"));
        assert_eq!(major_of("18"), Some("18"));
        assert_eq!(major_of("\"17-alpine\""), Some("17"));
        // `pgvector/pgvector` tags as `pg<major>`; both spellings must reduce to the same number,
        // or the harness and the compose file can never agree after the pgvector move.
        assert_eq!(major_of("pg18"), Some("18"));
        assert_eq!(major_of("\"pg18\""), Some("18"));
        assert_eq!(
            major_of("pg18@sha256:691673308c99d2161ba298736f3147"),
            Some("18")
        );
        // Not a version: a floating tag pins nothing, so the rule must not read one as agreement.
        assert_eq!(major_of("latest"), None);
        assert_eq!(major_of("pglatest"), None);
        assert_eq!(major_of(""), None);
    }

    /// The digest and the tag are separated by the same character, and a real compose line
    /// carries both.
    ///
    /// The bug this pins: splitting the whole reference on its last colon lands inside
    /// `sha256:<hex>`, so the "tag" becomes hex, `major_of` rejects it, and the rule reports that
    /// the compose file pins no Postgres at all — while looking directly at the line that does.
    #[test]
    fn an_image_reference_survives_a_digest_pin_and_a_registry_port() {
        assert_eq!(
            image_reference("image: postgres:18-alpine@sha256:9a8afca54e7861fd90fab5fdf4c42"),
            Some(("postgres", "18-alpine"))
        );
        assert_eq!(
            image_reference("image: pgvector/pgvector:pg18@sha256:691673308c99d2161ba29873"),
            Some(("pgvector/pgvector", "pg18"))
        );
        // No digest, and a registry host carrying a port: the repository half has its own colon.
        assert_eq!(
            image_reference("image: pgvector/pgvector:pg18"),
            Some(("pgvector/pgvector", "pg18"))
        );
        assert_eq!(
            image_reference("image: ghcr.io:443/timschoenle/tankovault:1.2.0"),
            Some(("ghcr.io:443/timschoenle/tankovault", "1.2.0"))
        );
        // Not an image line at all, and an image with no tag: both must decline rather than
        // guess, or an untagged image would be read as pinning something.
        assert_eq!(image_reference("environment:"), None);
        assert_eq!(image_reference("image: postgres"), None);
    }

    /// Proves the advisory rule fires in *both* directions, which is the whole point of it: the
    /// bug it pins is that `cargo-deny` and `cargo-audit` read separate ignore lists, so an
    /// exception reviewed into one file leaves the other gate acting on a list nobody edited.
    #[test]
    fn an_advisory_ignored_by_only_one_gate_is_a_violation() {
        let root = tempdir("advisory-ignores");
        std::fs::create_dir_all(root.join(".cargo")).unwrap();
        let write = |name: &str, ids: &[&str]| {
            let mut body = String::new();
            for id in ids {
                let _ = writeln!(body, "    \"{id}\",");
            }
            std::fs::write(
                root.join(name),
                format!("[advisories]\nignore = [\n{body}]\n"),
            )
            .unwrap();
        };

        write("deny.toml", &["RUSTSEC-2023-0071", "RUSTSEC-2024-0436"]);
        write(".cargo/audit.toml", &["RUSTSEC-2024-0436"]);
        let findings = advisory_ignores_agree(&root).unwrap();
        assert_eq!(findings.len(), 1);
        assert!(findings[0].detail.contains("RUSTSEC-2023-0071"));
        assert_eq!(findings[0].file, PathBuf::from(".cargo/audit.toml"));

        // The reverse: suppressed for the gate that reports, recorded in neither justification.
        write("deny.toml", &["RUSTSEC-2024-0436"]);
        write(
            ".cargo/audit.toml",
            &["RUSTSEC-2023-0071", "RUSTSEC-2024-0436"],
        );
        let findings = advisory_ignores_agree(&root).unwrap();
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].file, PathBuf::from("deny.toml"));

        write("deny.toml", &["RUSTSEC-2024-0436"]);
        write(".cargo/audit.toml", &["RUSTSEC-2024-0436"]);
        assert!(advisory_ignores_agree(&root).unwrap().is_empty());

        // A deleted key is not an empty list; it must stop the run rather than read as "no
        // exceptions", which is what an unconfigured gate also looks like.
        std::fs::write(root.join(".cargo/audit.toml"), "[advisories]\n").unwrap();
        assert!(advisory_ignores_agree(&root).is_err());

        std::fs::remove_dir_all(&root).ok();
    }
}
