-- Raise the per-provider crawl budgets stored on existing providers.
--
-- The policy ceilings doubled (`MAX_RPS` 4.0 -> 8.0, `MAX_CONCURRENCY` 8 -> 16) and the serde
-- defaults with them (1.0/2 -> 2.0/4). Neither reaches a running install: `providers.politeness`
-- is a stored JSONB blob, and `xtask seed` skips providers that already exist, so a preset or
-- default change only affects a fresh database. This migration is what moves the rows.
--
-- Each field is scaled by the same factor the ceiling moved rather than being set to a flat
-- value, so an operator who had deliberately tuned one provider down keeps its position
-- relative to the others instead of being reset to the new default.
--
-- Absent keys are left absent on purpose: `tankovault_domain::Politeness` supplies its serde
-- default when a key is missing, so a row without `rps` already picks up the new 2.0 without
-- being written here.

-- Requests per second, doubled and capped at the new `MAX_RPS`. `least` mirrors the `clamped()`
-- call every read path applies anyway; doing it here keeps the stored value honest rather than
-- leaving an out-of-policy number in the column that only looks different once it is read back.
UPDATE providers
   SET politeness = politeness || jsonb_build_object(
         'rps', least((politeness ->> 'rps')::float8 * 2, 8.0)
       ),
       updated_at = now()
 WHERE politeness ? 'rps';

-- In-flight requests, doubled and capped at the new `MAX_CONCURRENCY`.
UPDATE providers
   SET politeness = politeness || jsonb_build_object(
         'concurrency', least((politeness ->> 'concurrency')::int * 2, 16)
       ),
       updated_at = now()
 WHERE politeness ? 'concurrency';

-- Halved, because a crawl delay is the floor that a raised `rps` cannot cross: a provider left
-- at 1000ms would ignore the doubled rate entirely, and the budget increase would silently do
-- nothing for exactly the providers an operator had throttled hardest.
UPDATE providers
   SET politeness = politeness || jsonb_build_object(
         'crawl_delay_ms', (politeness ->> 'crawl_delay_ms')::bigint / 2
       ),
       updated_at = now()
 WHERE politeness ? 'crawl_delay_ms'
   AND (politeness ->> 'crawl_delay_ms')::bigint > 0;
