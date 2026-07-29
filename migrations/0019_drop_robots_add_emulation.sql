-- Retire robots.txt enforcement; give every provider a browser emulation profile.
--
-- These land together because they are the same decision. The crawler no longer presents
-- itself as `TankoVaultBot`: providers sit behind Cloudflare/DDoS-Guard, which fingerprint
-- the TLS ClientHello and the HTTP/2 SETTINGS frame, so the fetch stack now reproduces a
-- real browser's handshake (`wreq` + an emulation profile) and sends that browser's
-- user-agent. A robots.txt gate keyed on a user-agent the crawler no longer sends can only
-- match the `*` group, and matching `*` while impersonating Chrome enforces nothing the
-- provider actually asked of us — so the gate goes rather than lingering as dead code that
-- reads like a guarantee.

-- ---------------------------------------------------------------------------------------
-- 1. Drop the robots.txt cache.
-- ---------------------------------------------------------------------------------------
--
-- Nothing else referenced these columns; the parser and the `RobotsFetcher` layer are gone
-- from `crates/fetch`. Politeness is now enforced solely by the budget the operator sets
-- (`rps`, `concurrency`, `crawl_delay_ms`) plus the provider-directed 429/503 backoff.
ALTER TABLE providers
  DROP COLUMN robots_txt,
  DROP COLUMN robots_at;

-- ---------------------------------------------------------------------------------------
-- 2. Backfill the emulation profile into stored politeness.
-- ---------------------------------------------------------------------------------------
--
-- `politeness` is a JSONB blob deserialized into `tankovault_domain::Politeness`, whose
-- serde default already supplies `"chrome"` for rows written before this migration. The
-- backfill makes the stored value explicit anyway, so that operators reading the column see
-- what the crawler will actually do instead of an absence that resolves elsewhere.
--
-- `jsonb_set` with `create_missing = true` only writes rows that lack the key, leaving any
-- provider an operator has already configured untouched.
UPDATE providers
   SET politeness = jsonb_set(politeness, '{emulation}', '"chrome"'::jsonb, true),
       updated_at = now()
 WHERE NOT (politeness ? 'emulation');
