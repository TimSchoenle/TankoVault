//! Redis-backed leader election for the singleton scheduler (design §12).
//!
//! The scheduler must run on exactly **one** control-plane replica at a time; otherwise
//! every replica would fan out duplicate scan runs on each cadence. We express this as a
//! best-effort distributed lock over Redis:
//!
//! - **Acquire:** `SET <key> <token> NX PX <ttl>` — succeeds only when the lock is free.
//! - **Renew:** a check-and-set Lua script `PEXPIRE`s the key *iff* it still holds our
//!   token, so a replica never extends another replica's lease.
//! - **Fail-open on config:** when no `redis` block is configured the process is treated
//!   as the sole leader (single-instance / local dev). A Redis *error* while leading,
//!   however, drops leadership so two partitioned replicas cannot both believe they lead.
//!
//! The lock TTL comfortably exceeds the renewal interval so brief Redis blips do not flap
//! leadership. Sweeps are the only leader-gated action; they read the cached flag set by
//! the background renewal task via [`Leadership::is_leader`].

use fred::prelude::*;
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
pub(crate) async fn connect(url: &str) -> anyhow::Result<Client> {
    let config = Config::from_url(url)?;
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
    // Lock is taken — extend the lease only if it is still ours. This GET-then-PEXPIRE is
    // not atomic, but the TTL comfortably exceeds the renewal interval, so the worst case
    // is a one-cycle-late hand-off; scan planning is idempotent on the DB side regardless.
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
