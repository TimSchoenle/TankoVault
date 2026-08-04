//! `xtask repo-lint` — invariants no compiler or linter can see: two artefacts that must agree
//! with nothing connecting them (a CSP and the HTML it governs, a secret published in a compose
//! file and the code meant to refuse it). These are text scans, not parsers: comment lines are
//! skipped ([`is_comment`]) so a rule can't fire on the prose describing it, and every allowance
//! is an explicit path listed in the rule rather than a sprinklable "ignore" comment.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

/// One violation: where, and what is wrong with it.
struct Finding {
    rule: &'static str,
    file: PathBuf,
    line: usize,
    detail: String,
}

/// Run every rule over `root`.
///
/// # Errors
/// Every violation found, listed. Rules run to completion rather than stopping at the first,
/// so one invocation reports the whole set.
pub(crate) fn run(root: &Path) -> anyhow::Result<()> {
    let mut findings = Vec::new();
    // Every rule below the first two reads one specific, required artefact; a missing one is a
    // broken checkout, not a clean bill of health, so those return `Result` and stop the run.
    findings.extend(no_unsafe_eval(root));
    findings.extend(no_dangerous_inner_html(root));
    findings.extend(shell_loads_nothing_off_origin(root)?);
    findings.extend(published_secrets_are_refused(root)?);
    findings.extend(dockerfile_ships_every_workspace_binary(root)?);
    findings.extend(deploy_blacklist_is_honoured(root)?);
    findings.extend(build_inputs_are_classified(root)?);
    findings.extend(notices_accept_every_allowed_licence(root)?);
    findings.extend(the_notices_url_is_the_one_the_server_publishes(root)?);
    findings.extend(tests_run_the_production_postgres_major(root)?);
    findings.extend(advisory_ignores_agree(root)?);
    findings.extend(every_metric_is_described(root)?);
    findings.extend(every_service_serves_metrics(root)?);
    findings.extend(concurrency_groups_hold_at_most_two_workflows(root)?);

    if findings.is_empty() {
        println!("repo-lint: 14 rules, no violations");
        return Ok(());
    }

    let mut report = String::from("repo-lint found violations:\n");
    for finding in &findings {
        let _ = writeln!(
            report,
            "  [{}] {}:{} — {}",
            finding.rule,
            finding.file.display(),
            finding.line,
            finding.detail
        );
    }
    let _ = write!(
        report,
        "\nEach rule is documented in xtask/src/repo_lint.rs; \
         docs/ENGINEERING_GUIDE.md §5 says which gate owns what."
    );
    anyhow::bail!(report)
}

// ---------------------------------------------------------------------------------------
// Rules
// ---------------------------------------------------------------------------------------

fn no_unsafe_eval(root: &Path) -> Vec<Finding> {
    scan(
        root,
        "csp-no-unsafe-eval",
        &["rs", "html", "yml", "yaml", "toml", "conf", "nginx"],
        &["docs", "target", "mutants.out", "mutants.out.old"],
        |line| {
            grants_unsafe_eval(line).then(|| {
                "a CSP granting this makes an injected string executable, and the SPA's access \
                 token lives in memory. `'wasm-unsafe-eval'` is the directive the app needs; \
                 nothing in it needs the other"
                    .to_owned()
            })
        },
    )
}

/// Whether `line` grants `'unsafe-eval'` in a Content-Security-Policy.
///
/// Split out from the scan so the decision can be tested against the strings that must and
/// must not trip it, without a filesystem.
fn grants_unsafe_eval(line: &str) -> bool {
    const DIRECTIVES: [&str; 5] = [
        "default-src",
        "script-src",
        "script-src-elem",
        "style-src",
        "worker-src",
    ];

    line.contains("'unsafe-eval'") && DIRECTIVES.iter().any(|directive| line.contains(directive))
}

fn no_dangerous_inner_html(root: &Path) -> Vec<Finding> {
    // The one legitimate use: `icons::Ic` renders a compile-time-constant path, not
    // caller-supplied data. Budget of one, not a blanket exemption — a second use must be
    // argued in review, not inherited from this one.
    const ALLOWED: [(&str, usize); 1] = [("web/frontend/src/icons.rs", 1)];

    let mut findings = Vec::new();
    for path in walk(&root.join("web/frontend/src"), &["rs"], &["target"]) {
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let relative = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");
        let budget = ALLOWED
            .iter()
            .find_map(|(allowed, count)| (*allowed == relative).then_some(*count))
            .unwrap_or(0);

        let mut seen = 0;
        for (number, line) in text.lines().enumerate() {
            if is_comment(line) || !line.contains("dangerous_inner_html") {
                continue;
            }
            seen += 1;
            if seen <= budget {
                continue;
            }
            findings.push(Finding {
                rule: "no-dangerous-inner-html",
                file: path.clone(),
                line: number + 1,
                detail: if budget == 0 {
                    "renders unescaped markup from data this app does not control. Render \
                     through `rsx!`, which escapes"
                        .to_owned()
                } else {
                    format!(
                        "this file is allowed {budget} use(s) of the attribute and this is \
                         number {seen}. Argue the new one in review, then raise the budget"
                    )
                },
            });
        }
    }
    findings
}

