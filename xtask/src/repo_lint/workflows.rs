//! Rules over `.github/workflows`: the Postgres major the tests run, advisory ignores,
//! concurrency groups, the OIDC token the release signs with, and the order release-please tags
//! in.

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
            // Removing it is the one exception, and only in a job that runs no buildx: there the
            // value reaches no cache key, while leaving it set breaks every packager that sets
            // its own timestamps. Inside a job that *does* build, an `unset` is the same
            // regression by subtraction — buildx then passes no `build-arg` at all, so the build
            // misses every layer a build carrying the constant exported — and is still reported.
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

/// **Every registry command in the publishing jobs of `release-please.yaml` runs through the retry
/// helper.** GHCR reports a throttled token request as `403 Forbidden` / `DENIED` with no
/// `Retry-After` — indistinguishable from a permissions failure — so buildx, syft and cosign all
/// give up on the first response.
///
/// A release makes eighteen `build` legs and nine `manifest` legs hit that registry inside a few
/// minutes, and `ci.yml` reads the same registry's build cache on the same push. At v2.0.0 two
/// `manifest` legs lost that race one second apart, each *after* its image was pushed:
/// `publish api` on the SBOM's pull, and `publish bootstrap` between the two `imagetools create`
/// calls — which left Docker Hub carrying `v2.0.0` and GHCR not. At v2.1.0 it was the by-digest
/// push in `build`, which had no wrapper because the rule only covered `manifest`:
/// `bootstrap (arm64)` died on `failed to fetch oauth token: denied: denied` while its seventeen
/// siblings pushed to the same registry and passed. That is what makes this recur silently: it
/// looks like a flake, it is load, and the next call added outside the wrapper brings it back on
/// a release nobody is watching.
///
/// Scoped to those two jobs on purpose. `plan`'s `imagetools inspect` probes are *deliberately*
/// unretried: a failed probe there means "not published yet", which is a legitimate answer that a
/// retry would only make expensive.
pub(super) fn registry_calls_in_publish_retry(root: &Path) -> anyhow::Result<Vec<Finding>> {
    const RULE: &str = "registry-calls-in-publish-retry";
    /// The jobs whose every registry call is on the publishing path.
    const JOBS: [&str; 2] = ["build", "manifest"];
    /// The helper each of them is invoked through, by the name it is written and called under.
    const HELPER: &str = "registry-retry";
    /// The composite action that installs it, by the path a job references it at.
    const HELPER_ACTION: &str = "./.github/actions/registry-retry";
    /// Commands that reach a registry, in the spellings this workflow uses.
    const REGISTRY_COMMANDS: [&str; 5] = [
        "docker buildx build",
        "docker buildx imagetools",
        "docker pull",
        "cosign sign",
        "cosign attest",
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
                     rename it in xtask/src/repo_lint/workflows.rs too"
                ),
            });
        }
    }

    // The jobs above only *reference* the installer, so a wrapper that stopped being written would
    // leave every call above failing with "no such file" — a message about the helper's path, not
    // about the registry.
    let action = root
        .join(HELPER_ACTION.trim_start_matches("./"))
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

