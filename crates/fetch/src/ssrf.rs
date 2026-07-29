//! SSRF guard — critical for this system (design §16).
//!
//! Workers and the "test adapter" endpoint fetch operator-supplied URLs. The guard:
//! - allows only `http`/`https`,
//! - resolves the host and **rejects private, loopback, link-local, CGNAT, benchmarking,
//!   documentation, and cloud-metadata IP ranges** (`169.254.169.254`, RFC1918, `::1`, …),
//! - re-checks **after DNS resolution and on every redirect** by injecting a validating
//!   [`wreq::dns::Resolve`] so the client only ever connects to vetted public IPs —
//!   closing the DNS-rebinding / redirect-to-internal hole.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use thiserror::Error;
use url::Url;
use wreq::dns::{Addrs, Name, Resolve, Resolving};

/// Reasons a URL/host is rejected by the guard.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum SsrfError {
    /// Scheme was not `http`/`https`.
    #[error("disallowed URL scheme: {0}")]
    Scheme(String),
    /// The URL had no host component.
    #[error("URL has no host")]
    NoHost,
    /// The host resolved only to forbidden (internal) addresses.
    #[error("host {0} resolves to a forbidden address range")]
    ForbiddenAddress(String),
    /// DNS resolution failed.
    #[error("DNS resolution failed for {0}")]
    Resolution(String),
}

/// True if `ip` is in any range we must never connect to from a crawler.
#[must_use]
pub fn is_forbidden_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_forbidden_v4(v4),
        IpAddr::V6(v6) => is_forbidden_v6(v6),
    }
}

fn is_forbidden_v4(ip: Ipv4Addr) -> bool {
    let o = ip.octets();
    ip.is_unspecified()                       // 0.0.0.0
        || ip.is_loopback()                   // 127.0.0.0/8
        || ip.is_private()                    // 10/8, 172.16/12, 192.168/16
        || ip.is_link_local()                 // 169.254.0.0/16 (incl. metadata .169.254)
        || ip.is_broadcast()                  // 255.255.255.255
        || ip.is_multicast()                  // 224.0.0.0/4
        || o[0] == 100 && (o[1] & 0xc0) == 64 // 100.64.0.0/10 CGNAT
        || o[0] == 192 && o[1] == 0 && o[2] == 0 // 192.0.0.0/24 IETF protocol
        || o[0] == 192 && o[1] == 0 && o[2] == 2 // 192.0.2.0/24 TEST-NET-1
        || o[0] == 198 && o[1] == 51 && o[2] == 100 // 198.51.100.0/24 TEST-NET-2
        || o[0] == 203 && o[1] == 0 && o[2] == 113   // 203.0.113.0/24 TEST-NET-3
        || o[0] == 198 && (o[1] & 0xfe) == 18 // 198.18.0.0/15 benchmarking
        || o[0] >= 240 // 240.0.0.0/4 reserved
}

fn is_forbidden_v6(ip: Ipv6Addr) -> bool {
    // Unwrap IPv4-mapped/compatible addresses and apply the v4 rules to the embedded v4.
    if let Some(v4) = ip.to_ipv4_mapped() {
        return is_forbidden_v4(v4);
    }
    if let Some(v4) = ip.to_ipv4() {
        // v4-compatible (deprecated) — treat like the embedded v4 unless it's a pure v6 addr.
        if !ip.is_loopback() && !ip.is_unspecified() {
            return is_forbidden_v4(v4);
        }
    }
    let seg = ip.segments();
    ip.is_unspecified()                 // ::
        || ip.is_loopback()             // ::1
        || ip.is_multicast()            // ff00::/8
        || (seg[0] & 0xfe00) == 0xfc00  // fc00::/7 unique local
        || (seg[0] & 0xffc0) == 0xfe80  // fe80::/10 link-local
        || (seg[0] == 0x2001 && seg[1] == 0x0db8) // 2001:db8::/32 documentation
}

