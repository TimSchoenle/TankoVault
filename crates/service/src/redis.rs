//! Owns the one way a service builds its Redis client.
//!
//! fred defaults to no reconnect policy and no command timeout. With those, a Redis restart
//! leaves every client disconnected for the life of the process (shared rate limiting silently
//! fails open, stream tickets fail, control-plane leadership is never held again), and a Redis
//! that stops answering without closing the socket hangs each caller forever.

use std::time::Duration;

use fred::prelude::{Builder, Client, Config, ReconnectPolicy};

/// Ceiling on any one command; a rate-limit check is on every request's path.
pub const COMMAND_TIMEOUT: Duration = Duration::from_millis(250);

/// Ceiling on opening a connection, at boot and on every reconnect.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// Builds, without connecting, a client that reconnects forever with capped exponential backoff
/// and bounds every command.
///
/// # Errors
/// Returns fred's error for an unusable configuration.
pub fn client(config: Config) -> Result<Client, fred::error::Error> {
    let mut builder = Builder::from_config(config);
    builder
        // 0 attempts = unlimited; 100 ms doubling to a 10 s ceiling.
        .set_policy(ReconnectPolicy::new_exponential(0, 100, 10_000, 2))
        .with_performance_config(|perf| perf.default_command_timeout = COMMAND_TIMEOUT)
        .with_connection_config(|conn| {
            conn.connection_timeout = CONNECT_TIMEOUT;
            conn.internal_command_timeout = CONNECT_TIMEOUT;
        });
    builder.build()
}