/// **The app shell loads nothing off-origin.** `default-src 'self'` refuses a CDN `<script>` or
/// `<link>` in `web/frontend/index.html` at browser runtime and nothing at build time — the
/// symptom is a missing font or dead feature in production only, and CDN-loading a boot script
/// would also silently drop it from the CSP's inline-script hashes.
fn shell_loads_nothing_off_origin(root: &Path) -> anyhow::Result<Vec<Finding>> {
    let shell = root.join("web/frontend/index.html");
    let Ok(html) = std::fs::read_to_string(&shell) else {
        anyhow::bail!(
            "repo-lint: cannot read {} — the app shell is not optional",
            shell.display()
        );
    };

    let mut findings = Vec::new();
    for (number, line) in html.lines().enumerate() {
        if is_comment(line) {
            continue;
        }
        for scheme in ["http://", "https://", "//cdn.", "//fonts."] {
            if line.contains(&format!("src=\"{scheme}"))
                || line.contains(&format!("href=\"{scheme}"))
            {
                findings.push(Finding {
                    rule: "shell-is-same-origin",
                    file: shell.clone(),
                    line: number + 1,
                    detail: format!(
                        "loads `{scheme}…`, which `default-src 'self'` refuses at runtime. \
                         Vendor the asset into web/frontend/assets/"
                    ),
                });
            }
        }
    }
    Ok(findings)
}

/// **A secret published in this repository must be refused by the code that reads it.** A
/// `${VAR:-value}` compose default is convenience for an ordinary setting but, for a
/// credential, a value anybody can read that an operator boots with unless they made
/// `deploy/local.env`. Every credential-shaped default must therefore appear literally in the
/// Rust refuse-lists (`services/api/src/main.rs::KNOWN_PLACEHOLDERS`,
/// `tankovault_service::internal_auth::KNOWN_PLACEHOLDERS`) — one half of the fix without the
/// other still boots with the published value.
fn published_secrets_are_refused(root: &Path) -> anyhow::Result<Vec<Finding>> {
    let compose = root.join("deploy/docker-compose.yml");
    let Ok(yaml) = std::fs::read_to_string(&compose) else {
        anyhow::bail!("repo-lint: cannot read {}", compose.display());
    };

    let sources = rust_sources(root, &["services", "crates"]);
    let mut haystack = String::new();
    for path in &sources {
        if let Ok(text) = std::fs::read_to_string(path) {
            haystack.push_str(&text);
        }
    }

    let mut findings = Vec::new();
    for (number, line) in yaml.lines().enumerate() {
        if is_comment(line) {
            continue;
        }
        let Some((name, default)) = compose_default(line) else {
            continue;
        };
        if !is_credential(&name) || default.is_empty() {
            continue;
        }
        if !haystack.contains(&default) {
            findings.push(Finding {
                rule: "published-secrets-are-refused",
                file: compose.clone(),
                line: number + 1,
                detail: format!(
                    "`{name}` defaults to `{default}`, which is published here and refused \
                     nowhere. Make the variable required (`:?`) and add the string to the \
                     service's KNOWN_PLACEHOLDERS"
                ),
            });
        }
    }
    Ok(findings)
}

