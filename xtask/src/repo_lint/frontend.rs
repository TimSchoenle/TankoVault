//! Rules over the served SPA shell and the frontend sources: the Content-Security-Policy and
//! the two injection sinks it exists to make unreachable.

use std::path::Path;

use super::Finding;
use super::text::{is_comment, scan, walk};

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
