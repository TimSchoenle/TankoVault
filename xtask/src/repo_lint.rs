//! `xtask repo-lint` — the invariants no compiler and no linter can see.
//!
//! # Why this exists
//!
//! `clippy.toml` covers everything expressible as "this path must not be called". What is left
//! is the shape of defect this repository keeps finding: **two artefacts that must agree, with
//! nothing connecting them.** A Content-Security-Policy in a Rust string and the HTML it
//! governs. A secret published in a compose file and the code that is supposed to refuse it.
//! Both halves are individually correct, review reads them on different days, and the
//! disagreement is invisible to every other gate.
//!
//! Each rule below exists because its invariant was already broken once, or because breaking it
//! is silent. A rule that only restates what `clippy` or a unit test already enforces does not
//! belong here — see the enforcement table in `docs/ENGINEERING_GUIDE.md` for which mechanism
//! owns which rule.
//!
//! # Scanning, and its limits
//!
//! These are text scans, not parsers. The two mitigations that make that honest:
//!
//! - **Comment lines are skipped** ([`is_comment`]). Without it every rule below would fire on
//!   the prose *describing* it — this module included — and the usual repair for that is to
//!   stop writing the prose, which is the wrong trade.
//! - **Every allowance is an explicit path, listed in the rule.** There is no "ignore" comment
//!   an author can sprinkle, because a suppression mechanism that is cheap to reach for stops
//!   recording anything.

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
    // The two scanning rules cannot fail to *run*: an unreadable path simply holds nothing to
    // judge. The two below read one specific, required artefact each, and a missing app shell
    // or compose file is a broken checkout rather than a clean bill of health — so those two
    // return `Result` and this function stops.
    findings.extend(no_unsafe_eval(root));
    findings.extend(no_dangerous_inner_html(root));
    findings.extend(shell_loads_nothing_off_origin(root)?);
    findings.extend(published_secrets_are_refused(root)?);

    if findings.is_empty() {
        println!("repo-lint: 4 rules, no violations");
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
    // The one legitimate use in the tree. `icons::Ic` interpolates `path_for(icon)`, which
    // returns `&'static str` from a closed `match` over an enum — the markup is a compile-time
    // constant and no caller-supplied value reaches it. Allowed as a *budget of one* rather
    // than a blanket exemption for the file: a second occurrence here is a new claim, and a new
    // claim should be argued in review rather than inherited from this one.
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

/// **The app shell loads nothing off-origin.**
///
/// `services/frontend` serves the SPA under `default-src 'self'`, so a CDN `<script>` or
/// `<link>` added to `web/frontend/index.html` is refused by the browser at runtime and by
/// nothing at build time. The symptom is a missing font or a dead feature in production only.
///
/// This also protects the CSP's inline-script hashes: the server hashes exactly the `<script>`
/// elements that carry no `src` (`services/frontend/src/main.rs::inline_script_hashes`), so an
/// author who "externalises" a boot script to a CDN silently loses both the hash and the load.
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

/// **A secret published in this repository must be refused by the code that reads it.**
///
/// `deploy/docker-compose.yml` supplies defaults with `${VAR:-value}`. For an ordinary setting
/// that is convenience; for a credential it means the value an operator runs with, when they
/// never created `deploy/local.env`, is one anybody can read here. The established repair is
/// two-sided — make the compose variable required (`:?`) *and* have the service refuse the
/// string in every profile — because either half alone leaves a path that boots with it.
///
/// This rule enforces the second half against the first: every credential-shaped default in
/// the compose file must appear literally in the Rust sources, which is where the refuse-lists
/// live (`services/api/src/main.rs::KNOWN_PLACEHOLDERS`,
/// `tankovault_service::internal_auth::KNOWN_PLACEHOLDERS`).
///
/// It was written against a tree where `TANKOVAULT_INTERNAL__TOKEN` had the prose and not the
/// refusal, on the credential authorizing every privileged inter-tier call.
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

    /// The half of a rule that is easy to get wrong is the half that has to *fire*. A rule
    /// only ever seen green is indistinguishable from one whose pattern never matches
    /// anything — which is precisely how a mistyped `clippy.toml` path behaves, and why that
    /// hazard is called out in both `clippy.toml` files.
    #[test]
    fn the_csp_rule_fires_on_a_policy_and_not_on_prose_about_one() {
        // Assembled rather than written out, so this file does not contain the very string it
        // forbids. That is not a trick to dodge the rule — it is the rule working: this test
        // failed on its own first run, which is the evidence that the pattern matches a real
        // policy. Keeping an exemption for this file instead would have removed that evidence.
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

    /// The rules must pass against the tree they ship with. A rule that has never been run
    /// green is indistinguishable from one that does not work.
    #[test]
    fn the_repository_satisfies_its_own_rules() {
        let root = crate::workspace_root();
        run(root).expect("repo-lint is green on this tree");
    }
}
