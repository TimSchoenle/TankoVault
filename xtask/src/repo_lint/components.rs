//! The frontend's function-length limit, applied to `#[component]` functions, which clippy
//! cannot see into.

use std::path::Path;

use super::Finding;
use super::text::rust_sources;

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
/// A component ends at the first `}` at its own indentation, and its body starts after the first
/// line of the signature that ends in `{`, both of which `rustfmt` guarantees.
fn long_components(source: &str, threshold: usize) -> Vec<LongComponent> {
    let lines: Vec<&str> = source.lines().collect();
    let mut found = Vec::new();
    let mut index = 0;
    while index < lines.len() {
        if !is_component_attribute(lines[index].trim()) {
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
        let body_start = (start..end)
            .find(|&i| lines[i].trim_end().ends_with('{'))
            .map_or(end, |i| i + 1);
        let counted = code_lines(&lines[body_start..end]);
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

/// Whether `line` is Dioxus's component attribute, bare, path-qualified or with arguments.
fn is_component_attribute(line: &str) -> bool {
    let Some(inner) = line
        .strip_prefix("#[")
        .and_then(|rest| rest.strip_suffix(']'))
    else {
        return false;
    };
    let path = inner.split_once('(').map_or(inner, |(path, _)| path).trim();
    matches!(
        path,
        "component" | "dioxus::component" | "dioxus::prelude::component"
    )
}

/// Counts the lines of a function body that hold code, the way clippy's `too_many_lines` does.
///
/// A port of clippy's loop: a line counts when any of it lies outside a `//` or `/* */`
/// comment, and a block comment carries its state across lines. Any other rule disagrees with
/// clippy near the threshold, and this one exists to apply clippy's limit where clippy cannot.
fn code_lines(body: &[&str]) -> usize {
    let mut in_comment = false;
    let mut count = 0;
    for &full in body {
        let mut line = full;
        let mut code_in_line = false;
        loop {
            line = line.trim_start();
            if line.is_empty() {
                break;
            }
            if in_comment {
                if let Some(i) = line.find("*/") {
                    line = &line[i + 2..];
                    in_comment = false;
                    continue;
                }
            } else {
                let multi = line.find("/*").unwrap_or(line.len());
                let single = line.find("//").unwrap_or(line.len());
                code_in_line |= multi > 0 && single > 0;
                if multi < single {
                    line = &line[multi + 2..];
                    in_comment = true;
                    continue;
                }
            }
            break;
        }
        count += usize::from(code_in_line);
    }
    count
}

/// Returns the function name `line` declares, if it declares one.
fn fn_name(line: &str) -> Option<&str> {
    let (before, rest) = line.trim_start().split_once("fn ")?;
    let qualifiers_only = without_visibility(before)
        .split_whitespace()
        .all(|word| matches!(word, "async" | "const" | "unsafe"));
    if !qualifiers_only {
        return None;
    }
    let end = rest
        .find(|c: char| !(c.is_alphanumeric() || c == '_'))
        .unwrap_or(rest.len());
    (end > 0).then(|| &rest[..end])
}

/// Strips a leading `pub` or `pub(…)`, whose path may contain spaces, as in `pub(in crate::x)`.
fn without_visibility(qualifiers: &str) -> &str {
    let Some(rest) = qualifiers.trim_start().strip_prefix("pub") else {
        return qualifiers;
    };
    match rest.strip_prefix('(') {
        Some(scoped) => scoped.split_once(')').map_or(rest, |(_, after)| after),
        None => rest,
    }
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

    /// `#[component]` re-emits the function from a proc macro and clippy skips
    /// `too_many_lines` on macro output, so components grew past the threshold that held for
    /// every helper.
    #[test]
    fn a_component_over_the_threshold_is_reported_with_its_counted_lines() {
        let source = format!("{}\n{}", component("Short", 3), component("Long", 11));
        assert_eq!(
            long_components(&source, 10),
            vec![LongComponent {
                name: "Long".to_owned(),
                line: 15,
                lines: 11,
            }]
        );
    }

    /// Only a bare `#[component]` was matched, so `#[component(no_case_check)]` or a
    /// path-qualified attribute let an over-threshold component past the rule.
    #[test]
    fn every_form_of_the_component_attribute_is_measured() {
        for attribute in [
            "#[component(no_case_check)]",
            "#[dioxus::component]",
            "#[dioxus::prelude::component(no_case_check)]",
        ] {
            let source = component("Long", 11).replacen("#[component]", attribute, 1);
            assert_eq!(long_components(&source, 10).len(), 1, "{attribute}");
        }
        for attribute in [
            "#[component_helper]",
            "#[derive(Props)]",
            "#[not::component]",
        ] {
            let source = component("Long", 11).replacen("#[component]", attribute, 1);
            assert_eq!(long_components(&source, 10), vec![], "{attribute}");
        }
    }

    /// Clippy counts body lines only, so a component exactly at the threshold passes.
    #[test]
    fn a_component_at_the_threshold_passes() {
        assert_eq!(long_components(&component("Exact", 10), 10), vec![]);
    }

    #[test]
    fn blank_and_comment_lines_do_not_count() {
        assert_eq!(long_components(&component("Short", 8), 8), vec![]);
    }

    /// The counter skipped every line starting with `#` or `*`, so an attribute or a deref in a
    /// body went uncounted, and it counted the lines of a block comment that clippy skips.
    #[test]
    fn lines_are_counted_as_clippy_counts_them() {
        let body = [
            "#[expect(unused)]",
            "*slot = 1;",
            "/* a block",
            "   comment */",
            "let a = 1; /* trailing",
            "   still a comment */ let b = 2;",
            "// a line comment",
            "",
            "let url = \"https://example.com\";",
        ];
        assert_eq!(code_lines(&body), 5);
    }

    /// `pub(in crate::views) fn` splits into words at the space inside the parentheses, and the
    /// second word failed the qualifier check, so such a component was never measured.
    #[test]
    fn a_scoped_visibility_still_names_the_function() {
        assert_eq!(
            fn_name("pub(in crate::views) fn Panel() -> Element {"),
            Some("Panel")
        );
        assert_eq!(fn_name("pub(crate) async fn Load() {"), Some("Load"));
        assert_eq!(fn_name("fn Plain() {"), Some("Plain"));
        assert_eq!(fn_name("let f = fn_pointer;"), None);
        assert_eq!(fn_name("let x = 1; fn"), None);
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
