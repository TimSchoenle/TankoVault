-- Restore the pre-raise crawl budgets: halve rate and concurrency back under the old ceilings
-- (`MAX_RPS` 4.0, `MAX_CONCURRENCY` 8), and restore the crawl delay.
--
-- **This is not an exact inverse, and cannot be.** The up-migration capped at the new ceilings,
-- so any provider that saturated (rps >= 4.0 or concurrency >= 8 before it ran) lost the
-- information needed to distinguish it from one that landed there by doubling. Halving returns
-- those rows to the ceiling value, not to whatever they held originally. Reverting the code
-- without reverting the data is the safe direction: `clamped()` bounds every read path, so a row
-- left above the restored ceiling is clamped down on read rather than crawling out of policy.

UPDATE providers
   SET politeness = politeness || jsonb_build_object(
         'rps', least((politeness ->> 'rps')::float8 / 2, 4.0)
       ),
       updated_at = now()
 WHERE politeness ? 'rps';

UPDATE providers
   SET politeness = politeness || jsonb_build_object(
         'concurrency', greatest(least((politeness ->> 'concurrency')::int / 2, 8), 1)
       ),
       updated_at = now()
 WHERE politeness ? 'concurrency';

UPDATE providers
   SET politeness = politeness || jsonb_build_object(
         'crawl_delay_ms', (politeness ->> 'crawl_delay_ms')::bigint * 2
       ),
       updated_at = now()
 WHERE politeness ? 'crawl_delay_ms'
   AND (politeness ->> 'crawl_delay_ms')::bigint > 0;
