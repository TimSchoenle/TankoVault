//! The app shell: read from the bundle, and branded with the operator's name before it is served.

use std::path::{Path, PathBuf};

use tankovault_config::BrandingConfig;

/// Rewrite the shell's `<title>` and description to name *this* deployment.
///
/// Both are pre-boot placeholders — the SPA replaces the document title from `/v1/branding` as
/// soon as it mounts — but they are what a reader sees for the several hundred milliseconds the
/// WASM bundle takes to download, and what a crawler or a link unfurler sees for good.
///
/// Applied to the source **before** [`content_security_policy`] hashes it, so the bytes the
/// browser receives and the bytes that were hashed stay the same generation. Only element text
/// and an attribute value change, never a `<script>`, so no hash actually moves — but deriving
/// the policy from the pre-rewrite source would make that a coincidence rather than a guarantee.
///
/// A shell whose markup has moved on is left alone rather than mangled: this is a string
/// replacement, not an HTML parser, and printing a warning is better than emitting broken markup.
///
/// [`content_security_policy`]: super::csp::content_security_policy
pub(super) fn brand_shell(source: &str, branding: &BrandingConfig) -> String {
    let name = escape_html(&branding.name);
    let description = branding.tagline.as_deref().map_or_else(
        || {
            format!(
                "{} — source, track and sync the manga you read.",
                branding.name
            )
        },
        |tagline| format!("{} — {tagline}", branding.name),
    );
    let description = escape_html(&description);

    let mut out = replace_between(
        source,
        "<title>",
        "</title>",
        &format!("{name} — manga tracker"),
    )
    .unwrap_or_else(|| {
        tracing::warn!("the app shell has no <title>; its tab name is not this deployment's");
        source.to_owned()
    });
    out = replace_between(
        &out,
        r#"<meta name="description" content=""#,
        r#"" />"#,
        &description,
    )
    .unwrap_or_else(|| {
        tracing::warn!("the app shell has no description meta; it is not this deployment's");
        out.clone()
    });
    out
}

/// Replace the text between `open` and the next `close` after it, or `None` when the pair is
/// absent.
fn replace_between(source: &str, open: &str, close: &str, value: &str) -> Option<String> {
    let start = source.find(open)? + open.len();
    let end = start + source[start..].find(close)?;
    let mut out = String::with_capacity(source.len() + value.len());
    out.push_str(&source[..start]);
    out.push_str(value);
    out.push_str(&source[end..]);
    Some(out)
}

/// Escape a configured value on its way into markup.
///
/// The value is the operator's, not a reader's, so this is not an XSS boundary — but an
/// unescaped `&` or `"` in a perfectly ordinary product name would still break the attribute it
/// lands in, and a shell that fails to parse is a blank page.
fn escape_html(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(ch),
        }
    }
    out
}

/// Where the app shell lives inside the bundle.
pub(super) fn shell_path(static_dir: &str) -> PathBuf {
    Path::new(static_dir).join("index.html")
}

/// Read the app shell once, at startup.
///
/// One read, not one per request, and that is the whole point: the bytes the browser receives and
/// the bytes [`content_security_policy`] hashed are then the same generation *by construction*.
/// A per-build hash list and a per-response nonce both depend on that, and nothing else enforces
/// it.
///
/// Fail open, which is this service's decision to make and not the library's: an unreadable shell
/// costs the SPA, while `/v1/*`, the ops probes and every hashed asset keep working.
///
/// [`content_security_policy`]: super::csp::content_security_policy
pub(super) fn read_shell(path: &Path) -> Option<String> {
    match std::fs::read_to_string(path) {
        Ok(source) => Some(source),
        Err(error) => {
            tracing::error!(
                shell = %path.display(),
                %error,
                "could not read the app shell; the SPA cannot be served"
            );
            None
        }
    }
}
