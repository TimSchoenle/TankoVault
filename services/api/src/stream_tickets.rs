//! Short-lived, single-use tickets for opening the SSE notification stream (SEC-8).
//!
//! # Why this exists
//!
//! `GET /v1/me/stream` cannot authenticate with a header: the browser's `EventSource` API
//! offers no way to set one, which is why the route took its access token in the query string.
//! A query string is the wrong place for a 15-minute bearer credential — `TraceLayer` records
//! the URI as a span field, `services/frontend`'s proxy preserves it verbatim for the upstream,
//! any reverse proxy or CDN in front writes it to an access log, and the browser keeps it in
//! history. Anyone who can read a log line can replay the token against `Authorization: Bearer`
//! on every other `/v1/me/*` route.
//!
//! A ticket removes the *value* of that disclosure rather than the disclosure itself. It is an
//! opaque 32-byte random string, valid for [`TICKET_TTL`], usable exactly once, and it grants
//! nothing but the stream. A leaked log line names a credential that was already spent by the
//! request that produced the log line.
//!
//! # Shape
//!
//! [`StreamTicketStore`] is the seam, with two implementations for the same reason the rate
//! limiter has two ([`crate::AppState`] holds whichever the deployment got):
//!
//! - [`RedisStreamTickets`] — the real one. Tickets have to be redeemable by whichever replica
//!   the stream request lands on, which is a shared-state problem, and Redis is where this
//!   system already keeps shared short-lived state (`tankovault_service::ratelimit::redis`).
//!   The same client serves both; see `main::connect_redis`.
//! - [`MemoryStreamTickets`] — the fallback when Redis is unconfigured or unreachable, and what
//!   the test harness uses. Correct for a single replica and *incorrect* across several, so it
//!   warns at boot rather than failing silently. That mirrors the rate limiter's existing
//!   degradation exactly: with two replicas behind a round-robin balancer a ticket minted on one
//!   is unknown to the other, and the stream fails to open until a retry lands on the right one.
//!   The alternative — refusing to boot without Redis — would take the whole edge down for a
//!   feature that is already best-effort.
//!
//! # What is stored
//!
//! The key is the **SHA-256 hash** of the ticket, never the ticket itself, so a Redis dump,
//! `MONITOR` session or slowlog entry does not hand over a usable credential. The value is the
//! user id. Redemption is one atomic `GETDEL`, which is what makes "single use" true under
//! concurrency rather than a read-then-delete race.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use tankovault_auth::{generate_refresh_token, hash_refresh_token};
use tankovault_domain::UserId;
use uuid::Uuid;

/// How long a minted ticket stays redeemable.
///
/// Long enough to cover the round trip from the mint call to the browser opening the
/// `EventSource`, short enough that a ticket sitting in a log line is dead by the time anyone
/// reads it. The audit suggested 30 s; there is no reason to be more generous, because the only
/// legitimate holder redeems it immediately.
pub const TICKET_TTL: Duration = Duration::from_secs(30);

/// Namespace prefix, so ticket keys cannot collide with the rate limiter's buckets or the
/// control-plane's scheduler lease in a shared Redis.
const KEY_PREFIX: &str = "tankovault:stream-ticket:";

/// Where stream tickets live.
///
/// Both methods swallow their own infrastructure errors into a `Result<_, String>` the caller
/// logs: a Redis outage must degrade the live stream, never the request that asked for a ticket.
#[async_trait]
pub trait StreamTicketStore: Send + Sync + 'static {
    /// Mint a fresh single-use ticket for `user`, returning the value to hand the client.
    ///
    /// # Errors
    /// A message describing the store failure, for the caller to log.
    async fn mint(&self, user: UserId) -> Result<String, String>;

    /// Redeem `ticket`, returning the user it was minted for.
    ///
    /// `Ok(None)` means "no such ticket": unknown, expired, or already spent. The three are
    /// deliberately indistinguishable — the caller answers `401` to all of them.
    ///
    /// # Errors
    /// A message describing the store failure, for the caller to log.
    async fn consume(&self, ticket: &str) -> Result<Option<UserId>, String>;
}

/// Generate a ticket and the key it is stored under.
///
/// Reuses the refresh-token generator (32 CSPRNG bytes, URL-safe base64) rather than inventing
/// a second opaque-token format, and the same SHA-256 hashing the reset and confirmation tokens
/// use, for the same reason: what is stored must not be replayable.
fn new_ticket() -> (String, String) {
    let raw = generate_refresh_token();
    let key = format!("{KEY_PREFIX}{}", hash_refresh_token(&raw));
    (raw, key)
}

/// The key `ticket` would have been stored under.
fn key_for(ticket: &str) -> String {
    format!("{KEY_PREFIX}{}", hash_refresh_token(ticket))
}

/// Tickets in Redis, redeemable by any replica.
pub struct RedisStreamTickets {
    client: fred::clients::Client,
}

impl RedisStreamTickets {
    /// Use an already-initialised client. Shares the connection with the rate-limit counters.
    #[must_use]
    pub fn new(client: fred::clients::Client) -> Self {
        Self { client }
    }
}

