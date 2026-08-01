//! Container healthcheck, as an argv branch in the service binary itself.
//!
//! Backend services ship on `scratch` with no shell, so the binary probes itself instead of
//! a `CMD-SHELL` healthcheck: a plain TCP connect to the ops listener, since "the listener
//! accepts" is what liveness should mean (readiness is answered separately over HTTP).

use std::net::{SocketAddr, ToSocketAddrs as _};
use std::time::Duration;

/// The argv flag a service checks for before doing anything else.
pub const HEALTHCHECK_FLAG: &str = "--healthcheck";

/// How long the probe waits for the connection. Well under Docker's own `timeout`, so the
/// process exits with a verdict rather than being killed without one.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);

/// Whether this process was invoked as a healthcheck rather than as the service.
///
/// Call at the very top of `main`, before config loading or telemetry: the probe must be
/// cheap and independent of whether the service's own configuration still parses.
#[must_use]
pub fn requested() -> bool {
    std::env::args().any(|arg| arg == HEALTHCHECK_FLAG)
}

/// Probe `addr` and exit the process: `0` if the listener accepted, `1` otherwise.
///
/// Never returns — a healthcheck's only output is its exit status.
pub fn run_and_exit(addr: &str) -> ! {
    let code = i32::from(!probe(addr));
    std::process::exit(code);
}

/// Connect to `addr`, returning whether the listener accepted.
fn probe(addr: &str) -> bool {
    // `0.0.0.0` is a bind address, not a destination: connecting to it is only meaningful by
    // accident. Rewrite to loopback, which is what "probe myself" means.
    let target = match addr.parse::<SocketAddr>() {
        Ok(sa) if sa.ip().is_unspecified() => {
            format!("127.0.0.1:{}", sa.port())
        }
        Ok(sa) => sa.to_string(),
        // Not a socket address (a hostname form, say). Hand it to the resolver as-is.
        Err(_) => addr.to_owned(),
    };

    let Ok(resolved) = target.to_socket_addrs() else {
        return false;
    };
    for candidate in resolved {
        if std::net::TcpStream::connect_timeout(&candidate, CONNECT_TIMEOUT).is_ok() {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    #[test]
    fn an_accepting_listener_is_healthy() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind an ephemeral port");
        let addr = listener.local_addr().expect("has an address").to_string();
        assert!(probe(&addr));
    }

    #[test]
    fn a_closed_port_is_not_healthy() {
        // Bind then drop, so the port is known to have been free and is now unbound.
        let addr = {
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind an ephemeral port");
            listener.local_addr().expect("has an address").to_string()
        };
        assert!(!probe(&addr));
    }

    /// Services bind `0.0.0.0`, which is not a destination. Probing it verbatim is either a
    /// platform-specific accident or a failure; rewriting to loopback is the intent.
    #[test]
    fn an_unspecified_bind_address_is_probed_on_loopback() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind an ephemeral port");
        let port = listener.local_addr().expect("has an address").port();
        assert!(probe(&format!("0.0.0.0:{port}")));
    }

    #[test]
    fn a_garbage_address_is_not_healthy_rather_than_panicking() {
        assert!(!probe("not a socket address at all"));
    }
}
