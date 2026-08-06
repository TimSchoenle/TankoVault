//! Rules over `.gitattributes`: that a generated artefact checks out the way its generator writes
//! it.

use std::path::{Path, PathBuf};

use super::Finding;

/// **Every generated artefact must declare `eol=lf`.**
///
/// The gates that guard these files compare *bytes*: `xtask openapi --check` tests a freshly
/// rendered document against the committed one for exact equality, `xtask notices --check` does
/// the same for `THIRD-PARTY-NOTICES`, and every generator writes `\n`. `.gitattributes` also
/// says `* text=auto`, so without an explicit `eol` these check out CRLF wherever
/// `core.autocrlf` is on — and the comparison then fails for every Windows developer on a clean
/// tree, having verified nothing.
///
/// It fails *misleadingly*, which is why this is a rule rather than a convention. The message is
/// "openapi.json is out of date; run `cargo run -p xtask -- openapi`" — so the obvious reading is
/// that the artefact is stale, and doing what it says rewrites the file with LF, leaves `git
/// diff` empty because the index was already normalised, and leaves the gate red. `xtask ci` is
/// this repository's definition of done, and it could not pass on the platform `.gitattributes`
/// itself says the repository is developed on.
///
/// `linguist-generated=true` is the marker because it already means "this file is written by a
/// tool, not a human" everywhere in that file, so a new artefact is caught by the attribute its
/// author already has to add to keep it out of the review surface.
///
/// This is one half of the contract. The other is that the generator emit LF in the first place;
/// see `rustfmt_emits_lf_whatever_the_host_does` in `xtask/src/main.rs`, where rustfmt's `Auto`
/// newline style was defaulting to CRLF on Windows.
pub(super) fn generated_artefacts_check_out_as_lf(root: &Path) -> anyhow::Result<Vec<Finding>> {
    let path = PathBuf::from(".gitattributes");
    let absolute = root.join(&path);
    let contents = std::fs::read_to_string(&absolute)
        .map_err(|e| anyhow::anyhow!("{}: {e}", absolute.display()))?;

    Ok(offenders(&contents)
        .into_iter()
        .map(|(line, pattern)| Finding {
            rule: "generated-artefacts-eol",
            file: path.clone(),
            line,
            detail: format!(
                "`{pattern}` is marked `linguist-generated=true` but does not declare `eol=lf`, \
                 so it checks out CRLF under `core.autocrlf` and the byte comparison that guards \
                 it fails on a clean tree"
            ),
        })
        .collect())
}

/// The `linguist-generated=true` patterns that do not also carry `eol=lf`, with 1-based lines.
///
/// Split out so the parse is testable without a filesystem, like the rest of these rules. A
/// line-based read: `.gitattributes` is `<pattern> <attr>...` per line, and attributes for one
/// path may legally be spread over several lines — this requires them together, which is how
/// every entry in this repository's file is written and the only form that reads unambiguously.
fn offenders(contents: &str) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    for (index, line) in contents.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let mut fields = trimmed.split_whitespace();
        let Some(pattern) = fields.next() else {
            continue;
        };
        let attributes: Vec<&str> = fields.collect();
        if attributes.contains(&"linguist-generated=true") && !attributes.contains(&"eol=lf") {
            out.push((index + 1, pattern.to_owned()));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_generated_artefact_without_eol_is_reported() {
        let found = offenders("openapi.json linguist-generated=true -diff\n");
        assert_eq!(found, vec![(1, "openapi.json".to_owned())]);
    }

    #[test]
    fn declaring_eol_lf_satisfies_the_rule() {
        assert!(offenders("openapi.json linguist-generated=true -diff eol=lf\n").is_empty());
    }

    /// The rule fires on the marker, not on every line: a path that is merely `-diff`, or the
    /// `* text=auto` catch-all, is not a generated artefact and must not be dragged in.
    #[test]
    fn only_generated_paths_are_held_to_it() {
        assert!(offenders("* text=auto\nfuzz/seeds/** -text\nsome/path -diff\n").is_empty());
    }

    /// Prose describing the rule contains the very attribute it looks for, so a comment that is
    /// not skipped makes documenting the rule break it — the reason [`super::super::text`] has an
    /// `is_comment` at all.
    #[test]
    fn prose_naming_the_attribute_does_not_fire() {
        assert!(offenders("# openapi.json linguist-generated=true and no eol\n").is_empty());
    }
}
