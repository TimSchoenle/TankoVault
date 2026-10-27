//! The frontend's function-length limit, applied to `#[component]` functions, which clippy
//! cannot see into.

use std::path::Path;

use super::Finding;
use super::text::{is_comment, rust_sources};

/// The frontend crates whose components the limit covers.
const FRONTEND_SOURCES: [&str; 2] = ["web/frontend/src", "web/frontend/crates/inkstone-ui/src"];

/// The clippy configuration that sets the frontend's `too_many_lines` threshold.
const CLIPPY_CONFIG: &str = "web/frontend/clippy.toml";

/// Every `#[component]` function stays within the frontend's `too-many-lines-threshold`.
///
/// `#[component]` re-emits the function from a proc macro, and clippy skips `too_many_lines`
/// on macro output, so the threshold in `clippy.toml` held for helpers and silently not for
/// components. This rule reads the same threshold, so the two cannot drift apart.
///
/// # Errors
/// `clippy.toml` is unreadable or does not set `too-many-lines-threshold`.
pub(super) fn components_fit_the_line_threshold(root: &Path) -> anyhow::Result<Vec<Finding>> {
    let config_path = root.join(CLIPPY_CONFIG);
    let config = std::fs::read_to_string(&config_path)
        .map_err(|e| anyhow::anyhow!("{}: {e}", config_path.display()))?;
    let threshold = line_threshold(&config).ok_or_else(|| {
        anyhow::anyhow!("{CLIPPY_CONFIG} does not set `too-many-lines-threshold`")
    })?;

    let mut findings = Vec::new();
    for path in rust_sources(root, &FRONTEND_SOURCES) {
        let Ok(source) = std::fs::read_to_string(&path) else {
            continue;
        };
        for component in long_components(&source, threshold) {
            findings.push(Finding {
                rule: "component-line-threshold",
                file: path.strip_prefix(root).unwrap_or(&path).to_path_buf(),
                line: component.line,
                detail: format!(
                    "`{}` has {} lines; the threshold in {CLIPPY_CONFIG} is {threshold}. Split \
                     it into one component per screen region",
                    component.name, component.lines
                ),
            });
        }
    }
    Ok(findings)
}

/// The `too-many-lines-threshold` value in a clippy configuration.
fn line_threshold(config: &str) -> Option<usize> {
    config.lines().find_map(|line| {
        let (key, value) = line.split_once('=')?;
        (key.trim() == "too-many-lines-threshold").then(|| value.trim().parse().ok())?
    })
}

/// A component over the threshold: its name, the line of its `fn`, and its line count.
#[derive(Debug, PartialEq, Eq)]
struct LongComponent {
    name: String,
    line: usize,
    lines: usize,
}

/// The `#[component]` functions in `source` with more than `threshold` lines.
///
/// Lines are counted as clippy counts them: from the signature to the closing brace, leaving
/// out blank and comment lines. A component ends at the first `}` at its own indentation, which
/// `rustfmt` guarantees.
fn long_components(source: &str, threshold: usize) -> Vec<LongComponent> {
    let lines: Vec<&str> = source.lines().collect();
    let mut found = Vec::new();
    let mut index = 0;
    while index < lines.len() {
        if lines[index].trim() != "#[component]" {
            index += 1;
            continue;
        }
        let indent = &lines[index][..lines[index].len() - lines[index].trim_start().len()];
        let Some(start) = (index + 1..lines.len()).find(|&i| fn_name(lines[i]).is_some()) else {
            break;
        };
        let close = format!("{indent}}}");
        let end = (start..lines.len())
            .find(|&i| lines[i] == close)
            .unwrap_or(lines.len() - 1);
        let counted = lines[start..=end]
            .iter()
            .filter(|line| !line.trim().is_empty() && !is_comment(line))
            .count();
        if counted > threshold {
            found.push(LongComponent {
                name: fn_name(lines[start]).unwrap_or_default().to_owned(),
                line: start + 1,
                lines: counted,
            });
        }
        index = end + 1;
    }
    found
}

/// The function name `line` declares, if it declares one.
fn fn_name(line: &str) -> Option<&str> {
    let rest = line.trim_start().split("fn ").nth(1)?;
    let before = line.trim_start().split("fn ").next()?;
    let qualifiers_only = before
        .split_whitespace()
        .all(|word| word.starts_with("pub") || word == "async" || word == "const");
    if !qualifiers_only {
        return None;
    }
    let end = rest
        .find(|c: char| !(c.is_alphanumeric() || c == '_'))
        .unwrap_or(rest.len());
    (end > 0).then(|| &rest[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn component(name: &str, body_lines: usize) -> String {
        use std::fmt::Write as _;

        let mut source = format!("#[component]\npub fn {name}() -> Element {{\n");
        for i in 0..body_lines {
            let _ = write!(source, "    let _{i} = {i};\n\n    // a comment\n");
        }
        source.push_str("}\n");
        source
    }

    #[test]
    fn a_component_over_the_threshold_is_reported_with_its_counted_lines() {
        let source = format!("{}\n{}", component("Short", 3), component("Long", 10));
        assert_eq!(
            long_components(&source, 10),
            vec![LongComponent {
                name: "Long".to_owned(),
                line: 15,
                lines: 12,
            }]
        );
    }

    #[test]
    fn blank_and_comment_lines_do_not_count() {
        assert_eq!(long_components(&component("Exact", 8), 10), vec![]);
    }

    #[test]
    fn a_plain_function_is_left_to_clippy() {
        let source = "pub fn helper() {\n    let a = 1;\n    let b = 2;\n}\n";
        assert_eq!(long_components(source, 1), vec![]);
    }

    #[test]
    fn the_threshold_is_read_from_its_key_only() {
        assert_eq!(
            line_threshold("too-many-arguments-threshold = 9\ntoo-many-lines-threshold = 250\n"),
            Some(250)
        );
        assert_eq!(line_threshold("too-many-arguments-threshold = 9\n"), None);
    }
}
