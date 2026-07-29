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
//! The address table and the pre-flight check are I/O-free and live in
//! [`tankovault_domain::ssrf`], so `services/render` and `services/challenge-solver` — which
//! also fetch caller-supplied URLs — can apply the same policy without depending on this
//! crate's `wreq`/BoringSSL stack. They are re-exported here so existing call sites are
//! unchanged.

use std::net::SocketAddr;
use wreq::dns::{Addrs, Name, Resolve, Resolving};

pub use tankovault_domain::ssrf::{SsrfError, is_forbidden_ip, validate_str, validate_url};

/// Resolve `host` and return only the vetted (public) addresses, erroring if none remain.
///
/// # Errors
/// [`SsrfError::Resolution`] on DNS failure, [`SsrfError::ForbiddenAddress`] if every
/// resolved address is internal.
pub async fn resolve_checked(host: &str) -> Result<Vec<SocketAddr>, SsrfError> {
    let addrs = tokio::net::lookup_host((host, 0))
        .await
        .map_err(|_| SsrfError::Resolution(host.to_owned()))?;
    let allowed: Vec<SocketAddr> = addrs.filter(|sa| !is_forbidden_ip(sa.ip())).collect();
    if allowed.is_empty() {
        return Err(SsrfError::ForbiddenAddress(host.to_owned()));
    }
    Ok(allowed)
}

/// Validate a URL the way an *inbound* handler must: scheme and address-range pre-flight,
/// then a real DNS resolution whose every answer is re-checked.
///
/// Used where a URL is about to be **stored** or handed to something this process does not
/// control — `admin/providers.rs`'s `base_url`, which the scan workers then hit on a timer.
/// For an outbound fetch through this crate's own stack the pre-flight alone is enough,
/// because [`SsrfResolver`] repeats the address check at connect time and on every redirect.
///
/// # Errors
/// As [`validate_url`], plus [`SsrfError::Resolution`] / [`SsrfError::ForbiddenAddress`].
pub async fn validate_and_resolve(raw: &str) -> Result<(), SsrfError> {
    let url = validate_str(raw)?;
    let Some(host) = url.host() else {
        return Err(SsrfError::NoHost);
    };
    // An IP literal was already range-checked by `validate_str`; there is nothing to resolve.
    let url::Host::Domain(domain) = host else {
        return Ok(());
    };
    resolve_checked(domain).await.map(|_| ())
}

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
