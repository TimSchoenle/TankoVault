//! Owns the rule that the OIDC token the release signs with is written without a trailing newline.

use super::PUBLISH_WORKFLOW;
use crate::repo_lint::Finding;
use crate::repo_lint::text::is_comment;
use std::path::{Path, PathBuf};

/// **The cosign OIDC token file is written with `printf '%s'`, never a newline-appending
/// redirect.** `release-please.yaml` mints one OIDC token per `publish` leg and hands cosign the
/// *path* to it, because a token in `argv` is readable from `/proc`. cosign reads that path with
/// `os.ReadFile` and puts the bytes straight into Fulcio's `Authorization` header: it trims
/// nothing. So a single trailing newline, which is what `jq -r`, `echo` and every other ordinary
/// redirect leave behind, makes Go's `net/http` reject the request before it is sent.
///
/// The failure names neither the file nor the newline: it is
/// `net/http: invalid header field value for "Authorization"`, raised at the Fulcio POST, and the
/// mint step ahead of it passes. A release then publishes every image unsigned, and the two
/// steps that consume the token (`sign` and `attest`) fail identically, so nothing in
/// the run points back at how the file was written.
///
/// Requiring at least one write keeps the rule from quietly becoming a no-op if the file is
/// renamed: a rename has to touch this constant too.
pub(in crate::repo_lint) fn the_oidc_token_carries_no_newline(
    root: &Path,
) -> anyhow::Result<Vec<Finding>> {
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
                 file was renamed, rename it in xtask/src/repo_lint/workflows/oidc.rs too"
            ),
        });
    }
    Ok(findings)
}

/// The file the minted token is written to, and the only safe way to write it.
const OIDC_TOKEN_FILE: &str = "cosign-oidc-token";
const OIDC_TOKEN_WRITER: &str = "printf '%s'";

/// Every line of `text` that redirects into the token file, paired with whether it writes the
/// token the one way that appends nothing.
///
/// A mention with no `>` is a *read* (`--identity-token "$token"`) and is not a write, so it is
/// not reported, which is why the rule cannot simply require `printf` on every matching line.
fn oidc_token_writes(text: &str) -> Vec<(usize, bool)> {
    text.lines()
        .enumerate()
        .filter(|(_, line)| {
            !is_comment(line) && line.contains(OIDC_TOKEN_FILE) && line.contains('>')
        })
        .map(|(index, line)| (index + 1, line.contains(OIDC_TOKEN_WRITER)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The bug this pins: the release minted the cosign OIDC token with
    /// `jq -er '.value' > "$RUNNER_TEMP/cosign-oidc-token"`. cosign reads that path with
    /// `os.ReadFile` and puts the bytes into Fulcio's `Authorization` header untrimmed, so the
    /// newline `jq` appends made Go's `net/http` refuse the request:
    /// `invalid header field value for "Authorization"`, raised at the POST and naming neither
    /// the newline nor the file. All nine images published unsigned.
    ///
    /// The rule has to tell a write from a read: the two steps that *consume* the token name the
    /// same file, and flagging those would make the rule unsatisfiable.
    #[test]
    fn a_newline_appending_write_of_the_oidc_token_is_caught() {
        // The broken shape.
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
        assert_eq!(oidc_token_writes(read), Vec::<(usize, bool)>::new());

        // A comment may describe the broken form (including this rule's own rationale in the
        // workflow) without tripping the rule.
        let documented = "          # never `jq -er '.value' > \"$RUNNER_TEMP/cosign-oidc-token\"`";
        assert_eq!(oidc_token_writes(documented), Vec::<(usize, bool)>::new());

        // The no-op guard: a renamed file leaves nothing to check, which the rule reports
        // instead of passing silently.
        assert_eq!(
            oidc_token_writes("          printf '%s' \"$token\" > \"$other\""),
            Vec::<(usize, bool)>::new()
        );
    }
}
