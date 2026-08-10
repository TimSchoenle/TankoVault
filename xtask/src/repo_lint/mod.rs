//! `xtask repo-lint` — invariants no compiler or linter can see: two artefacts that must agree
//! with nothing connecting them (a CSP and the HTML it governs, a secret published in a compose
//! file and the code meant to refuse it). These are text scans, not parsers: comment lines are
//! skipped ([`text::is_comment`]) so a rule can't fire on the prose describing it, and every
//! allowance is an explicit path listed in the rule rather than a sprinklable "ignore" comment.
//!
//! One module per artefact family; [`text`] holds the scanning primitives they share. A rule's
//! reason lives on the rule, so read it there before changing it.

mod deploy;
mod floors;
mod frontend;
mod gitattributes;
mod metrics;
mod notices;
mod secrets;
mod text;
mod tls;
mod workflows;

pub(crate) use deploy::{deploy_exclusions, service_bins};

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
    findings.extend(frontend::no_unsafe_eval(root));
    findings.extend(frontend::no_dangerous_inner_html(root));
    findings.extend(frontend::shell_loads_nothing_off_origin(root)?);
    findings.extend(frontend::autostart_entry_agrees(root)?);
    findings.extend(frontend::the_window_ceiling_matches_the_layout(root)?);
    findings.extend(frontend::resize_probes_are_siblings(root)?);
    findings.extend(secrets::published_secrets_are_refused(root)?);
    findings.extend(deploy::dockerfile_ships_every_workspace_binary(root)?);
    findings.extend(deploy::deploy_blacklist_is_honoured(root)?);
    findings.extend(deploy::build_inputs_are_classified(root)?);
    findings.extend(notices::notices_accept_every_allowed_licence(root)?);
    findings.extend(notices::the_notices_url_is_the_one_the_server_publishes(
        root,
    )?);
    findings.extend(workflows::tests_run_the_production_postgres_major(root)?);
    findings.extend(workflows::advisory_ignores_agree(root)?);
    findings.extend(metrics::every_metric_is_described(root)?);
    findings.extend(metrics::every_service_serves_metrics(root)?);
    findings.extend(workflows::concurrency_groups_hold_one_workflow(root)?);
    findings.extend(workflows::the_oidc_token_carries_no_newline(root)?);
    findings.extend(workflows::the_build_epoch_is_one_constant(root)?);
    findings.extend(workflows::registry_calls_in_publish_retry(root)?);
    findings.extend(workflows::release_please_tags_before_it_proposes(root)?);
    findings.extend(floors::coverage_floors_parse(root)?);
    findings.extend(gitattributes::generated_artefacts_check_out_as_lf(root)?);
    findings.extend(tls::every_service_installs_a_crypto_provider(root)?);

    if findings.is_empty() {
        println!("repo-lint: 23 rules, no violations");
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
        "\nEach rule is documented beside it in xtask/src/repo_lint/; \
         docs/ENGINEERING_GUIDE.md §5 says which gate owns what."
    );
    anyhow::bail!(report)
}

/// A unique scratch directory for the rules, which read real paths.
#[cfg(test)]
pub(super) fn tempdir(purpose: &str) -> PathBuf {
    static COUNTER: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let unique = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "tankovault-repo-lint-{purpose}-{}-{unique}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The rules must pass against the tree they ship with. A rule that has never been run
    /// green is indistinguishable from one that does not work.
    #[test]
    fn the_repository_satisfies_its_own_rules() {
        let root = crate::workspace_root();
        run(root).expect("repo-lint is green on this tree");
    }
}
