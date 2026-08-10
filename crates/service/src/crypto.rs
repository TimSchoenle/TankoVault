//! The process-wide rustls [`CryptoProvider`](rustls::crypto::CryptoProvider).

use std::sync::Once;

static INSTALL: Once = Once::new();

/// Install `ring` as the process-wide rustls crypto provider.
///
/// rustls 0.23 only picks a provider on its own when *exactly one* of its `ring` and
/// `aws-lc-rs` features is enabled. Cargo unifies features across the whole graph, and this
/// workspace ends up with both — `lettre` asks for `ring`, `quinn-proto` (reached through
/// `wreq`'s HTTP/3 support) asks for `aws-lc-rs` — so the automatic path cannot decide and
/// **panics** inside `ServerConfig::builder()` rather than returning an error. That aborts the
/// process at boot, after the metrics listener is already up, with a message that names rustls
/// and nothing of ours.
///
/// So the choice is made here instead, once, before anything can build a rustls configuration:
/// the internal mTLS listener, `reqwest`, `sqlx`, `fred` and `lettre` all read the same
/// installed provider. `ring` because it is what the rest of the stack already selected —
/// `lettre` names it explicitly — and it keeps one implementation on the handshake path rather
/// than two.
///
/// Idempotent, and deliberately not an error: a provider installed first by something else is
/// still a provider, and refusing to boot over which one won would be worse than the ambiguity
/// this exists to remove.
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

    /// Pins the boot crash: with both provider features unified into `rustls`, every service
    /// that reached a TLS handshake panicked in `ServerConfig::builder()` because no provider
    /// was installed process-wide. `get_default()` returning `Some` is exactly the condition
    /// that panic checked.
    #[test]
    fn installing_leaves_the_process_with_a_provider() {
        install_default_provider();
        assert!(rustls::crypto::CryptoProvider::get_default().is_some());

        // Second call is a no-op rather than a panic or a swap: services call this from `main`,
        // and the reload supervisor can re-enter the runtime that follows it.
        install_default_provider();
        assert!(rustls::crypto::CryptoProvider::get_default().is_some());
    }

    /// The builder that panicked. It is reached here with no certificate material, so it fails
    /// on the *arguments* — reaching that error at all proves the provider lookup succeeded.
    #[test]
    fn a_server_config_builder_no_longer_panics() {
        install_default_provider();
        let roots = rustls::RootCertStore::empty();
        let verifier = rustls::server::WebPkiClientVerifier::builder(std::sync::Arc::new(roots))
            .build()
            .expect_err("an empty root store is refused, but only after the provider is read");
        assert!(!verifier.to_string().is_empty());
    }
}
