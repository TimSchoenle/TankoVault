//! Shared text-scanning primitives. Every rule is a text scan rather than a parser, so these
//! are what the rules are built from — most importantly [`is_comment`], which keeps a rule from
//! firing on the prose that documents it.

use std::path::{Path, PathBuf};

use super::Finding;

/// The value of a `const <name>: &str = "…";`, with its 1-based line number.
///
/// Split out so the parse can be tested without a filesystem. Deliberately anchored on `const`:
/// a `let` or a doc comment mentioning the name is not the declaration.
pub(super) fn const_str(source: &str, name: &str) -> Option<(usize, String)> {
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
pub(super) fn toml_string_array(
    text: &str,
    table: &str,
    key: &str,
) -> Option<(usize, Vec<String>)> {
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

// ---------------------------------------------------------------------------------------
// Scanning
// ---------------------------------------------------------------------------------------

/// Apply `check` to every non-comment line of every file under `root` with one of `extensions`,
/// skipping any path containing one of `excluded` as a component.
pub(super) fn scan(
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
pub(super) fn is_comment(line: &str) -> bool {
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
pub(super) fn walk(root: &Path, extensions: &[&str], excluded: &[&str]) -> Vec<PathBuf> {
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
pub(super) fn rust_sources(root: &Path, dirs: &[&str]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for dir in dirs {
        out.extend(walk(&root.join(dir), &["rs"], &["target"]));
    }
    out
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
}