/// **Every workspace binary must be listed in the Dockerfile's `SERVICE_BINS`.** The Dockerfile
/// compiles all binaries in one `cargo` invocation from a literal list, so a `[[bin]]` added to
/// the workspace without updating it produces an image that fails at the final `COPY` — only
/// once someone tries to build the service nobody knew was missing. This reads `SERVICE_BINS`
/// and every manifest's `[[bin]] name = …` and reports each direction of disagreement.
/// (`web/frontend` doesn't count: it's outside the host workspace and its `app` binary is a
/// `wasm32` artefact `dx` builds, not one any runtime stage copies.) Every workspace binary is
/// *built*; which of them may be *published* is [`deploy_blacklist_is_honoured`].
fn dockerfile_ships_every_workspace_binary(root: &Path) -> anyhow::Result<Vec<Finding>> {
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
fn deploy_blacklist_is_honoured(root: &Path) -> anyhow::Result<Vec<Finding>> {
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
fn build_inputs_are_classified(root: &Path) -> anyhow::Result<Vec<Finding>> {
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

/// **Every licence `cargo-deny` admits must be one the notices generator accepts, and vice
/// versa.** `deny.toml`'s `[licenses] allow` decides what may enter the dependency graph;
/// `about.toml`'s `accepted` decides which licence each crate's notice is published under in
/// `THIRD-PARTY-NOTICES`. Nothing connects the two lists, and each direction of drift fails
/// differently: a licence cargo-deny admits but the generator does not accept stops generation
/// dead with `--fail` (loud, but only in the job that regenerates), while one the generator
/// accepts and cargo-deny forbids is a standing invitation to publish a notice for terms that
/// must never be in the graph at all — the GPL-3.0 note in `deny.toml` is exactly that hazard,
/// and it is silent.
///
/// `web/frontend/about.toml` is deliberately *not* held to this: no `deny.toml` covers that
/// workspace, so its shorter list is the only licence gate the browser bundle has, and widening
/// it to match this one would retire the gate.
fn notices_accept_every_allowed_licence(root: &Path) -> anyhow::Result<Vec<Finding>> {
    const RULE: &str = "notices-accept-every-allowed-licence";

    let deny = root.join("deny.toml");
    let about = root.join("about.toml");
    let (Ok(deny_text), Ok(about_text)) = (
        std::fs::read_to_string(&deny),
        std::fs::read_to_string(&about),
    ) else {
        anyhow::bail!(
            "repo-lint: cannot read {} and {}",
            deny.display(),
            about.display()
        );
    };

    let Some((allow_line, allowed)) = toml_string_array(&deny_text, "[licenses]", "allow") else {
        anyhow::bail!(
            "repo-lint: {} declares no `[licenses] allow = [...]`; an absent list is not an \
             empty one — cargo-deny would admit nothing",
            deny.display()
        );
    };
    let Some((accepted_line, accepted)) = toml_string_array(&about_text, "", "accepted") else {
        anyhow::bail!(
            "repo-lint: {} declares no `accepted = [...]`; the notices generator would satisfy \
             no crate at all",
            about.display()
        );
    };

    let mut findings = Vec::new();
    for licence in &allowed {
        if !accepted.contains(licence) {
            findings.push(Finding {
                rule: RULE,
                file: about.clone(),
                line: accepted_line,
                detail: format!(
                    "`deny.toml` admits `{licence}` into the graph and this list does not \
                     accept it — `xtask notices` fails on the first crate that uses it"
                ),
            });
        }
    }
    for licence in &accepted {
        if !allowed.contains(licence) {
            findings.push(Finding {
                rule: RULE,
                file: deny.clone(),
                line: allow_line,
                detail: format!(
                    "`about.toml` accepts `{licence}`, which this list forbids — either it \
                     belongs in the graph and belongs here, or it does not and must not be \
                     publishable as a notice"
                ),
            });
        }
    }
    Ok(findings)
}

/// **The URL the SPA links its notices at must be the one the frontend service publishes
/// them at.** `web/frontend` is a separate workspace, so the two `NOTICES_ROUTE` literals have
/// no compile-time relationship. Getting them out of step does not 404: every unmatched path on
/// that server falls back to the app shell, so a stale link answers `200` with the application
/// itself, and the reader — who is owed those notices for the bundle their browser just ran —
/// sees a page that looks like it worked.
fn the_notices_url_is_the_one_the_server_publishes(root: &Path) -> anyhow::Result<Vec<Finding>> {
    const RULE: &str = "notices-url-matches-the-route";
    const SERVER: &str = "services/frontend/src/main.rs";
    const SPA: &str = "web/frontend/src/components/nav.rs";

    let mut routes = Vec::new();
    for relative in [SERVER, SPA] {
        let path = root.join(relative);
        let Ok(text) = std::fs::read_to_string(&path) else {
            anyhow::bail!("repo-lint: cannot read {}", path.display());
        };
        let Some((line, value)) = const_str(&text, "NOTICES_ROUTE") else {
            anyhow::bail!(
                "repo-lint: {} declares no `const NOTICES_ROUTE: &str = \"…\"`; the notices \
                 link and the route that serves it are held together by nothing else",
                path.display()
            );
        };
        routes.push((path, line, value));
    }

    let [
        (server_path, server_line, server_route),
        (spa_path, spa_line, spa_route),
    ] = routes.as_slice()
    else {
        unreachable!("two files were read")
    };

    if server_route == spa_route {
        return Ok(Vec::new());
    }
    Ok(vec![
        Finding {
            rule: RULE,
            file: spa_path.clone(),
            line: *spa_line,
            detail: format!(
                "the SPA links `{spa_route}` but the server publishes `{server_route}` \
                 ({SERVER}:{server_line}) — the link resolves to the app shell with a 200 \
                 rather than failing"
            ),
        },
        Finding {
            rule: RULE,
            file: server_path.clone(),
            line: *server_line,
            detail: format!("the other half of this disagreement is {SPA}:{spa_line}"),
        },
    ])
}

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
fn tests_run_the_production_postgres_major(root: &Path) -> anyhow::Result<Vec<Finding>> {
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
fn advisory_ignores_agree(root: &Path) -> anyhow::Result<Vec<Finding>> {
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

/// The value of a `const <name>: &str = "…";`, with its 1-based line number.
///
/// Split out so the parse can be tested without a filesystem. Deliberately anchored on `const`:
/// a `let` or a doc comment mentioning the name is not the declaration.
fn const_str(source: &str, name: &str) -> Option<(usize, String)> {
    for (index, line) in source.lines().enumerate() {
        let trimmed = line.trim_start();
        if is_comment(trimmed) {
            continue;
        }
        // A visibility modifier is not part of the declaration this rule is about — the
        // constant is the literal, whoever else may read it. Stripped rather than matched so
        // `pub`, `pub(crate)` and a bare `const` all parse.
        let trimmed = trimmed
            .strip_prefix("pub(crate) ")
            .or_else(|| trimmed.strip_prefix("pub(super) "))
            .or_else(|| trimmed.strip_prefix("pub "))
            .unwrap_or(trimmed);
        let Some(rest) = trimmed.strip_prefix("const ") else {
            continue;
        };
        let Some(rest) = rest.strip_prefix(name) else {
            continue;
        };
        let Some((_, value)) = rest.split_once('=') else {
            continue;
        };
        let value = value.trim().trim_end_matches(';').trim();
        return Some((index + 1, value.trim_matches('"').to_owned()));
    }
    None
}

/// The entries of a `<key> = [ "…", "…" ]` array in `table`, with the key's 1-based line number.
///
/// A line-based read of one known table, like the rest of this module. `table` is the header the
/// key must sit under (`""` for a top-level key), which is what keeps `[licenses] allow` from
/// being confused with `[bans] allow-wildcard-paths` or `[sources] allow-registry`.
fn toml_string_array(text: &str, table: &str, key: &str) -> Option<(usize, Vec<String>)> {
    let mut in_table = table.is_empty();
    let mut collecting = None;
    let mut entries = Vec::new();
    for (index, line) in text.lines().enumerate() {
        let trimmed = line.trim();
        if is_comment(trimmed) {
            continue;
        }
        if collecting.is_none() && trimmed.starts_with('[') {
            in_table = trimmed.starts_with(table) && !table.is_empty();
            continue;
        }
        if collecting.is_none() {
            let is_key = in_table
                && trimmed
                    .strip_prefix(key)
                    .is_some_and(|rest| rest.trim_start().starts_with('='));
            if is_key && trimmed.ends_with('[') {
                collecting = Some(index + 1);
            }
            continue;
        }
        if trimmed.starts_with(']') {
            return collecting.map(|line| (line, entries));
        }
        entries.extend(
            trimmed
                .split(',')
                .map(|entry| entry.trim().trim_matches('"').to_owned())
                .filter(|entry| !entry.is_empty()),
        );
    }
    None
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

// ---------------------------------------------------------------------------------------
// Scanning
// ---------------------------------------------------------------------------------------

/// Apply `check` to every non-comment line of every file under `root` with one of `extensions`,
/// skipping any path containing one of `excluded` as a component.
fn scan(
    root: &Path,
    rule: &'static str,
    extensions: &[&str],
    excluded: &[&str],
    check: impl Fn(&str) -> Option<String>,
) -> Vec<Finding> {
    let mut findings = Vec::new();
    for path in walk(root, extensions, excluded) {
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        for (number, line) in text.lines().enumerate() {
            if is_comment(line) {
                continue;
            }
            if let Some(detail) = check(line) {
                findings.push(Finding {
                    rule,
                    file: path.clone(),
                    line: number + 1,
                    detail,
                });
            }
        }
    }
    findings
}

/// Whether `line` is a comment in one of the languages scanned.
///
/// Load-bearing, not a nicety: every rule here has to be *described* somewhere, and the
/// description contains the string the rule forbids. Without this, documenting a rule breaks it.
fn is_comment(line: &str) -> bool {
    let trimmed = line.trim_start();
    trimmed.starts_with("//")        // Rust, including `///` and `//!`
        || trimmed.starts_with('#')  // YAML, TOML, shell
        || trimmed.starts_with("<!--")
        || trimmed.starts_with('*') // continuation of a block comment
}

/// Every file under `root` with one of `extensions`, skipping `excluded` directories.
///
/// Infallible by design: an unreadable directory is skipped rather than reported. The rules
/// this feeds are about what the tree *contains*, and a path the process cannot open contains
/// nothing it could be judged on. The two rules that read a specific, required file
/// (`index.html`, the compose file) check for it themselves and fail loudly.
fn walk(root: &Path, extensions: &[&str], excluded: &[&str]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with('.') {
                continue;
            }
            if path.is_dir() {
                if !excluded.contains(&name.as_ref()) {
                    stack.push(path);
                }
            } else if path
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| extensions.contains(&e))
            {
                out.push(path);
            }
        }
    }
    out.sort();
    out
}

/// The macros that create a series. `describe_*` is deliberately absent: it does not create
/// one, and matching it would make the catalogue's own describes look like call sites.
const RECORDING_MACROS: [&str; 3] = [
    "metrics::counter!(",
    "metrics::gauge!(",
    "metrics::histogram!(",
];

/// Path to the catalogue, which is the rule's reference set.
const CATALOGUE_SRC: &str = "crates/service/src/metrics.rs";

/// Every metric emitted anywhere in the workspace must have a row in
/// `tankovault_service::metrics::CATALOGUE`, and every row must be emitted.
///
/// Nothing else can catch this. A metric name is a string: a typo compiles, a new counter with
/// no row compiles and exposes a series with no `# HELP`, `# TYPE` or unit, and a row for a
/// metric nobody emits compiles and reads as coverage that does not exist — which is the exact
/// defect `docs/OBSERVABILITY.md` carried by hand until this rule replaced the hand-checking.
///
/// `crates/fetch` and `crates/solver` sit below `tankovault-service` and cannot import the
/// name constants, so their names are literals; this is what holds them to the list.
fn every_metric_is_described(root: &Path) -> anyhow::Result<Vec<Finding>> {
    let source = std::fs::read_to_string(root.join(CATALOGUE_SRC))?;
    let constants = metric_constants(&source);
    let described = catalogue_names(&source, &constants);

    let mut findings = Vec::new();
    let mut emitted: std::collections::HashSet<String> = std::collections::HashSet::new();

    for file in rust_sources(root, &["crates", "services"]) {
        let Ok(src) = std::fs::read_to_string(&file) else {
            continue;
        };
        for (line, name) in recorded_metrics(&src, &constants) {
            match name {
                Some(name) if described.contains(&name) => {
                    emitted.insert(name);
                }
                Some(name) => findings.push(Finding {
                    rule: "metrics-catalogue",
                    file: file.clone(),
                    line,
                    detail: format!(
                        "`{name}` is recorded here but has no row in CATALOGUE ({CATALOGUE_SRC}), \
                         so it is exposed with no HELP, TYPE or unit — and, if it is a histogram, \
                         with the exporter's summary quantiles instead of buckets"
                    ),
                }),
                None => findings.push(Finding {
                    rule: "metrics-catalogue",
                    file: file.clone(),
                    line,
                    detail: "the metric name here is not a literal or a `names::*` constant, so \
                             it cannot be held to the catalogue. Name it with a constant"
                        .to_owned(),
                }),
            }
        }
    }

    for name in &described {
        if !emitted.contains(name) {
            findings.push(Finding {
                rule: "metrics-catalogue",
                file: root.join(CATALOGUE_SRC),
                line: 0,
                detail: format!(
                    "`{name}` has a CATALOGUE row but nothing records it. A documented metric \
                     nobody emits reads as coverage that does not exist — drop the row, or wire \
                     the call site it promises"
                ),
            });
        }
    }
    findings.sort_by(|a, b| (&a.file, a.line, &a.detail).cmp(&(&b.file, b.line, &b.detail)));
    Ok(findings)
}

/// `pub const IDENT: &str = "value";` from the `names` module, as IDENT → value.
fn metric_constants(source: &str) -> std::collections::HashMap<String, String> {
    let mut out = std::collections::HashMap::new();
    for line in source.lines() {
        let line = line.trim();
        let Some(rest) = line.strip_prefix("pub const ") else {
            continue;
        };
        let Some((ident, value)) = rest.split_once(": &str = ") else {
            continue;
        };
        if let Some(value) = value
            .trim()
            .strip_suffix("\";")
            .and_then(|v| v.strip_prefix('"'))
        {
            out.insert(ident.trim().to_owned(), value.to_owned());
        }
    }
    out
}

/// The metric names the catalogue declares, resolved through [`metric_constants`].
fn catalogue_names(
    source: &str,
    constants: &std::collections::HashMap<String, String>,
) -> std::collections::HashSet<String> {
    let mut out = std::collections::HashSet::new();
    for line in source.lines() {
        let line = line.trim();
        let Some(rest) = line.strip_prefix("name: names::") else {
            continue;
        };
        if let Some(value) = constants.get(rest.trim_end_matches(',')) {
            out.insert(value.clone());
        }
    }
    out
}

/// Every recording macro in `src`, as (1-based line, resolved name).
///
/// `None` for a name that is neither a string literal nor a `…::IDENT` path, which the rule
/// reports rather than silently passing — an unresolvable name is a hole in the check.
fn recorded_metrics(
    src: &str,
    constants: &std::collections::HashMap<String, String>,
) -> Vec<(usize, Option<String>)> {
    let mut out = Vec::new();
    for macro_call in RECORDING_MACROS {
        let mut from = 0;
        while let Some(at) = src[from..].find(macro_call) {
            let open = from + at + macro_call.len();
            from = open;
            // Comment lines are skipped for the same reason every other rule skips them: the
            // prose documenting a metric must not read as an emission of it.
            let line_start = src[..open].rfind('\n').map_or(0, |i| i + 1);
            if is_comment(&src[line_start..open]) {
                continue;
            }
            let line = src[..open].matches('\n').count() + 1;
            let arg: String = src[open..]
                .chars()
                .take_while(|c| *c != ',' && *c != ')')
                .collect();
            let arg = arg.trim();
            let resolved =
                if let Some(literal) = arg.strip_prefix('"').and_then(|a| a.strip_suffix('"')) {
                    Some(literal.to_owned())
                } else {
                    constants
                        .get(arg.rsplit("::").next().unwrap_or(arg).trim())
                        .cloned()
                };
            out.push((line, resolved));
        }
    }
    out
}

/// Every long-running service must install the recorder, serve the scrape on its own port and
/// mount the ops probes.
///
/// This was convention, held only by copy-paste: a new service that omits
/// `spawn_metrics_server` compiles, passes every other gate, reports healthy, and is silently
/// unscrapable. `bootstrap` is exempt — it is a one-shot CLI that exits.
fn every_service_serves_metrics(root: &Path) -> anyhow::Result<Vec<Finding>> {
    const REQUIRED: [(&str, &str); 3] = [
        (
            "MetricsRegistry::install",
            "no recorder is installed, so every measurement in the process is dropped",
        ),
        (
            "spawn_metrics_server",
            "the scrape is never served on the isolated metrics port, so Prometheus reports \
             the target down while the service looks healthy",
        ),
        (
            "ops_router",
            "`/health` and `/ready` are unmounted, so an orchestrator cannot probe it",
        ),
    ];

    let mut findings = Vec::new();
    for entry in std::fs::read_dir(root.join("services"))? {
        let dir = entry?.path();
        let name = dir
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        if !dir.is_dir() || name == "bootstrap" || name == "test-support" {
            continue;
        }
        let sources: String = walk(&dir, &["rs"], &["target"])
            .iter()
            .filter_map(|f| std::fs::read_to_string(f).ok())
            .collect();
        for (needle, consequence) in REQUIRED {
            if !sources.contains(needle) {
                findings.push(Finding {
                    rule: "metrics-wiring",
                    file: dir.join("src"),
                    line: 0,
                    detail: format!("`{name}` never calls `{needle}`: {consequence}"),
                });
            }
        }
    }
    Ok(findings)
}

/// **No concurrency group may be shared by three or more workflows.** GitHub keeps at most *one*
/// pending run per `concurrency.group`. A third run entering a group that already has one running
/// and one pending does not queue behind them — it cancels the pending run outright. So a group
/// with three members silently drops a run on every event that fires all three, and which run is
/// dropped is decided by run-creation order: nothing a reader of the workflow files can see, and
/// nothing that shows up as a failure. The dropped run is reported as `cancelled`, which is what
/// a run that was superseded on purpose also looks like.
///
/// `auto-fix.yaml`, `auto-format.yaml` and `update-lockfile.yaml` shared `pr-autocommit-<pr>`
/// until 2026-08-04. On release 1.2.1, `release-please` force-pushed its release commit while
/// `auto-fix` was three minutes into a 33-minute run; `auto-format` took the pending slot and the
/// lockfile sync was cancelled one second after it was created. `Cargo.lock`,
/// `web/frontend/Cargo.lock` and `openapi.json` reached `main` still recording 1.2.0 against a
/// 1.2.1 manifest, and every `--locked` build failed — the release images included.
///
/// Two is the largest safe number, so two is what this rule allows.
fn concurrency_groups_hold_at_most_two_workflows(root: &Path) -> anyhow::Result<Vec<Finding>> {
    const RULE: &str = "concurrency-groups-hold-at-most-two-workflows";
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
        if members.len() < 3 {
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
                 group and cancels the pending one when a third arrives, so every event that \
                 fires all of them loses a run without reporting a failure",
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

/// Every `.rs` file under the named top-level directories of `root`.
fn rust_sources(root: &Path, dirs: &[&str]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for dir in dirs {
        out.extend(walk(&root.join(dir), &["rs"], &["target"]));
    }
    out
}

/// The variable name and literal default of a `${NAME:-default}` compose interpolation.
///
/// Returns `None` for `${NAME:?…}` (required, no default) and for lines with no interpolation,
/// which is most of them.
fn compose_default(line: &str) -> Option<(String, String)> {
    let start = line.find("${")?;
    let rest = &line[start + 2..];
    let end = rest.find('}')?;
    let inner = &rest[..end];
    let (name, default) = inner.split_once(":-")?;
    Some((name.trim().to_owned(), default.trim().to_owned()))
}

/// Whether a configuration key names a credential rather than an ordinary setting.
fn is_credential(name: &str) -> bool {
    const MARKERS: [&str; 5] = ["TOKEN", "SECRET", "PASSWORD", "PEPPER", "_KEY"];
    let upper = name.to_uppercase();
    MARKERS.iter().any(|marker| upper.contains(marker))
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn a_rules_own_documentation_does_not_trip_it() {
        // Every line here is a comment carrying the forbidden string. If `is_comment` ever
        // stops covering one of these, the rule that forbids it can no longer be documented.
        assert!(is_comment("// a CSP must never grant 'unsafe-eval'"));
        assert!(is_comment("/// see `dangerous_inner_html`"));
        assert!(is_comment("//! 'unsafe-eval'"));
        assert!(is_comment("  # TANKOVAULT_X: \"${X:-dev-secret}\""));
        assert!(is_comment("<!-- 'unsafe-eval' -->"));
        assert!(!is_comment("script-src 'self'"));
    }

    #[test]
    fn compose_defaults_are_read_but_required_variables_are_not() {
        assert_eq!(
            compose_default("  X: \"${TANKOVAULT_A__TOKEN:-dev-token}\""),
            Some(("TANKOVAULT_A__TOKEN".to_owned(), "dev-token".to_owned()))
        );
        // `:?` is the required form — there is no default to publish.
        assert_eq!(
            compose_default("  X: \"${TANKOVAULT_A__TOKEN:?set it}\""),
            None
        );
        // An empty default publishes nothing.
        assert_eq!(
            compose_default("  X: \"${TANKOVAULT_A__PEPPER:-}\""),
            Some(("TANKOVAULT_A__PEPPER".to_owned(), String::new()))
        );
        assert_eq!(compose_default("  X: \"literal\""), None);
    }

    #[test]
    fn credential_keys_are_told_apart_from_settings() {
        assert!(is_credential("TANKOVAULT_INTERNAL__TOKEN"));
        assert!(is_credential("TANKOVAULT_AUTH__JWT_SECRET"));
        assert!(is_credential("TANKOVAULT_AUTH__PASSWORD_PEPPER"));
        assert!(is_credential("TANKOVAULT_ANILIST__TOKEN_ENCRYPTION_KEY"));
        assert!(!is_credential("TANKOVAULT_TELEMETRY__JSON_LOGS"));
        assert!(!is_credential("TANKOVAULT_FRONTEND__STATIC_DIR"));
    }

    /// A rule only ever seen green is indistinguishable from one whose pattern never fires —
    /// exactly how a mistyped `clippy.toml` path behaves.
    #[test]
    fn the_csp_rule_fires_on_a_policy_and_not_on_prose_about_one() {
        // Assembled rather than written out, so this file doesn't contain the string it forbids.
        let granted = format!("script-src 'self' '{}'", "unsafe-eval");
        assert!(grants_unsafe_eval(&granted));
        assert!(grants_unsafe_eval(&format!(
            "  default-src '{}';",
            "unsafe-eval"
        )));

        // The directive the app genuinely needs. Contains the substring `unsafe-eval` and must
        // never be confused with it.
        assert!(!grants_unsafe_eval("script-src 'self' 'wasm-unsafe-eval'"));
        // The test in `services/frontend` that asserts the served header does *not* grant it:
        // it names the source expression but no directive, so it is not a policy.
        assert!(!grants_unsafe_eval(&format!(
            "assert!(!csp.contains(\"'{}'\"));",
            "unsafe-eval"
        )));
    }

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

    /// The array read has to be scoped to its table: `deny.toml` carries three keys beginning
    /// `allow`, in three tables, and picking the wrong one would compare the notices config
    /// against a list of registries.
    #[test]
    fn a_toml_array_is_read_from_its_own_table_only() {
        let deny = "[licenses]\n\
             version = 2\n\
             # allow = [\"GPL-3.0\"]\n\
             allow = [\n\
             \x20   \"MIT\",\n\
             \x20   # a comment between entries\n\
             \x20   \"Apache-2.0\",\n\
             ]\n\
             \n\
             [bans]\n\
             allow-wildcard-paths = true\n\
             deny = [\n\
             \x20   \"openssl\",\n\
             ]\n";
        let (line, entries) = toml_string_array(deny, "[licenses]", "allow").expect("the list");
        assert_eq!(line, 4, "the commented decoy on line 3 is not the key");
        assert_eq!(entries, ["MIT", "Apache-2.0"]);

        // `[bans] deny` is a list of crates, not licences: asking for it under `[licenses]`
        // must find nothing rather than the wrong array.
        assert!(toml_string_array(deny, "[licenses]", "deny").is_none());
        // A top-level key (`about.toml` has no tables at all) is `""`.
        assert_eq!(
            toml_string_array("accepted = [\n    \"MIT\",\n]\n", "", "accepted"),
            Some((1, vec!["MIT".to_owned()]))
        );
    }

    /// Both directions of the licence-list rule. The dangerous one is the second: a licence the
    /// notices generator accepts and `deny.toml` forbids publishes terms for something that
    /// must never be in the graph, and nothing else in the repository would say so.
    #[test]
    fn the_licence_lists_are_compared_in_both_directions() {
        let root = tempdir("licences");
        let write = |name: &str, body: &str| std::fs::write(root.join(name), body).unwrap();

        write(
            "deny.toml",
            "[licenses]\nallow = [\n    \"MIT\",\n    \"MPL-2.0\",\n]\n",
        );
        write(
            "about.toml",
            "accepted = [\n    \"MIT\",\n    \"MPL-2.0\",\n]\n",
        );
        assert!(
            notices_accept_every_allowed_licence(&root)
                .unwrap()
                .is_empty(),
            "equal lists are the passing case"
        );

        // Admitted into the graph, unknown to the generator: `xtask notices` dies on the first
        // crate that uses it.
        write("about.toml", "accepted = [\n    \"MIT\",\n]\n");
        let missing = notices_accept_every_allowed_licence(&root).unwrap();
        assert_eq!(missing.len(), 1);
        assert!(
            missing[0].detail.contains("MPL-2.0"),
            "{}",
            missing[0].detail
        );

        // Accepted by the generator, forbidden in the graph.
        write(
            "about.toml",
            "accepted = [\n    \"MIT\",\n    \"MPL-2.0\",\n    \"GPL-3.0\",\n]\n",
        );
        let extra = notices_accept_every_allowed_licence(&root).unwrap();
        assert_eq!(extra.len(), 1);
        assert!(extra[0].detail.contains("GPL-3.0"), "{}", extra[0].detail);

        std::fs::remove_dir_all(&root).ok();
    }

    /// The `const NOTICES_ROUTE` parse, including the two shapes that must not be mistaken for
    /// the declaration.
    #[test]
    fn the_notices_route_is_read_off_its_const() {
        let source = "//! const NOTICES_ROUTE: &str = \"/decoy\";\n\
             use axum::Router;\n\
             \n\
             const NOTICES_ROUTE: &str = \"/third-party-notices\";\n";
        assert_eq!(
            const_str(source, "NOTICES_ROUTE"),
            Some((4, "/third-party-notices".to_owned()))
        );
        assert_eq!(
            const_str("let NOTICES_ROUTE = \"/x\";\n", "NOTICES_ROUTE"),
            None
        );
        assert_eq!(
            const_str("const NOTICES: &str = \"x\";\n", "NOTICES_ROUTE"),
            None
        );
        // The SPA's copy is `pub(crate)` so the footer can link it too. Visibility is not part
        // of the declaration this rule is about, and reading it as one turned the whole gate
        // into a hard error the moment the constant gained a reader.
        assert_eq!(
            const_str(
                "pub(crate) const NOTICES_ROUTE: &str = \"/n\";\n",
                "NOTICES_ROUTE"
            ),
            Some((1, "/n".to_owned()))
        );
        assert_eq!(
            const_str("pub const NOTICES_ROUTE: &str = \"/n\";\n", "NOTICES_ROUTE"),
            Some((1, "/n".to_owned()))
        );
    }

    /// Proves the URL rule fires. It has to be proved rather than observed green, because the
    /// failure it guards against is itself invisible: the server answers every unmatched path
    /// with the app shell, so a stale link returns 200 and a page.
    #[test]
    fn a_stale_notices_link_is_a_violation() {
        let root = tempdir("notices-url");
        let server = root.join("services/frontend/src");
        let spa = root.join("web/frontend/src/components");
        std::fs::create_dir_all(&server).unwrap();
        std::fs::create_dir_all(&spa).unwrap();
        let write = |dir: &Path, name: &str, route: &str| {
            std::fs::write(
                dir.join(name),
                format!("const NOTICES_ROUTE: &str = \"{route}\";\n"),
            )
            .unwrap();
        };

        write(&server, "main.rs", "/third-party-notices");
        write(&spa, "nav.rs", "/third-party-notices");
        assert!(
            the_notices_url_is_the_one_the_server_publishes(&root)
                .unwrap()
                .is_empty()
        );

        write(&spa, "nav.rs", "/licenses");
        let findings = the_notices_url_is_the_one_the_server_publishes(&root).unwrap();
        // Both halves are reported: either file could be the one that moved.
        assert_eq!(findings.len(), 2);
        assert!(findings[0].detail.contains("/licenses"));
        assert!(findings[0].detail.contains("/third-party-notices"));

        std::fs::remove_dir_all(&root).ok();
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

    /// A unique scratch directory for the rules above, which read real paths.
    fn tempdir(purpose: &str) -> PathBuf {
        static COUNTER: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let unique = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "tankovault-repo-lint-{purpose}-{}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// The rules must pass against the tree they ship with. A rule that has never been run
    /// green is indistinguishable from one that does not work.
    #[test]
    fn the_repository_satisfies_its_own_rules() {
        let root = crate::workspace_root();
        run(root).expect("repo-lint is green on this tree");
    }
}