/// **release-please tags a release in one pass and proposes the next one in another, with the
/// draft's publish in between.** The action does both halves in a single process by default, and
/// the two disagree about when a tag exists: `draft: true` means the release the tagging half
/// creates carries no git tag until `desktop-release` publishes it, while the pull-request half
/// resolves "the previous release" by looking that tag up.
///
/// Run together, the pull-request half looks for a tag its own predecessor has not created yet,
/// finds nothing, and falls back to the version in `.release-please-manifest.json` with **no base
/// commit to bound the commit walk**. The version is therefore right and the range is the entire
/// history of the repository. Release v3.1.0 logged `looking for tagName: v3.1.0` / `No latest
/// release found` / `Considering: 259 commits` and opened #138: a 245-line changelog re-listing
/// every commit ever merged, and a 4.0.0 major bump — from re-counting `!` commits that had been
/// released a dozen tags earlier — for a `main` with nothing on it since v3.1.0.
///
/// Nothing else reports this. The action succeeds, the release publishes normally, and
/// `auto-merge-release-please.yml` would have merged the bogus pull request on schedule.
///
/// Both halves are checked, not just the ordering: an invocation that skips neither is the
/// original bug restored, and one that skips both is a workflow that silently stops releasing.
pub(super) fn release_please_tags_before_it_proposes(root: &Path) -> anyhow::Result<Vec<Finding>> {
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

/// Each `uses:` of `action`, as its 1-based line number and the non-comment lines of that step.
///
/// A step's continuation is everything indented further than its `-`, which is what bounds the
/// body without reading `with:` — the keys this rule looks for may sit under any of them.
fn action_steps<'text>(text: &'text str, action: &str) -> Vec<(usize, Vec<&'text str>)> {
    let mut steps = Vec::new();
    let mut lines = text.lines().enumerate().peekable();
    while let Some((index, line)) = lines.next() {
        if is_comment(line) || !line.contains(action) {
            continue;
        }
        let indent = line.len() - line.trim_start().len();
        let mut body = Vec::new();
        while let Some((_, next)) = lines.peek() {
            let trimmed = next.trim();
            if !trimmed.is_empty() && next.len() - next.trim_start().len() <= indent {
                break;
            }
            if !is_comment(next) && !trimmed.is_empty() {
                body.push(*next);
            }
            lines.next();
        }
        steps.push((index + 1, body));
    }
    steps
}

/// The name of the top-level job the 1-based `line` falls in.
fn job_at(text: &str, line: usize) -> Option<String> {
    text.lines()
        .take(line)
        .filter(|candidate| is_job_header(candidate))
        .map(|candidate| candidate.trim().trim_end_matches(':').to_owned())
        .last()
}

/// Whether the line opens a top-level job: jobs are the only two-space-indented keys in these
/// files, and everything nested inside one is indented further.
fn is_job_header(line: &str) -> bool {
    !is_comment(line)
        && line.starts_with("  ")
        && !line.starts_with("   ")
        && line.trim_end().ends_with(':')
}