#[async_trait]
impl StreamTicketStore for RedisStreamTickets {
    async fn mint(&self, user: UserId) -> Result<String, String> {
        use fred::interfaces::KeysInterface as _;

        let (raw, key) = new_ticket();
        // `NX` so a hash collision — or a repeated random value, which would mean the CSPRNG is
        // broken — can never overwrite another user's live ticket and hand them this stream.
        let stored: Option<String> = self
            .client
            .set(
                &key,
                user.as_uuid().to_string(),
                Some(fred::types::Expiration::PX(
                    i64::try_from(TICKET_TTL.as_millis()).unwrap_or(30_000),
                )),
                Some(fred::types::SetOptions::NX),
                false,
            )
            .await
            .map_err(|e| e.to_string())?;
        // `SET … NX` answers with a null reply when the key already existed, which is the one
        // case where the ticket we generated is not the ticket that is stored.
        if stored.is_none() {
            return Err("stream ticket key already present".to_owned());
        }
        Ok(raw)
    }

    async fn consume(&self, ticket: &str) -> Result<Option<UserId>, String> {
        use fred::interfaces::KeysInterface as _;

        // One atomic round trip. A `GET` followed by a `DEL` would let two concurrent
        // redemptions of the same ticket both succeed, which is exactly the single-use property
        // this store exists to provide.
        let value: Option<String> = self
            .client
            .getdel(&key_for(ticket))
            .await
            .map_err(|e| e.to_string())?;
        Ok(value
            .and_then(|raw| Uuid::parse_str(&raw).ok())
            .map(UserId::from_uuid))
    }
}

/// Tickets in this process only.
///
/// Correct for one replica; see the module docs for what it costs across several.
#[derive(Default)]
pub struct MemoryStreamTickets {
    /// Ticket key → (user, expiry). Expired entries are dropped lazily on the next write, which
    /// is enough: the map only ever holds tickets minted in the last [`TICKET_TTL`] plus
    /// whatever has not been swept, and every mint sweeps.
    live: Mutex<HashMap<String, (UserId, Instant)>>,
}

impl MemoryStreamTickets {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl StreamTicketStore for MemoryStreamTickets {
    async fn mint(&self, user: UserId) -> Result<String, String> {
        let (raw, key) = new_ticket();
        let now = Instant::now();
        let mut live = self.live.lock().map_err(|_| "ticket mutex poisoned")?;
        live.retain(|_, (_, expires_at)| *expires_at > now);
        live.insert(key, (user, now + TICKET_TTL));
        Ok(raw)
    }

    async fn consume(&self, ticket: &str) -> Result<Option<UserId>, String> {
        let key = key_for(ticket);
        let mut live = self.live.lock().map_err(|_| "ticket mutex poisoned")?;
        // Removed whether or not it is still valid: presenting a ticket spends it.
        let Some((user, expires_at)) = live.remove(&key) else {
            return Ok(None);
        };
        Ok((expires_at > Instant::now()).then_some(user))
    }
}

#[cfg(test)]
mod tests {
    use super::{KEY_PREFIX, MemoryStreamTickets, StreamTicketStore, key_for, new_ticket};
    use tankovault_domain::UserId;

    /// A ticket is redeemable exactly once.
    ///
    /// The whole point of the scheme (SEC-8): the query string still ends up in access logs,
    /// `Referer` headers and browser history, and what makes that harmless is that the value
    /// recorded there is already spent. A store that returned the user twice would put a
    /// 30-second replayable stream credential in every log line instead of an access token —
    /// smaller, but the same class of finding.
    #[tokio::test]
    async fn a_ticket_cannot_be_redeemed_twice() {
        let store = MemoryStreamTickets::new();
        let user = UserId::new();
        let ticket = store.mint(user).await.expect("mint");

        assert_eq!(store.consume(&ticket).await.expect("consume"), Some(user));
        assert_eq!(
            store.consume(&ticket).await.expect("consume"),
            None,
            "a spent ticket must not open a second stream"
        );
    }

    /// An unknown ticket is not an error, just nobody.
    ///
    /// The handler answers `401` for `Ok(None)` and `503` for `Err`, so conflating the two would
    /// turn every guessed ticket into a report that the ticket store is broken.
    #[tokio::test]
    async fn an_unknown_ticket_resolves_to_nobody() {
        let store = MemoryStreamTickets::new();
        assert_eq!(store.consume("not-a-ticket").await.expect("consume"), None);
    }

    /// Two mints for the same user are two distinct tickets.
    ///
    /// `EventSource` reconnects by re-running the whole open sequence, so a client mints a fresh
    /// ticket per attempt. A store that returned a stable per-user value would be a bearer token
    /// in the query string again, wearing a different name.
    #[tokio::test]
    async fn each_mint_is_a_fresh_value() {
        let store = MemoryStreamTickets::new();
        let user = UserId::new();
        let first = store.mint(user).await.expect("mint");
        let second = store.mint(user).await.expect("mint");
        assert_ne!(first, second);
        assert_eq!(store.consume(&first).await.expect("consume"), Some(user));
        assert_eq!(store.consume(&second).await.expect("consume"), Some(user));
    }

    /// What is stored is the hash, not the ticket.
    ///
    /// A Redis dump, `MONITOR` session or slowlog entry must not hand over a usable credential.
    /// The key derivation is shared by both stores, so pinning it here covers both.
    #[test]
    fn the_stored_key_does_not_contain_the_ticket() {
        let (raw, key) = new_ticket();
        assert_eq!(key, key_for(&raw));
        assert!(key.starts_with(KEY_PREFIX));
        assert!(
            !key.contains(&raw),
            "the ticket value must not be recoverable from the key it is stored under"
        );
        // SHA-256, hex: 64 characters after the namespace.
        assert_eq!(key.len() - KEY_PREFIX.len(), 64);
    }
}
