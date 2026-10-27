//! The Sentry release name agrees with the one the images are built with.

use super::PUBLISH_WORKFLOW;
use crate::repo_lint::Finding;
use std::path::{Path, PathBuf};

/// **The Sentry release name is one decision written in two files, and a disagreement is silent
/// in both directions.** `crates/service/src/sentry.rs` defaults `telemetry.sentry.release` to
/// `tankovault@<CARGO_PKG_VERSION>`, which is what every replica stamps on every event it sends.
/// `release-please.yaml` creates a release object under the same name, uploads the debug
/// information for it and finalises it, which is what makes an issue attributable to a deploy.
///
/// Nothing connects the two but the string. Sentry accepts events for a release it has never
/// been told about and accepts a release nothing ever reports to, so a rename on either side
/// produces a green release, a green deploy, and an empty "first seen in" on every issue from
/// then on. There is no error to read and no job to re-run — the only symptom is regression
/// detection quietly not working, which is exactly the kind of absence nobody notices.
///
/// The prefix is read out of the Rust source rather than hard-coded here, so renaming the
/// product in one place fails this rule instead of being duplicated into it.
pub(in crate::repo_lint) fn sentry_release_name_agrees(
    root: &Path,
) -> anyhow::Result<Vec<Finding>> {
    const RULE: &str = "sentry-release-name-agrees";
    const SERVICE: &str = "crates/service/src/sentry.rs";
    /// How the workflow spells the version it releases. `version`, not `tag_name`: the code
    /// builds its name from `CARGO_PKG_VERSION`, which carries no `v`.
    const VERSION_EXPRESSION: &str = "${{ needs.release-please.outputs.version }}";

    let service_path = root.join(SERVICE);
    let Ok(service) = std::fs::read_to_string(&service_path) else {
        anyhow::bail!("repo-lint: cannot read {}", service_path.display());
    };
    let workflow_path = root.join(PUBLISH_WORKFLOW);
    let Ok(workflow) = std::fs::read_to_string(&workflow_path) else {
        anyhow::bail!("repo-lint: cannot read {}", workflow_path.display());
    };

    let Some(prefix) = release_name_prefix(&service) else {
        return Ok(vec![Finding {
            rule: RULE,
            file: PathBuf::from(SERVICE),
            line: 0,
            detail: "no `format!(\"…{}\", env!(\"CARGO_PKG_VERSION\"))` builds the default Sentry \
                     release name, so this rule cannot tell what the workflow has to publish; if \
                     the default moved, move this rule with it"
                .to_owned(),
        }]);
    };

    let expected = format!("{prefix}{VERSION_EXPRESSION}");
    if workflow.contains(&expected) {
        return Ok(Vec::new());
    }
    Ok(vec![Finding {
        rule: RULE,
        file: PathBuf::from(PUBLISH_WORKFLOW),
        line: 0,
        detail: format!(
            "does not create the release `{expected}`; {SERVICE} tags every event with \
             `{prefix}<version>`, and a release object under any other name collects none of them"
        ),
    }])
}

/// The literal prefix of a `format!("…{}", env!("CARGO_PKG_VERSION"))` — `tankovault@` from
/// `format!("tankovault@{}", env!("CARGO_PKG_VERSION"))`.
///
/// Split out so the shapes that must *not* match can be tested without a filesystem: the same
/// file holds several other `format!` calls, and one of them ending in a placeholder would
/// otherwise be read as the release name.
fn release_name_prefix(text: &str) -> Option<String> {
    const OPEN: &str = "format!(\"";
    const VERSION: &str = "env!(\"CARGO_PKG_VERSION\")";
    const PLACEHOLDER: &str = "{}";
    /// Enough to reach the first argument of the call, and not enough to reach the next one.
    const LOOKAHEAD: usize = 64;

    let mut cursor = 0;
    while let Some(offset) = text[cursor..].find(OPEN) {
        let start = cursor + offset + OPEN.len();
        cursor = start;
        let Some(end) = text[start..].find('"') else {
            break;
        };
        let end = start + end;
        let literal = &text[start..end];
        let arguments: String = text[end..].chars().take(LOOKAHEAD).collect();
        if let Some(prefix) = literal.strip_suffix(PLACEHOLDER)
            && arguments.contains(VERSION)
        {
            return Some(prefix.to_owned());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The release name is the only thing tying a running replica's events to the release object
    /// this workflow creates, and neither side reports a mismatch — so the extractor has to find
    /// the *release* `format!` and not merely the first one that ends in a placeholder. Both
    /// near misses below appear in `crates/service/src/sentry.rs`.
    #[test]
    fn the_release_prefix_is_read_from_the_call_that_builds_it() {
        let source = r#"
            let dsn = format!("telemetry.sentry.dsn is not valid ({e})");
            let name = format!("service-{}", service_name);
            let release = format!("tankovault@{}", env!("CARGO_PKG_VERSION"));
        "#;
        assert_eq!(release_name_prefix(source).as_deref(), Some("tankovault@"));

        // A placeholder with a different argument is not the release name.
        let unrelated = r#"let name = format!("service-{}", service_name);"#;
        assert_eq!(release_name_prefix(unrelated), None);

        // Neither is a call whose literal carries no trailing placeholder.
        let inlined = r#"let release = format!("tankovault@{version}", );"#;
        assert_eq!(release_name_prefix(inlined), None);
    }
}
