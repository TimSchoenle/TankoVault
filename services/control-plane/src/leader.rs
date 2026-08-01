//! Redis-backed leader election for the singleton scheduler: it must run on exactly one
//! control-plane replica, expressed as a best-effort distributed lock over Redis. Fails
//! open to sole-leader when unconfigured, but drops leadership on a Redis error while
//! leading, so partitioned replicas cannot both believe they lead.

use fred::prelude::*;
use secrecy::{ExposeSecret as _, SecretString};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use uuid::Uuid;

/// Redis key holding the current scheduler leader's token.
const LOCK_KEY: &str = "tankovault:control-plane:scheduler-leader";
/// How long a held lease survives without renewal, in milliseconds.
const LOCK_TTL_MS: i64 = 30_000;
/// How often the background task re-acquires or renews the lease (TTL / 3).
const RENEW_INTERVAL: Duration = Duration::from_secs(10);

/// A cheap, cloneable handle onto the current leadership status.
#[derive(Clone)]
pub(crate) struct Leadership {
    held: Arc<AtomicBool>,
}

impl Leadership {
    /// Whether this replica currently holds scheduler leadership.
    pub(crate) fn is_leader(&self) -> bool {
        self.held.load(Ordering::Relaxed)
    }
}

/// Connect (and wait for readiness) a Redis client for leader election.
///
/// `url` is a [`SecretString`] because `redis://:password@host` is a supported form; it is
/// exposed exactly here, into the client config.
pub(crate) async fn connect(url: &SecretString) -> anyhow::Result<Client> {
    let config = Config::from_url(url.expose_secret())?;
    let client = Builder::from_config(config).build()?;
    client.init().await?;
    Ok(client)
}

/// Start leader election.
///
/// With `Some(client)` a background task continuously acquires/renews the lock and
/// publishes the result into the returned [`Leadership`]. With `None` (no Redis) the
/// replica is permanently the leader.
pub(crate) fn spawn(client: Option<Client>) -> Leadership {
    let held = Arc::new(AtomicBool::new(client.is_none()));
    if let Some(client) = client {
        let token = Uuid::now_v7().to_string();
        let held = held.clone();
        tokio::spawn(async move {
            loop {
                let leader = match acquire_or_renew(&client, &token).await {
                    Ok(v) => v,
                    Err(e) => {
                        tracing::warn!(error = %e, "leader election: redis error; standing down");
                        false
                    }
                };
                let previous = held.swap(leader, Ordering::Relaxed);
                match (previous, leader) {
                    (false, true) => tracing::info!("acquired scheduler leadership"),
                    (true, false) => tracing::warn!("lost scheduler leadership"),
                    _ => {}
                }
                tokio::time::sleep(RENEW_INTERVAL).await;
            }
        });
    } else {
        tracing::info!("no redis configured; scheduler runs unguarded (single leader)");
    }
    Leadership { held }
}

/// Try to acquire the free lock, or renew it if we already hold it. Returns whether this
/// replica holds leadership after the attempt.
async fn acquire_or_renew(client: &Client, token: &str) -> Result<bool, Error> {
    let ttl_ms = LOCK_TTL_MS;
    let acquired: Option<String> = client
        .set(
            LOCK_KEY,
            token,
            Some(Expiration::PX(ttl_ms)),
            Some(SetOptions::NX),
            false,
        )
        .await?;
    if acquired.is_some() {
        return Ok(true);
    }
    // Extend only if the lease is still ours. GET-then-PEXPIRE isn't atomic, but TTL
    // comfortably exceeds the renewal interval, so the worst case is a one-cycle-late
    // hand-off — and scan planning is idempotent regardless.
    let current: Option<String> = client.get(LOCK_KEY).await?;
    if current.as_deref() == Some(token) {
        let _: i64 = client.pexpire(LOCK_KEY, ttl_ms, None).await?;
        return Ok(true);
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redisless_replica_is_always_leader() {
        // With no Redis client, the process is the sole leader from the outset.
        let leadership = spawn(None);
        assert!(leadership.is_leader());
    }

    #[test]
    fn renewal_interval_is_well_below_the_lease_ttl() {
        // The lease must outlive several renewal cycles so a transient Redis blip does
        // not flap leadership between replicas.
        assert!(RENEW_INTERVAL.as_millis() * 2 < u128::try_from(LOCK_TTL_MS).unwrap());
    }
}