/// The non-comment, non-empty lines of one top-level job, each with its 1-based line number.
///
/// Jobs are the only two-space-indented keys in these files, so the next one of those ends the
/// job — which is what keeps the rule off `plan`'s deliberately unretried probes.
fn job_lines<'text>(text: &'text str, job: &str) -> Vec<(usize, &'text str)> {
    let header = format!("  {job}:");
    let mut inside = false;
    let mut lines = Vec::new();
    for (index, line) in text.lines().enumerate() {
        if line.trim_end() == header {
            inside = true;
            continue;
        }
        if inside
            && line.starts_with("  ")
            && !line.starts_with("   ")
            && line.trim_end().ends_with(':')
        {
            break;
        }
        if inside && !is_comment(line) && !line.trim().is_empty() {
            lines.push((index + 1, line));
        }
    }
    lines
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

/// Whether the job the given line index falls in runs `build_action`.
///
/// Textual, like the rest of this module: a job is a two-space-indented key under `jobs:`, and
/// everything nested inside one is indented further, so the nearest such key at or above the line
/// is its job and the block runs to the next one. A line that sits above the first job header
/// counts as building, so an unrecognised layout is judged conservatively rather than waved
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

    /// The bug this pins: release 2.0.0 published seven of nine images. GHCR reports a throttled
    /// token request as `403 Forbidden` / `DENIED` with no `Retry-After`, which is the same
    /// response as a permissions failure, so nothing retried it — `publish api` died on the SBOM's
    /// pull and `publish bootstrap` between the two `imagetools create` calls, both *after* their
    /// images had been pushed, one second apart, while the seven legs that had finished their
    /// registry work moments earlier all passed.
    ///
    /// Release 2.1.0 then failed the same way one job earlier: the by-digest push in `build` was a
    /// `docker/build-push-action` step, which cannot be wrapped and was outside this rule, and
    /// `bootstrap (arm64)` died on `failed to fetch oauth token: denied: denied` while seventeen
    /// sibling legs pushed to the same registry and passed. Which is why the rule now reads both
    /// publishing jobs and counts `docker buildx build` as a registry call.
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
- uses: ./.github/actions/registry-retry\n      \
# a comment naming docker pull is prose, not a call\n      \
- run: \"${RUNNER_TEMP}/registry-retry\" docker buildx imagetools create --tag \"$TAG\"\n      \
- run: docker pull \"$IMAGE:$TAG\"\n  \
summary:\n    \
steps:\n      \
- run: cosign sign --yes \"$IMAGE@$DIGEST\"\n";

        let lines = job_lines(workflow, "manifest");
        // `plan`'s probe above and `summary`'s call below are outside the job, and the comment is
        // not a call — so the one unwrapped `docker pull` is all that is left to report.
        let unwrapped: Vec<usize> = lines
            .iter()
            .filter(|(_, line)| line.contains("docker pull"))
            .map(|(index, _)| *index)
            .collect();
        assert_eq!(unwrapped, vec![10]);
        // The installer is now a shared composite action, and the line referencing it is what the
        // rule reads as "installed" — a job that calls the helper without installing it is the
        // no-op the guard reports.
        assert!(
            lines
                .iter()
                .any(|(_, line)| line.contains("./.github/actions/registry-retry"))
        );
        assert!(!lines.iter().any(|(_, line)| line.contains("cosign sign")));
        assert!(!lines.iter().any(|(_, line)| line.contains("inspect")));
    }

    /// The v2.1.0 shape specifically: an unwrapped `docker buildx build` in the `build` job. It
    /// pushes to both registries by digest, so it is a registry call like any other — but it did
    /// not read as one while the command list only named `imagetools`, `pull`, `sign` and
    /// `attest`.
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
        let step =
            "jobs:\n  build:\n    steps:\n      - env:\n          SOURCE_DATE_EPOCH: \"7\"\n";
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

    /// The bug this pins: the desktop job in `release-please.yaml` inherited the workflow-level
    /// `SOURCE_DATE_EPOCH`, and appimagetool passes a `-mkfs-time` of its own to mksquashfs,
    /// which refuses a timestamp from the command line and the environment at once. Release
    /// v2.0.0's `AppImage` leg died as `linuxdeploy failed with exit code Some(1)` and took every
    /// installer for that release with it. The fix removes the variable in that one job — which
    /// this rule forbade outright, because until then any line naming the epoch outside its
    /// declaration was an override.
    ///
    /// The exception has to stay job-scoped. Ahead of a buildx step an `unset` is the original
    /// cache-key regression written backwards: buildx then passes no `build-arg` at all, so the
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

    /// The bug this pins: release v3.1.0. `release-please.yaml` invoked the action once, so it
    /// created the draft release — which carries no git tag until `desktop-release` publishes it —
    /// and then, in the same process, went looking for `v3.1.0` to work out what the *next* release
    /// contained. It found nothing (`looking for tagName: v3.1.0` / `No latest release found for
    /// path: .`), took the version from the manifest and the commit range from the beginning of
    /// history, and opened #138: `Considering: 259 commits`, a 245-line changelog re-listing the
    /// whole repository, and a 4.0.0 major bump from re-counting `!` commits released a dozen tags
    /// earlier — for a `main` with no commits on it since v3.1.0 at all.
    ///
    /// Nothing downstream would have caught it: the action succeeded, the release published
    /// normally, and `auto-merge-release-please.yml` merges release pull requests on a schedule.
    #[test]
    fn a_release_please_step_that_does_not_split_its_two_halves_is_caught() {
        const ACTION: &str = "googleapis/release-please-action";

        // The shape that opened #138: one invocation, doing both halves, before the tag exists.
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

        // The fix: tag, publish the draft, then propose — one half each, in that order.
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
        // input — and a comment naming that input is prose, not a second skip.
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
