//! Redis-backed rate-limit counters, shared across replicas.
//!
//! The in-memory store multiplies the configured limit by the replica count. This one
//! holds a single token bucket per (class, client) in Redis, so the limit means what it
//! says regardless of how many API replicas are running.

use super::{RateLimitDecision, RateLimitStore, RouteClass};
use async_trait::async_trait;
use fred::clients::Client;
use fred::interfaces::LuaInterface;
use fred::prelude::*;
use std::time::Duration;
use tankovault_config::{RateLimitConfig, RateLimitPolicy};

/// Token-bucket check-and-consume, evaluated atomically inside Redis.
///
/// Atomic: separate round trips would let concurrent requests each observe the
/// pre-decrement count and all be allowed. Time is passed in as an argument rather than
/// read via `redis.call('TIME')`, keeping the script deterministic and safe to replicate.
///
/// Returns `{allowed, remaining, retry_after_ms}`.
const TOKEN_BUCKET: &str = r"
local key      = KEYS[1]
local capacity = tonumber(ARGV[1])
local refill   = tonumber(ARGV[2])
local now_ms   = tonumber(ARGV[3])
local ttl_ms   = tonumber(ARGV[4])

local state  = redis.call('HMGET', key, 'tokens', 'ts')
local tokens = tonumber(state[1])
local ts     = tonumber(state[2])

if tokens == nil or ts == nil then
  tokens = capacity
  ts     = now_ms
end

-- Refill for the elapsed interval, clamped at the bucket's capacity. `max(0, ...)` guards
-- against a clock that stepped backwards between replicas.
local elapsed = math.max(0, now_ms - ts) / 1000.0
tokens = math.min(capacity, tokens + elapsed * refill)

local allowed = 0
local retry_after_ms = 0
if tokens >= 1 then
  tokens  = tokens - 1
  allowed = 1
else
  retry_after_ms = math.ceil(((1 - tokens) / refill) * 1000)
end

redis.call('HSET', key, 'tokens', tokens, 'ts', now_ms)
redis.call('PEXPIRE', key, ttl_ms)

return {allowed, math.floor(tokens), retry_after_ms}
";

/// Shared token buckets in Redis.
pub struct RedisStore {
    client: Client,
    policies: [RateLimitPolicy; RouteClass::COUNT],
}

impl RedisStore {
    /// Use `client` for counters, with the per-class policies from `cfg`.
    ///
    /// The client must already be initialised (`Client::init`); this store does not manage
    /// its lifecycle, since the same connection is typically shared with other Redis users
    /// in the process.
    #[must_use]
    pub fn new(client: Client, cfg: &RateLimitConfig) -> Self {
        Self {
            client,
            policies: RouteClass::ALL.map(|class| class.policy(cfg)),
        }
    }

    /// Namespaced key, so rate-limit state cannot collide with the scheduler's leader
    /// lease or any other user of the same Redis.
    fn redis_key(class: RouteClass, key: &str) -> String {
        format!("tankovault:ratelimit:{}:{key}", class.as_str())
    }
}

#[async_trait]
impl RateLimitStore for RedisStore {
    async fn check(&self, class: RouteClass, key: &str) -> RateLimitDecision {
        let policy = self.policies[class.index()];
        let capacity = f64::from(policy.capacity().max(1));
        // Tokens per second, from the per-minute sustained rate.
        let refill = f64::from(policy.per_minute.max(1)) / 60.0;
        let now_ms = now_millis();
        // Keep an idle bucket only long enough to refill completely; past that its state
        // is indistinguishable from a fresh one, so retaining it wastes memory.
        //
        // The cast is bounded by construction: `capacity / refill` is at most
        // `u32::MAX * 60`, which is nowhere near `i64::MAX` even after the ×1000.
        #[expect(
            clippy::cast_possible_truncation,
            reason = "bounded by construction: capacity/refill is at most u32::MAX * 60"
        )]
        let ttl_ms = ((capacity / refill) * 1000.0).ceil() as i64 + 1000;

        let outcome: Result<(i64, i64, i64), Error> = self
            .client
            .eval(
                TOKEN_BUCKET,
                vec![Self::redis_key(class, key)],
                vec![
                    capacity.to_string(),
                    refill.to_string(),
                    now_ms.to_string(),
                    ttl_ms.to_string(),
                ],
            )
            .await;

        match outcome {
            Ok((allowed, remaining, retry_after_ms)) => {
                let limit = policy.capacity();
                if allowed == 1 {
                    RateLimitDecision::allow(
                        limit,
                        u32::try_from(remaining.max(0)).unwrap_or(u32::MAX),
                    )
                } else {
                    RateLimitDecision::deny(
                        limit,
                        Duration::from_millis(
                            u64::try_from(retry_after_ms.max(0)).unwrap_or(1_000),
                        ),
                    )
                }
            }
            Err(e) => {
                // Fail open: a counter-store outage must not take the edge down. This is a
                // deliberate availability-over-enforcement trade, logged so it is visible
                // rather than silently unlimited.
                tracing::warn!(
                    error = %e,
                    class = class.as_str(),
                    "redis rate-limit check failed; allowing the request"
                );
                metrics::counter!(
                    crate::metrics::names::RATE_LIMIT_STORE_ERRORS,
                    "backend" => "redis"
                )
                .increment(1);
                RateLimitDecision::allow(policy.capacity(), 0)
            }
        }
    }
}

/// Milliseconds since the Unix epoch.
///
/// A clock before the epoch would mean a catastrophically misconfigured host; treating it
/// as `0` keeps the script's arithmetic well-defined instead of panicking on the edge.
fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_are_namespaced_per_class() {
        // Two classes must not share a bucket, and neither may collide with the
        // control-plane's `tankovault:control-plane:scheduler-leader` key.
        let global = RedisStore::redis_key(RouteClass::Global, "ip:203.0.113.1");
        let auth = RedisStore::redis_key(RouteClass::Auth, "ip:203.0.113.1");
        assert_ne!(global, auth);
        assert!(global.starts_with("tankovault:ratelimit:"));
        assert!(auth.starts_with("tankovault:ratelimit:"));
    }

    #[test]
    fn the_script_returns_the_three_values_the_store_unpacks() {
        // A drift between the Lua `return` and the tuple type would only surface as a
        // runtime decode error under load, so pin the contract here.
        assert!(TOKEN_BUCKET.contains("return {allowed, math.floor(tokens), retry_after_ms}"));
    }

    #[test]
    fn epoch_millis_are_positive() {
        assert!(now_millis() > 0);
    }
}
