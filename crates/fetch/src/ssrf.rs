//! SSRF guard — critical for this system (design §16).
//!
//! Workers and the "test adapter" endpoint fetch operator-supplied URLs. The guard:
//! - allows only `http`/`https`,
//! - rejects private, loopback, link-local, CGNAT, benchmarking, documentation and
//!   cloud-metadata IP ranges (`169.254.169.254`, RFC1918, `::1`, …), including when the
//!   authority is an IP *literal*, which the connector never resolves,
//! - re-checks **after DNS resolution and on every redirect** by injecting a validating
//!   [`wreq::dns::Resolve`] so the client only ever connects to vetted public IPs —
//!   closing the DNS-rebinding / redirect-to-internal hole.
//!
//! **The policy itself is not here** — it lives in [`tankovault_domain::ssrf`] and is
//! re-exported, so a service needing the same checks does not have to link this crate's
//! `wreq`/`BoringSSL` stack. What belongs here is [`SsrfResolver`]: a `wreq::dns::Resolve`,
//! which can only exist where `wreq` does.

use std::net::SocketAddr;
use wreq::dns::{Addrs, Name, Resolve, Resolving};

pub use tankovault_domain::ssrf::{
    SsrfError, is_forbidden_ip, resolve_checked, validate_and_resolve, validate_str, validate_url,
};

/// A [`wreq::dns::Resolve`] that filters out forbidden addresses at connect time, for
/// the initial request and every redirect hop. Injected into the base client so no code
/// path can connect to an internal address, even under DNS rebinding.
#[derive(Debug, Default, Clone)]
pub struct SsrfResolver;

impl Resolve for SsrfResolver {
    fn resolve(&self, name: Name) -> Resolving {
        Box::pin(async move {
            let host = name.as_str().to_owned();
            let addrs = tokio::net::lookup_host((host.as_str(), 0)).await?;
            let allowed: Vec<SocketAddr> = addrs.filter(|sa| !is_forbidden_ip(sa.ip())).collect();
            if allowed.is_empty() {
                let err: Box<dyn std::error::Error + Send + Sync> =
                    Box::new(SsrfError::ForbiddenAddress(host));
                return Err(err);
            }
            let iter: Addrs = Box::new(allowed.into_iter());
            Ok(iter)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The policy itself is tested in `tankovault_domain::ssrf`; this only pins that the
    /// re-export still resolves, so moving it cannot silently drop the guard from this crate.
    #[test]
    fn the_policy_is_reachable_through_this_module() {
        assert!(validate_str("http://169.254.169.254/").is_err());
        assert!(validate_str("https://example.com/").is_ok());
    }

    /// An IP literal must not attempt DNS at all: the range check already answered.
    #[tokio::test]
    async fn validate_and_resolve_short_circuits_on_literals() {
        assert!(
            validate_and_resolve("http://169.254.169.254/")
                .await
                .is_err()
        );
        assert!(validate_and_resolve("http://1.1.1.1/").await.is_ok());
        assert!(validate_and_resolve("file:///etc/passwd").await.is_err());
    }
}
