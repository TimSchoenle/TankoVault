//! Owns the SSRF guard (`docs/ARCHITECTURE.md` §16).
//!
//! Workers and the "test adapter" endpoint fetch operator-supplied URLs. The guard:
//! - allows only `http` and `https`,
//! - rejects private, loopback, link-local, CGNAT, benchmarking, documentation and
//!   cloud-metadata ranges (`169.254.169.254`, RFC 1918, `::1`, …), including an IP literal
//!   authority, which the connector never resolves,
//! - re-checks **after DNS resolution and on every redirect** through a validating
//!   [`wreq::dns::Resolve`], so the client connects only to vetted public IPs and DNS
//!   rebinding or a redirect to an internal host cannot get through.
//!
//! **The policy lives in [`tankovault_domain::ssrf`]** and is re-exported, so a service needing
//! the same checks need not link `wreq` and `BoringSSL`. Only [`SsrfResolver`] belongs here,
//! because a `wreq::dns::Resolve` can only exist where `wreq` does.

use std::net::SocketAddr;
use wreq::dns::{Addrs, Name, Resolve, Resolving};

pub use tankovault_domain::ssrf::{
    SsrfError, is_forbidden_ip, resolve_checked, validate_and_resolve, validate_str, validate_url,
};

/// Filters forbidden addresses out of DNS answers at connect time, on the first request and
/// every redirect hop.
///
/// Injected into the base client, so no code path can connect to an internal address, even
/// under DNS rebinding.
#[derive(Debug, Default, Clone)]
pub struct SsrfResolver;

impl Resolve for SsrfResolver {
    fn resolve(&self, name: Name) -> Resolving {
        Box::pin(async move {
            let host = name.as_str().to_owned();
            let addrs = tokio::net::lookup_host((host.as_str(), 0)).await?;
            let mut allowed: Vec<SocketAddr> =
                addrs.filter(|sa| !is_forbidden_ip(sa.ip())).collect();
            if allowed.is_empty() {
                let err: Box<dyn std::error::Error + Send + Sync> =
                    Box::new(SsrfError::ForbiddenAddress(host));
                return Err(err);
            }
            prefer_ipv6(&mut allowed);
            let iter: Addrs = Box::new(allowed.into_iter());
            Ok(iter)
        })
    }
}

/// Orders a resolved address list IPv6-first, keeping the resolver's order within each family.
///
/// The connector tries the list in order and falls back, so this is a preference: an IPv6-less
/// host still reaches every dual-stack origin over IPv4. The preference matters. On Docker's
/// NAT66 a container has a unique-local source address, so RFC 6724 ranks a global IPv4
/// destination first, and glibc and musl both return IPv4 first. Some origins answer an IPv4
/// client with a bare nginx `404` on **every** route while serving IPv6 normally, which reads as
/// "the page was removed". Browsers prefer IPv6 and fall back, and this does the same.
fn prefer_ipv6(addrs: &mut [SocketAddr]) {
    addrs.sort_by_key(|sa| u8::from(sa.is_ipv4()));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The re-exported policy resolves, so moving it cannot silently drop the guard from this
    /// crate; the policy itself is tested in `tankovault_domain::ssrf`.
    #[test]
    fn the_policy_is_reachable_through_this_module() {
        assert!(validate_str("http://169.254.169.254/").is_err());
        assert!(validate_str("https://example.com/").is_ok());
    }

    /// Resolved addresses are ordered IPv6-first.
    ///
    /// The bug: resolver order puts IPv4 first for every dual-stack origin, and origins that
    /// answer IPv4 with a bare nginx `404` on every route then fail completely.
    #[test]
    fn resolved_addresses_are_ordered_ipv6_first() {
        let addr = |s: &str| s.parse::<SocketAddr>().expect("valid socket address");
        let mut addrs = vec![
            addr("188.114.96.3:0"),
            addr("188.114.97.3:0"),
            addr("[2a06:98c1:3120::3]:0"),
        ];
        prefer_ipv6(&mut addrs);
        assert_eq!(
            addrs,
            vec![
                addr("[2a06:98c1:3120::3]:0"),
                // Order within a family stays the resolver's, so a rotating DNS answer still
                // spreads load across an origin's addresses.
                addr("188.114.96.3:0"),
                addr("188.114.97.3:0"),
            ]
        );
    }

    /// An IP literal skips DNS: the range check already answered.
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
