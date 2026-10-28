//! Owns the process-wide rustls [`CryptoProvider`](rustls::crypto::CryptoProvider).

use std::sync::Once;

static INSTALL: Once = Once::new();

/// Installs `ring` as the process-wide rustls crypto provider.
///
/// rustls 0.23 picks a provider on its own only when exactly one of its `ring` and `aws-lc-rs`
/// features is enabled. Cargo unifies features across the graph and this workspace enables both
/// (`lettre` asks for `ring`; `quinn-proto`, reached through `wreq`'s HTTP/3 support, asks for
/// `aws-lc-rs`), so the automatic path cannot decide and **panics** inside
/// `ServerConfig::builder()` instead of returning an error. That aborts the process at boot,
/// after the metrics listener is up, with a message that names rustls and nothing of ours.
///
/// The choice is therefore made here, once, before anything builds a rustls configuration: the
/// internal mTLS listener, `reqwest`, `sqlx`, `fred` and `lettre` all read the installed provider.
/// `ring` is what the rest of the stack already selects (`lettre` names it explicitly), and it
/// keeps one implementation on the handshake path.
///
/// Idempotent, and deliberately not an error: a provider installed first by something else is
/// still a provider, and refusing to boot over which one won would be worse than the ambiguity
/// this removes.
pub fn install_default_provider() {
    INSTALL.call_once(|| {
        if rustls::crypto::ring::default_provider()
            .install_default()
            .is_err()
        {
            tracing::debug!("a rustls crypto provider was already installed");
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Installing leaves the process with a default provider.
    ///
    /// The bug: with both provider features unified into `rustls`, every service that reached a
    /// TLS handshake panicked in `ServerConfig::builder()` because no provider was installed
    /// process-wide. `get_default()` returning `Some` is exactly the condition that panic checked.
    #[test]
    fn installing_leaves_the_process_with_a_provider() {
        install_default_provider();
        assert!(rustls::crypto::CryptoProvider::get_default().is_some());

        // A second call is a no-op, neither a panic nor a swap: services call this from `main`,
        // and the reload supervisor can re-enter the runtime that follows it.
        install_default_provider();
        assert!(rustls::crypto::CryptoProvider::get_default().is_some());
    }

    /// The server config builder returns an error instead of panicking.
    ///
    /// It is reached with no certificate material, so it fails on the arguments; reaching that
    /// error at all proves the provider lookup succeeded.
    #[test]
    fn a_server_config_builder_no_longer_panics() {
        install_default_provider();
        let roots = rustls::RootCertStore::empty();
        let verifier = rustls::server::WebPkiClientVerifier::builder(std::sync::Arc::new(roots))
            .build()
            .expect_err("an empty root store is refused, but only after the provider is read");
        assert_ne!(verifier.to_string(), "");
    }
}
