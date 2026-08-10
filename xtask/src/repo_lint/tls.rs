//! The TLS bootstrap rule: every binary installs a rustls crypto provider before it runs.

use std::path::Path;

use super::Finding;

/// The call every `main` has to make, and the module that explains why.
const INSTALL_CALL: &str = "install_crypto_provider()";

/// Every service `main` must call `tankovault_service::install_crypto_provider()`.
///
/// Nothing else catches this. `rustls` only selects a provider by itself when exactly one of
/// its `ring` and `aws-lc-rs` features is enabled across the whole graph, and this workspace
/// enables both — so its fallback is a **panic**, thrown from inside `ServerConfig::builder()`
/// and friends the first time a TLS configuration is built. A service that omits the call
/// compiles, passes every other gate, boots far enough to bind its metrics port, and then dies
/// with a message that names only rustls. That is how it shipped once already.
///
/// The check is on `main.rs` specifically, not the crate's sources: installing the provider
/// somewhere deeper is exactly the ordering mistake this rule exists to prevent.
pub(super) fn every_service_installs_a_crypto_provider(
    root: &Path,
) -> anyhow::Result<Vec<Finding>> {
    let mut findings = Vec::new();
    for entry in std::fs::read_dir(root.join("services"))? {
        let dir = entry?.path();
        if !dir.is_dir() {
            continue;
        }
        let main = dir.join("src").join("main.rs");
        let Ok(source) = std::fs::read_to_string(&main) else {
            continue;
        };
        if !source.contains(INSTALL_CALL) {
            let name = dir.file_name().unwrap_or_default().to_string_lossy();
            findings.push(Finding {
                rule: "crypto-provider",
                file: main,
                line: 0,
                detail: format!(
                    "`{name}` never calls `{INSTALL_CALL}`: rustls cannot pick a provider in \
                     this dependency graph, so the first TLS configuration it builds panics \
                     the process at boot (see crates/service/src/crypto.rs)"
                ),
            });
        }
    }
    Ok(findings)
}