/// Validate the scheme, the presence of a host, and — when the authority is an IP literal
/// — the address range.
///
/// The literal check is load-bearing, not belt-and-braces: `hyper-util`'s `HttpConnector`
/// short-circuits DNS whenever the authority parses as an IP, so [`SsrfResolver`] is never
/// consulted for `http://127.0.0.1/` or `http://169.254.169.254/`. Hostnames are still
/// checked at connect time by the resolver, which additionally covers DNS rebinding.
///
/// # Errors
/// [`SsrfError::Scheme`] / [`SsrfError::NoHost`] / [`SsrfError::ForbiddenAddress`].
pub fn validate_url(url: &Url) -> Result<(), SsrfError> {
    if !matches!(url.scheme(), "http" | "https") {
        return Err(SsrfError::Scheme(url.scheme().to_owned()));
    }
    match url.host() {
        None => Err(SsrfError::NoHost),
        Some(url::Host::Ipv4(ip)) if is_forbidden_ip(IpAddr::V4(ip)) => {
            Err(SsrfError::ForbiddenAddress(ip.to_string()))
        }
        Some(url::Host::Ipv6(ip)) if is_forbidden_ip(IpAddr::V6(ip)) => {
            Err(SsrfError::ForbiddenAddress(ip.to_string()))
        }
        Some(_) => Ok(()),
    }
}

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
    use std::str::FromStr;

    fn forbidden(s: &str) -> bool {
        is_forbidden_ip(IpAddr::from_str(s).unwrap())
    }

    #[test]
    fn blocks_cloud_metadata_endpoint() {
        assert!(forbidden("169.254.169.254"));
    }

    #[test]
    fn blocks_rfc1918_ranges() {
        assert!(forbidden("10.0.0.1"));
        assert!(forbidden("172.16.5.4"));
        assert!(forbidden("192.168.1.1"));
    }

    #[test]
    fn blocks_loopback_and_unspecified() {
        assert!(forbidden("127.0.0.1"));
        assert!(forbidden("0.0.0.0"));
        assert!(forbidden("::1"));
        assert!(forbidden("::"));
    }

    #[test]
    fn blocks_cgnat_and_benchmarking() {
        assert!(forbidden("100.64.0.1"));
        assert!(forbidden("198.18.0.1"));
    }

    #[test]
    fn blocks_ipv6_unique_local_and_link_local() {
        assert!(forbidden("fc00::1"));
        assert!(forbidden("fd12:3456::1"));
        assert!(forbidden("fe80::1"));
    }

    #[test]
    fn blocks_ipv4_mapped_internal_v6() {
        // ::ffff:169.254.169.254 must be caught via the embedded v4.
        assert!(forbidden("::ffff:169.254.169.254"));
        assert!(forbidden("::ffff:10.0.0.1"));
    }

    #[test]
    fn allows_public_addresses() {
        assert!(!forbidden("1.1.1.1"));
        assert!(!forbidden("8.8.8.8"));
        assert!(!forbidden("93.184.216.34")); // example.com
        assert!(!forbidden("2606:4700:4700::1111")); // public v6
    }

    #[test]
    fn validate_url_rejects_non_web_schemes() {
        let u = Url::parse("file:///etc/passwd").unwrap();
        assert!(matches!(validate_url(&u), Err(SsrfError::Scheme(_))));
        let g = Url::parse("gopher://host/x").unwrap();
        assert!(matches!(validate_url(&g), Err(SsrfError::Scheme(_))));
    }

    #[test]
    fn validate_url_accepts_http_https() {
        assert!(validate_url(&Url::parse("https://example.com/x").unwrap()).is_ok());
        assert!(validate_url(&Url::parse("http://example.com/x").unwrap()).is_ok());
    }

    /// Regression: the connector skips the custom resolver for IP-literal authorities, so
    /// the range check has to happen here or not at all.
    #[test]
    fn validate_url_rejects_ip_literals_in_forbidden_ranges() {
        for raw in [
            "http://169.254.169.254/latest/meta-data/",
            "http://127.0.0.1/",
            "http://10.0.0.1/",
            "https://192.168.1.1/",
            "http://[::1]:5432/",
            "http://[fd00::1]/",
            "http://[::ffff:169.254.169.254]/",
        ] {
            let url = Url::parse(raw).unwrap();
            assert!(
                matches!(validate_url(&url), Err(SsrfError::ForbiddenAddress(_))),
                "{raw} should be rejected"
            );
        }
    }

    #[test]
    fn validate_url_accepts_public_ip_literals() {
        assert!(validate_url(&Url::parse("http://1.1.1.1/").unwrap()).is_ok());
        assert!(validate_url(&Url::parse("http://[2606:4700:4700::1111]/").unwrap()).is_ok());
    }
}
