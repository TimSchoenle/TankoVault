//! The rustls crypto provider is installed by the code that needs it, not by convention.
//!
//! Its own test binary on purpose: the assertion is about a *process-wide* install, so it can
//! only observe the absence it starts from if nothing else in the binary has run first. Keep
//! this file to one test for the same reason — `cargo test` runs a binary's tests in parallel
//! threads of one process.

use std::path::PathBuf;

use tankovault_config::ResolvedTls;
use tankovault_service::ReloadingTls;

/// Pins the boot crash of 2026-08-10: with mutual TLS switched on, every service panicked at
/// startup — after the metrics listener was already up — inside `ServerConfig::builder()`.
/// `rustls` picks a provider by itself only when exactly one of its `ring` and `aws-lc-rs`
/// features is enabled, and this graph enables both (lettre pulls `ring`, quinn-proto pulls
/// `aws-lc-rs` through `wreq`), so it panicked instead of choosing.
///
/// The paths below do not exist, so this stops at the file read: reaching a named
/// [`tankovault_service::TlsError`] *and* leaving a provider behind is the whole claim. If the
/// install moves back out to the callers, the second assertion fails here rather than in a
/// container.
#[test]
fn loading_tls_material_installs_the_provider_it_needs() {
    assert!(
        rustls::crypto::CryptoProvider::get_default().is_none(),
        "nothing may install a provider before the code under test does, or this proves nothing"
    );

    let err = ReloadingTls::load(&ResolvedTls {
        cert: PathBuf::from("/nonexistent/tls.crt"),
        key: PathBuf::from("/nonexistent/tls.key"),
        ca: PathBuf::from("/nonexistent/ca.crt"),
    })
    .expect_err("nothing is readable there");
    assert!(err.to_string().contains("tls.crt"), "{err}");

    assert!(
        rustls::crypto::CryptoProvider::get_default().is_some(),
        "the listener would panic on the next handshake setup instead of returning an error"
    );
}
