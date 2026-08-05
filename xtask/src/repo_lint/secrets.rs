//! Rules over credentials: a default published in the compose file must be one the code
//! refuses to boot with.

use std::path::Path;

use super::Finding;
use super::text::{is_comment, rust_sources};

/// **A secret published in this repository must be refused by the code that reads it.** A
/// `${VAR:-value}` compose default is convenience for an ordinary setting but, for a
/// credential, a value anybody can read that an operator boots with unless they made
/// `deploy/local.env`. Every credential-shaped default must therefore appear literally in the
/// Rust refuse-lists (`services/api/src/main.rs::KNOWN_PLACEHOLDERS`,
/// `tankovault_service::internal_auth::KNOWN_PLACEHOLDERS`) — one half of the fix without the
/// other still boots with the published value.
pub(super) fn published_secrets_are_refused(root: &Path) -> anyhow::Result<Vec<Finding>> {
    let compose = root.join("deploy/docker-compose.yml");
    let Ok(yaml) = std::fs::read_to_string(&compose) else {
        anyhow::bail!("repo-lint: cannot read {}", compose.display());
    };

    let sources = rust_sources(root, &["services", "crates"]);
    let mut haystack = String::new();
    for path in &sources {
        if let Ok(text) = std::fs::read_to_string(path) {
            haystack.push_str(&text);
        }
    }

    let mut findings = Vec::new();
    for (number, line) in yaml.lines().enumerate() {
        if is_comment(line) {
            continue;
        }
        let Some((name, default)) = compose_default(line) else {
            continue;
        };
        if !is_credential(&name) || default.is_empty() {
            continue;
        }
        if !haystack.contains(&default) {
            findings.push(Finding {
                rule: "published-secrets-are-refused",
                file: compose.clone(),
                line: number + 1,
                detail: format!(
                    "`{name}` defaults to `{default}`, which is published here and refused \
                     nowhere. Make the variable required (`:?`) and add the string to the \
                     service's KNOWN_PLACEHOLDERS"
                ),
            });
        }
    }
    Ok(findings)
}

/// The variable name and literal default of a `${NAME:-default}` compose interpolation.
///
/// Returns `None` for `${NAME:?…}` (required, no default) and for lines with no interpolation,
/// which is most of them.
fn compose_default(line: &str) -> Option<(String, String)> {
    let start = line.find("${")?;
    let rest = &line[start + 2..];
    let end = rest.find('}')?;
    let inner = &rest[..end];
    let (name, default) = inner.split_once(":-")?;
    Some((name.trim().to_owned(), default.trim().to_owned()))
}

/// Whether a configuration key names a credential rather than an ordinary setting.
fn is_credential(name: &str) -> bool {
    const MARKERS: [&str; 5] = ["TOKEN", "SECRET", "PASSWORD", "PEPPER", "_KEY"];
    let upper = name.to_uppercase();
    MARKERS.iter().any(|marker| upper.contains(marker))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compose_defaults_are_read_but_required_variables_are_not() {
        assert_eq!(
            compose_default("  X: \"${TANKOVAULT_A__TOKEN:-dev-token}\""),
            Some(("TANKOVAULT_A__TOKEN".to_owned(), "dev-token".to_owned()))
        );
        // `:?` is the required form — there is no default to publish.
        assert_eq!(
            compose_default("  X: \"${TANKOVAULT_A__TOKEN:?set it}\""),
            None
        );
        // An empty default publishes nothing.
        assert_eq!(
            compose_default("  X: \"${TANKOVAULT_A__PEPPER:-}\""),
            Some(("TANKOVAULT_A__PEPPER".to_owned(), String::new()))
        );
        assert_eq!(compose_default("  X: \"literal\""), None);
    }

    #[test]
    fn credential_keys_are_told_apart_from_settings() {
        assert!(is_credential("TANKOVAULT_INTERNAL__TOKEN"));
        assert!(is_credential("TANKOVAULT_AUTH__JWT_SECRET"));
        assert!(is_credential("TANKOVAULT_AUTH__PASSWORD_PEPPER"));
        assert!(is_credential("TANKOVAULT_ANILIST__TOKEN_ENCRYPTION_KEY"));
        assert!(!is_credential("TANKOVAULT_TELEMETRY__JSON_LOGS"));
        assert!(!is_credential("TANKOVAULT_FRONTEND__STATIC_DIR"));
    }
}
