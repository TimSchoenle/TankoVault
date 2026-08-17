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

/// Order a resolved address list IPv6-first, preserving the resolver's order within each family.
///
/// The connector attempts the list in order and falls back to the next entry, so this is a
/// preference and not a requirement: an IPv6-less host still reaches every dual-stack origin over
/// IPv4. It exists because the preference is load-bearing rather than cosmetic. A container on
/// Docker's NAT66 gets a *unique-local* source address, and RFC 6724's scope-match rule then ranks
/// a global IPv4 destination above a global IPv6 one — so glibc and musl both hand back IPv4
/// first, we connect to it, and that is the only address we ever use. Four of the nine Keyoapp
/// origins answer an IPv4 client with a bare nginx `404` on **every** route while serving the same
/// request normally over IPv6, which is not a status any bot-management path escalates: it reads
/// as "the site removed that page", so every scan of those four failed 100% with nothing to
/// escalate. Browsers do not see this, because they prefer IPv6 and fall back — which is exactly
/// what this restores.
fn prefer_ipv6(addrs: &mut [SocketAddr]) {
    addrs.sort_by_key(|sa| u8::from(sa.is_ipv4()));
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

    /// Pins the IPv6-first ordering. Resolver order put IPv4 first for every dual-stack origin,
    /// so the crawler only ever used IPv4 — and four Keyoapp origins answer an IPv4 client with a
    /// bare nginx `404` on every route while serving IPv6 normally, which failed those providers
    /// completely with an error no bot-management path escalates.
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
                // Order within a family is the resolver's, which is how a rotating DNS answer
                // still spreads load across an origin's addresses.
                addr("188.114.96.3:0"),
                addr("188.114.97.3:0"),
            ]
        );
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
