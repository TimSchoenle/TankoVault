//! What `docs/CONFIGURATION.md` claims, read out of the document. Keys are harvested only from
//! the **leftmost cell of a table row** — the document mentions keys constantly in prose, and
//! treating every backticked token as a claim would fire on sentences that are correct — and
//! keys under "Removed keys" (§8) are collected separately and asserted **absent** from the code.

use std::collections::BTreeSet;

/// What the document says, split by what the caller must do with it.
pub(super) struct Documented {
    /// Keys presented as live: they must exist in the code.
    pub(super) live: BTreeSet<String>,
    /// Keys under §8: they must *not* exist in the code.
    pub(super) removed: BTreeSet<String>,
}

/// Read every documented key out of `markdown`.
pub(super) fn parse(markdown: &str) -> Documented {
    let mut live = BTreeSet::new();
    let mut removed = BTreeSet::new();
    let mut in_removed_section = false;

    for line in markdown.lines() {
        let trimmed = line.trim_start();
        if let Some(heading) = trimmed.strip_prefix("## ") {
            in_removed_section = heading.to_ascii_lowercase().contains("removed keys");
            continue;
        }
        let Some(cell) = first_table_cell(trimmed) else {
            continue;
        };
        let target = if in_removed_section {
            &mut removed
        } else {
            &mut live
        };
        target.extend(keys_in_cell(cell));
    }

    Documented { live, removed }
}

/// The leftmost cell of a markdown table row, or `None` if the line is not one.
///
/// The header separator (`|---|---|`) is not a row and is skipped here rather than by the
/// caller, so a `---` cell can never be mistaken for a key.
fn first_table_cell(line: &str) -> Option<&str> {
    let inner = line.strip_prefix('|')?;
    let cell = inner.split('|').next()?.trim();
    (!cell
        .chars()
        .all(|c| c == '-' || c == ':' || c.is_whitespace()))
    .then_some(cell)
}

/// Expand one cell into the keys it documents, applying the suffix shorthand.
fn keys_in_cell(cell: &str) -> Vec<String> {
    let mut keys: Vec<String> = Vec::new();
    for token in cell.split('`').skip(1).step_by(2) {
        if let Some(suffix) = token.strip_prefix("__") {
            // `__BURST` after `TANKOVAULT_RATE_LIMIT__GLOBAL__PER_MINUTE` means
            // `TANKOVAULT_RATE_LIMIT__GLOBAL__BURST`: replace the last segment, not append.
            let expanded = if is_key_text(suffix) {
                keys.last()
                    .and_then(|previous| previous.rsplit_once("__"))
                    .map(|(parent, _)| format!("{parent}__{suffix}"))
            } else {
                None
            };
            if let Some(key) = expanded {
                keys.push(key);
            }
        } else if token.starts_with("TANKOVAULT_") && is_key_text(token) {
            keys.push(token.to_owned());
        }
    }
    keys
}

/// Whether a backticked token is a bare key rather than a pattern or a sentence.
///
/// `TANKOVAULT_SECURITY__*` and `TANKOVAULT_EMAIL__*` appear in the document as families, and a
/// family is not a key — accepting one would put a literal `*` in the comparison set.
fn is_key_text(token: &str) -> bool {
    !token.is_empty()
        && token
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

#[cfg(test)]
mod tests {
    use super::parse;

    #[test]
    fn a_key_is_read_from_the_first_cell_only() {
        let doc = "\
| Key | Notes |
|---|---|
| `TANKOVAULT_DATABASE__URL` | Replaces `TANKOVAULT_OLD_URL`, which did nothing. |
";
        let documented = parse(doc);
        assert!(documented.live.contains("TANKOVAULT_DATABASE__URL"));
        assert!(
            !documented.live.contains("TANKOVAULT_OLD_URL"),
            "a key named in prose is explanation, not a claim that it exists"
        );
    }

    /// The shorthand the rate-limit and `AniList` blocks already use.
    #[test]
    fn a_leading_underscore_suffix_replaces_the_last_segment() {
        let doc = "\
| Key | Default |
|---|---|
| `TANKOVAULT_RATE_LIMIT__GLOBAL__PER_MINUTE` / `__BURST` | `300` / `60` |
| `TANKOVAULT_ANILIST__CLIENT_ID` / `__CLIENT_SECRET` / `__REDIRECT_URI` | *(required)* |
";
        let live = parse(doc).live;
        assert!(live.contains("TANKOVAULT_RATE_LIMIT__GLOBAL__PER_MINUTE"));
        assert!(live.contains("TANKOVAULT_RATE_LIMIT__GLOBAL__BURST"));
        assert!(live.contains("TANKOVAULT_ANILIST__CLIENT_SECRET"));
        assert!(live.contains("TANKOVAULT_ANILIST__REDIRECT_URI"));
        assert_eq!(live.len(), 5);
    }

    /// A chained suffix expands against the key before it, not against the first key in the
    /// cell — otherwise `__COVER` after `__TITLE` would silently document the wrong thing.
    #[test]
    fn chained_suffixes_expand_against_their_immediate_predecessor() {
        let doc = "\
| Key |
|---|
| `TANKOVAULT_METADATA__PRIORITY__DESCRIPTION` / `__TITLE` / `__COVER` |
";
        let live = parse(doc).live;
        assert!(live.contains("TANKOVAULT_METADATA__PRIORITY__TITLE"));
        assert!(live.contains("TANKOVAULT_METADATA__PRIORITY__COVER"));
    }

    /// A family is not a key. Accepting `TANKOVAULT_SECURITY__*` would put a literal `*` into
    /// the comparison and report it as an undocumented key forever.
    #[test]
    fn a_wildcard_family_is_not_a_key() {
        let doc = "| `TANKOVAULT_SECURITY__*` | not read here |\n";
        assert!(parse(doc).live.is_empty());
    }

    /// §8's rows are the inverse claim, so they must not land in `live` — a removed key that
    /// still appeared there would be reported as missing from the code, which is the state the
    /// section is documenting on purpose.
    #[test]
    fn the_removed_section_is_collected_separately() {
        let doc = "\
## 4. Shared blocks

| `TANKOVAULT_DATABASE__URL` | required |

## 8. Removed keys

| `TANKOVAULT_TELEMETRY__OTLP_ENDPOINT` | It never exported anything. |
";
        let documented = parse(doc);
        assert_eq!(
            documented.live.iter().collect::<Vec<_>>(),
            ["TANKOVAULT_DATABASE__URL"]
        );
        assert_eq!(
            documented.removed.iter().collect::<Vec<_>>(),
            ["TANKOVAULT_TELEMETRY__OTLP_ENDPOINT"]
        );
    }

    /// A heading after §8 ends it. Without this the sections below "Removed keys" would all be
    /// read as removed, and a document that grows a §9 would quietly lose its whole tail.
    #[test]
    fn the_removed_section_ends_at_the_next_heading() {
        let doc = "\
## 8. Removed keys

| `TANKOVAULT_GONE` | why |

## 9. Something else

| `TANKOVAULT_STILL_HERE` | why |
";
        let documented = parse(doc);
        assert_eq!(
            documented.removed.iter().collect::<Vec<_>>(),
            ["TANKOVAULT_GONE"]
        );
        assert_eq!(
            documented.live.iter().collect::<Vec<_>>(),
            ["TANKOVAULT_STILL_HERE"]
        );
    }
}
