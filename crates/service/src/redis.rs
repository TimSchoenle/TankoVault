//! The one way a service builds its Redis client.
//!
//! fred's defaults are no reconnect policy and no command timeout. Together they meant a Redis
//! restart left every client disconnected for the life of the process — shared rate limiting
//! silently fell open, stream tickets failed, and control-plane leadership was never held again
//! — while a Redis that stopped answering without closing the socket hung each caller forever.

use std::time::Duration;

use fred::prelude::{Builder, Client, Config, ReconnectPolicy};

/// Ceiling on any one command; a rate-limit check is on every request's path.
pub const COMMAND_TIMEOUT: Duration = Duration::from_millis(250);

/// Ceiling on opening a connection, at boot and on every reconnect.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// Build (not connect) a client that reconnects for ever with capped exponential backoff and
/// bounds every command.
///
/// # Errors
/// Whatever fred reports for an unusable configuration.
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
