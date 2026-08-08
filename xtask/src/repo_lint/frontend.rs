//! Rules over the served SPA shell and the frontend sources: the Content-Security-Policy and
//! the two injection sinks it exists to make unreachable.

use std::path::Path;

use super::Finding;
use super::text::{const_str, is_comment, scan, walk};

pub(super) fn no_unsafe_eval(root: &Path) -> Vec<Finding> {
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

pub(super) fn no_dangerous_inner_html(root: &Path) -> Vec<Finding> {
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
pub(super) fn shell_loads_nothing_off_origin(root: &Path) -> anyhow::Result<Vec<Finding>> {
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

/// **The installer and the app write the same autostart entry.** The NSIS "start when I sign in"
/// checkbox and the switch in the desktop settings sheet are two views of one
/// `HKCU\…\Run` value; renaming either side alone leaves the app starting at sign-in under a name
/// nothing in the UI can see, and no build step relates a `.hbs` template to a Rust constant.
pub(super) fn autostart_entry_agrees(root: &Path) -> anyhow::Result<Vec<Finding>> {
    let template = root.join("web/frontend/bundle/windows/installer.nsi.hbs");
    let platform = root.join("web/frontend/src/platform/desktop.rs");
    let (Ok(nsi), Ok(rust)) = (
        std::fs::read_to_string(&template),
        std::fs::read_to_string(&platform),
    ) else {
        anyhow::bail!(
            "repo-lint: cannot read {} or {} — neither is optional",
            template.display(),
            platform.display()
        );
    };

    let mut findings = Vec::new();
    for (nsi_define, rust_const, what) in [
        ("AUTOSTART_VALUE", "VALUE", "registry value name"),
        ("AUTOSTART_KEY", "KEY", "registry key path"),
    ] {
        let Some(expected) = quoted_after(&nsi, &format!("!define {nsi_define} ")) else {
            findings.push(Finding {
                rule: "autostart-entry-agrees",
                file: template.clone(),
                line: 1,
                detail: format!("no `!define {nsi_define} \"…\"` — the {what} has no definition"),
            });
            continue;
        };
        // Matched on the value, not on the literal's syntax: the key is a raw string on the Rust
        // side and a plain one in the template, and the point is that they mean the same thing.
        let Some((number, line)) = rust.lines().enumerate().find(|(_, line)| {
            line.trim_start()
                .starts_with(&format!("const {rust_const}: &str"))
        }) else {
            findings.push(Finding {
                rule: "autostart-entry-agrees",
                file: platform.clone(),
                line: 1,
                detail: format!("no `const {rust_const}: &str` to hold the {what} `{expected}`"),
            });
            continue;
        };
        if !line.contains(&format!("\"{expected}\"")) {
            findings.push(Finding {
                rule: "autostart-entry-agrees",
                file: platform.clone(),
                line: number + 1,
                detail: format!(
                    "disagrees with the installer's `{nsi_define}` (`{expected}`), so the two \
                     would write different {what}s and one would be orphaned"
                ),
            });
        }
    }
    Ok(findings)
}

/// **The desktop window's ceiling is built from the layout it has to hold.**
///
/// `platform::desktop` sizes the window from the rail, the content gutters and the widest
/// `--measure` any route asks for, so the window it opens is one the content column can actually
/// fill. Two of those live in `input.css` and one in `components::shell`, and nothing compiles a
/// Rust constant against a stylesheet — so a retuned `--rail-w` would silently leave the window
/// short again, which is the defect the sum replaced (a 1760px ceiling gave Discover five covers
/// across where the layout fits seven).
/// The grid probe's three measured boxes must be siblings, never nested.
///
/// **A resize event bubbles in the desktop build.** dioxus-desktop dispatches it to the target's
/// ancestors as well, so a box observed *inside* another delivers its own width to the outer
/// box's handler too — and the outer one holds the width the whole page size is derived from.
/// The probe used to be exactly that shape: a full-width `div` with the `--card` and `--gap`
/// spans inside it. Discover measured 1438 px, then 190, then 18, settled on one column and
/// fetched a one-column page into a seven-column grid. It survived review because on the *first*
/// screen of a session the window's own resize fires the outer box again afterwards and hides
/// it; only a fresh navigation shows it.
///
/// Nesting is checked as brace depth rather than by parsing `rsx!`: the three handlers are
/// written as siblings in one element, so they sit at one depth, and any re-nesting moves one of
/// them. Depth is counted over the whole file, which is why an unbalanced brace inside a string
/// would confuse it — there are none, and a false positive here is a failed lint, not a shipped
/// bug.
pub(super) fn resize_probes_are_siblings(root: &Path) -> anyhow::Result<Vec<Finding>> {
    let file = root.join("web/frontend/src/components/grid.rs");
    let Ok(source) = std::fs::read_to_string(&file) else {
        anyhow::bail!(
            "repo-lint: cannot read {} — it is not optional",
            file.display()
        );
    };

    let observed = resize_handler_depths(&source);
    let mut findings = Vec::new();
    if observed.len() < 2 {
        findings.push(Finding {
            rule: "resize-probes-are-siblings",
            file: file.clone(),
            line: 1,
            detail: format!(
                "expected the probe's resize handlers here and found {}. If the probe moved, \
                 move this rule with it — it is the only thing holding the boxes apart",
                observed.len()
            ),
        });
    }
    let Some((_, expected)) = observed.first().copied() else {
        return Ok(findings);
    };
    for (line, depth) in observed {
        if depth != expected {
            findings.push(Finding {
                rule: "resize-probes-are-siblings",
                file: file.clone(),
                line,
                detail: "this `onresize` is nested inside another probe box. A resize event \
                         bubbles in the desktop build, so the outer box reports the inner \
                         one's width and the page size is derived from it — see `GridFitProbe`"
                    .to_owned(),
            });
        }
    }
    Ok(findings)
}

/// Every `onresize` in `source`, as (1-based line, brace depth at that line).
fn resize_handler_depths(source: &str) -> Vec<(usize, i32)> {
    let mut depths = Vec::new();
    let mut depth = 0_i32;
    for (number, line) in source.lines().enumerate() {
        if !is_comment(line) && line.contains("onresize:") {
            depths.push((number + 1, depth));
        }
        if is_comment(line) {
            continue;
        }
        for character in line.chars() {
            match character {
                '{' => depth += 1,
                '}' => depth -= 1,
                _ => {}
            }
        }
    }
    depths
}

pub(super) fn the_window_ceiling_matches_the_layout(root: &Path) -> anyhow::Result<Vec<Finding>> {
    let css = root.join("web/frontend/input.css");
    let shell = root.join("web/frontend/src/components/shell.rs");
    let platform = root.join("web/frontend/src/platform/desktop.rs");
    let (Ok(styles), Ok(routes), Ok(rust)) = (
        std::fs::read_to_string(&css),
        std::fs::read_to_string(&shell),
        std::fs::read_to_string(&platform),
    ) else {
        anyhow::bail!(
            "repo-lint: cannot read {}, {} or {} — none is optional",
            css.display(),
            shell.display(),
            platform.display()
        );
    };

    let mut findings = Vec::new();
    let mut expect = |rust_const: &str, expected: Option<f64>, source: &str, what: &str| {
        let Some(expected) = expected else {
            findings.push(Finding {
                rule: "window-ceiling-matches-the-layout",
                file: platform.clone(),
                line: 1,
                detail: format!("cannot read the {what} from {source}"),
            });
            return;
        };
        let declared = const_str(&rust, rust_const).and_then(|(line, value)| {
            value
                .trim_end_matches("f64")
                .parse::<f64>()
                .ok()
                .map(|parsed| (line, parsed))
        });
        let Some((line, declared)) = declared else {
            findings.push(Finding {
                rule: "window-ceiling-matches-the-layout",
                file: platform.clone(),
                line: 1,
                detail: format!("no `const {rust_const}: f64` to hold the {what} ({expected})"),
            });
            return;
        };
        if (declared - expected).abs() > f64::EPSILON {
            findings.push(Finding {
                rule: "window-ceiling-matches-the-layout",
                file: platform.clone(),
                line,
                detail: format!(
                    "is {declared}, but {source} puts the {what} at {expected} — the window \
                     would be sized for a layout that no longer exists"
                ),
            });
        }
    };

    expect(
        "RAIL_WIDTH",
        first_css_px(&styles, "--rail-w"),
        "input.css",
        "navigation rail width",
    );
    expect(
        "CONTENT_GUTTER",
        first_css_px(&styles, "--gutter"),
        "input.css",
        "content gutter",
    );
    expect(
        "WIDEST_MEASURE",
        widest_measure(&routes),
        "components::shell::measure_for",
        "widest measured column",
    );
    Ok(findings)
}

/// The pixel value of the first `<token>:` declaration in `styles`.
///
/// The first, deliberately: `:root` opens the file and the narrow-viewport overrides come after,
/// and it is the `:root` value the desktop window is sized against — no window this rule is about
/// is narrow enough to reach those media queries.
fn first_css_px(styles: &str, token: &str) -> Option<f64> {
    without_block_comments(styles)
        .split_once(&format!("{token}:"))?
        .1
        .trim()
        .split_once("px")
        .and_then(|(value, _)| value.trim().parse().ok())
}

/// `styles` with every `/* … */` span removed.
///
/// [`is_comment`] is line-based and knows nothing of CSS block comments, while `input.css`
/// documents almost every token directly above the line declaring it. A scan that trusted lines
/// would read a number straight out of the sentence explaining the rule — which is how the first
/// draft of this reader answered `999px` to a comment.
fn without_block_comments(styles: &str) -> String {
    let mut out = String::with_capacity(styles.len());
    let mut rest = styles;
    while let Some((before, after)) = rest.split_once("/*") {
        out.push_str(before);
        // An unterminated comment swallows the remainder, which is what a CSS parser does too.
        let Some((_, tail)) = after.split_once("*/") else {
            return out;
        };
        rest = tail;
    }
    out.push_str(rest);
    out
}

/// The largest `"<n>px"` any route's measured column is set to.
fn widest_measure(routes: &str) -> Option<f64> {
    let body = routes.split_once("fn measure_for")?.1;
    body.split("=> \"")
        .skip(1)
        .filter_map(|rest| rest.split_once("px\"")?.0.parse::<f64>().ok())
        .fold(None, |widest: Option<f64>, value| {
            Some(widest.map_or(value, |w| w.max(value)))
        })
}

/// The contents of the first double-quoted run following `prefix`, or `None` if `prefix` is absent
/// or nothing is quoted after it on that line.
fn quoted_after(text: &str, prefix: &str) -> Option<String> {
    let line = text
        .lines()
        .find(|line| line.trim_start().starts_with(prefix))?;
    let rest = line.split_once(prefix)?.1;
    let opened = rest.strip_prefix('"')?;
    let (value, _) = opened.split_once('"')?;
    Some(value.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

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

    /// Same reasoning again, for the window-ceiling rule's two readers. `--gutter` is the case
    /// that matters: the narrow-viewport override comes later in the file, and taking *that* one
    /// would size the desktop window against a media query no desktop window ever reaches.
    #[test]
    fn the_ceiling_rule_reads_the_root_token_not_a_later_override() {
        let styles = "/* --rail-w: 999px in a comment */\n\
                      :root { --rail-w: 280px; --gutter: 40px; }\n\
                      @media (max-width: 820px) { :root { --gutter: 16px; } }\n";
        assert_eq!(first_css_px(styles, "--rail-w"), Some(280.0));
        assert_eq!(first_css_px(styles, "--gutter"), Some(40.0));
        assert_eq!(first_css_px(styles, "--nothing"), None);
    }

    /// The measured column is per route and the ceiling is built from the widest of them, so the
    /// reader has to take the maximum rather than the first — and `none` (the console's
    /// full-bleed opt-out) is not a width.
    #[test]
    fn the_ceiling_rule_takes_the_widest_measured_column() {
        let routes = "fn measure_for(route: &Route) -> &'static str {\n\
                      match route {\n\
                      Route::Home {} => \"1760px\",\n\
                      Route::Account {} => \"1120px\",\n\
                      Route::Console {} => \"none\",\n\
                      _ => \"1600px\",\n\
                      }\n}\n";
        assert_eq!(widest_measure(routes), Some(1760.0));
        assert_eq!(widest_measure("nothing here"), None);
    }

    /// Same reasoning as above: the autostart rule reads a value out of an NSIS `!define`, and a
    /// reader that silently returned `None` would make the rule pass for ever.
    #[test]
    fn the_autostart_rule_reads_an_nsis_define() {
        let script = "; !define AUTOSTART_VALUE \"a comment about it\"\n\
                      !define AUTOSTART_VALUE \"TankoVault\"\n\
                      !define AUTOSTART_KEY \"Software\\Microsoft\\Windows\\CurrentVersion\\Run\"\n";
        assert_eq!(
            quoted_after(script, "!define AUTOSTART_KEY ").as_deref(),
            Some(r"Software\Microsoft\Windows\CurrentVersion\Run")
        );
        assert_eq!(quoted_after(script, "!define MISSING ").as_deref(), None);
        // The `;` comment above is not a definition, and `starts_with` on the trimmed line is
        // what keeps it from being read as one.
        assert_eq!(
            quoted_after(script, "!define AUTOSTART_VALUE ").as_deref(),
            Some("TankoVault")
        );
    }
}
