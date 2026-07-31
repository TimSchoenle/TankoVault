//! Container healthcheck, as an argv branch in the service binary itself.
//!
//! Every backend service ships on `scratch`: no shell, no `wget`, no `curl`. Docker's
//! `HEALTHCHECK` needs *something* executable inside the image, so a `CMD-SHELL` probe is
//! simply not available — which is why the eight app services had no healthcheck at all, and
//! why `depends_on` could only ever say `service_started` rather than `service_healthy`.
//!
//! The one executable that is always in the image is the service binary. So it probes itself:
//!
//! ```yaml
//! healthcheck:
//!   test: ["CMD", "/app/api", "--healthcheck"]
//!   interval: 10s
//!   timeout: 3s
//!   retries: 5
//!   start_period: 20s
//! ```
//!
//! The probe is a plain TCP connect to the service's own ops listener, not an HTTP request.
//! That is deliberate: it needs no HTTP client in a binary that might not otherwise have one
//! (`challenge-solver` has no `reqwest`), it cannot be confused by a `503` from `/ready`, and
//! "the listener is accepting connections" is exactly what a *liveness* check should mean.
//! Readiness — whether Postgres and NATS are reachable — is a different question with a
//! different consumer (an orchestrator's traffic routing), and `/ready` already answers it
//! over HTTP for anything that can speak HTTP.

use std::net::{SocketAddr, ToSocketAddrs as _};
use std::time::Duration;

/// The argv flag a service checks for before doing anything else.
pub const HEALTHCHECK_FLAG: &str = "--healthcheck";

/// How long the probe waits for the connection. Well under Docker's own `timeout`, so the
/// process exits with a verdict rather than being killed without one.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);

/// Whether this process was invoked as a healthcheck rather than as the service.
///
/// Call at the very top of `main`, before config loading or telemetry: the probe runs on a
/// schedule inside a live container and must be cheap, silent, and independent of whether the
/// service's own configuration still parses.
#[must_use]
pub fn requested() -> bool {
    std::env::args().any(|arg| arg == HEALTHCHECK_FLAG)
}

/// Probe `addr` and exit the process: `0` if the listener accepted, `1` otherwise.
///
/// Never returns — a healthcheck's only output is its exit status, and returning would let a
/// caller accidentally continue into the service's boot sequence.
///
/// `addr` is the service's own bind address, so the probe follows a rebind without anyone
/// having to remember to update the compose file.
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
