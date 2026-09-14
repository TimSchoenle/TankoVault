-- `series_sources (provider_id, series_id)`, so "how many series does this provider carry" is
-- answered from the index alone.
--
-- The public provider list behind Discover's filter counts `DISTINCT series_id` per provider,
-- and the console's provider table groups by the same pair. The only index leading on
-- `provider_id` was the unique `(provider_id, source_path)`, which carries no `series_id`, so
-- every count visited the heap row of every source. Measured on the local catalogue with its
-- 80 777 sources spread over 102 providers:
--
--    index                         | plan per provider           | warm    | cold
--   -------------------------------+-----------------------------+---------+--------
--    (provider_id, source_path)    | Bitmap Heap Scan            | 37.1 ms | 343 ms
--    (provider_id, series_id)      | Index Only, Heap Fetches: 0 |  6.2 ms |  14 ms
--
-- The list is fetched on every Discover load and, until the client stopped refetching on token
-- renewal, on every silent refresh of Home; production logged it at 1.0–21.4 s.
--
-- Not `CONCURRENTLY`, for the reason `0020_performance_indexes` gives. `series_sources` is tens of
-- thousands of rows, so the build takes well under a second; on a deployment where it does not,
-- run this first and the migration finds it present:
--
--   CREATE INDEX CONCURRENTLY IF NOT EXISTS series_sources_provider_series_idx
--       ON series_sources (provider_id, series_id);

CREATE INDEX IF NOT EXISTS series_sources_provider_series_idx
    ON series_sources (provider_id, series_id);
