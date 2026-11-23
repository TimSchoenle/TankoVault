//! Owns the fuzz target that exercises `tankovault_domain::ssrf::validate_str`, the address check
//! every provider and webhook URL passes before a request.
//!
//! # Oracle
//! 1. No panic.
//! 2. An accepted URL is `http`/`https`, and an IP-literal host it carries is never one
//!    `is_forbidden_ip` refuses: the parse and the check must agree on what the host is.

#![no_main]

use libfuzzer_sys::fuzz_target;
use tankovault_domain::ssrf::{is_forbidden_ip, validate_str};
use url::Host;

fuzz_target!(|data: &str| {
    let Ok(url) = validate_str(data) else {
        return;
    };
    assert!(
        matches!(url.scheme(), "http" | "https"),
        "accepted scheme {:?}",
        url.scheme()
    );
    let ip = match url.host() {
        Some(Host::Ipv4(v4)) => Some(v4.into()),
        Some(Host::Ipv6(v6)) => Some(v6.into()),
        _ => None,
    };
    if let Some(ip) = ip {
        assert!(!is_forbidden_ip(ip), "accepted forbidden address {ip}");
    }
});
