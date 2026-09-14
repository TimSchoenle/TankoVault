-- Production query-latency diagnostics, September 2026.
--
-- Generated at d3b9ec05 from the committed `.sqlx` cache: every EXPLAIN below prepares the exact
-- statement text the deployed binaries compiled, not a hand copy. Regenerate if the queries move.
--
-- WHAT IT DOES
--   Read-only. The session sets `default_transaction_read_only`, so nothing here can write, and
--   every statement runs in its own transaction, so no snapshot is held open across the run
--   (a long-lived snapshot would stall vacuum on `chapters` while the crawler writes).
--   EXPLAIN ANALYZE *executes* each SELECT. Expect 5-20 minutes against a starved instance, and
--   extra load on Postgres for that time. Run it off-peak, or pause scanning first
--   (`TANKOVAULT_PROVIDERS__ACTIVE` / the console) if you want numbers without crawler churn --
--   but then say so when pasting results, because churn is part of what is being measured.
--   Each statement is capped by `statement_timeout = 120s`; a cancel is recorded and the script
--   continues.
--
-- HOW TO RUN (on the Docker host, from the compose project directory)
--
--   docker compose exec -T postgres psql -U tankovault -d tankovault -X \
--       -f - < docs/perf/2026-09-prod-diagnostics.sql > prod-diag-$(date +%Y%m%d-%H%M).txt 2>&1
--
--   The user whose Home is measured defaults to the account with the most watchlist rows
--   (the ~550-row account in the slow log). Pin a specific one with `-v uid=<uuid>`.
--   Skip the EXPLAIN section (catalogue/config numbers only, a few seconds) with `-v explain=off`.
--
-- ALSO CAPTURE, OUTSIDE PSQL (paste the output alongside)
--
--   # Container memory limit, usage, page cache, OOM kills and memory/IO pressure (cgroup v2).
--   docker compose exec -T postgres sh -c 'for f in memory.max memory.current memory.peak \
--       memory.events memory.pressure io.pressure; do echo "== $f"; cat /sys/fs/cgroup/$f; done; \
--       echo "== memory.stat"; grep -E "^(anon|file|shmem|file_mapped|active_file|inactive_file|workingset_refault_file|pgmajfault) " /sys/fs/cgroup/memory.stat'
--   docker stats --no-stream
--   free -m; nproc                    # host headroom, to size a new limit against
--
--   # The API's pool settings. Filtered on purpose: TANKOVAULT_DATABASE__URL carries the password.
--   docker compose exec -T api sh -c 'env | grep -E "^TANKOVAULT_(DATABASE__(MAX_CONNECTIONS|ACQUIRE_TIMEOUT_SECS)|SECURITY__REQUEST_TIMEOUT_SECS)="'
--   Defaults when unset (crates/config/src/database.rs, security.rs, docs/CONFIGURATION.md):
--   max_connections 16 per replica, acquire_timeout_secs 10, request_timeout_secs 30. The slow
--   log's 29.9 s / 0-row entries are that request timeout dropping the query future: sqlx logs
--   from `QueryLogger::drop`, and nothing cancels the statement server-side. There is no min_connections, idle_timeout,
--   max_lifetime or statement_timeout set anywhere (crates/db/src/pool.rs uses sqlx defaults:
--   min 0, idle 10 min, lifetime 30 min, test_before_acquire off). Repeat the `env` line for
--   worker, control-plane, notifier and sync: every replica holds its own pool against the same
--   `max_connections`.
--
-- OPTIONAL, NEEDS A POSTGRES RESTART: pg_stat_statements and auto_explain
--
--   Neither is loaded today. To collect a day of real per-statement numbers, add to the
--   `postgres.command` list in deploy/docker-compose.yml:
--
--       - -c
--       - shared_preload_libraries=pg_stat_statements,auto_explain
--       - -c
--       - pg_stat_statements.max=5000
--       - -c
--       - pg_stat_statements.track=top
--       - -c
--       - track_io_timing=on
--       - -c
--       - auto_explain.log_min_duration=2s
--       - -c
--       - auto_explain.log_analyze=on
--       - -c
--       - auto_explain.log_buffers=on
--       - -c
--       - auto_explain.log_nested_statements=on
--       - -c
--       - auto_explain.log_format=text
--
--   then `docker compose up -d postgres` (a restart: every service's pool reconnects) and once:
--       docker compose exec -T postgres psql -U tankovault -d tankovault -c 'CREATE EXTENSION IF NOT EXISTS pg_stat_statements'
--   `track_io_timing` is what separates "slow because it read from disk" from "slow on CPU" in
--   both EXPLAIN and pg_stat_statements; its overhead is a clock read per block I/O
--   (`docker compose exec postgres pg_test_timing` shows the cost on this host).
--   auto_explain with log_analyze instruments every statement to decide whether to log it;
--   that costs a few percent, acceptable for a diagnostic window. `auto_explain.log_timing=off`
--   removes most of it if needed and keeps row counts and buffers. Plans land in
--   `docker compose logs postgres`.
--
--   OPTIONAL, NO RESTART: `CREATE EXTENSION IF NOT EXISTS pg_buffercache;` (contrib, ships in the
--   image) lets section 5 show which relations actually occupy shared_buffers.

\set ON_ERROR_STOP off
\pset pager off
\timing on
SET statement_timeout = '120s';
SET default_transaction_read_only = on;
SET application_name = 'tankovault-diagnostics';

\echo '################ 1. Migration state (0052 and 0055 must both be present)'
SELECT max(version) AS latest_migration FROM _sqlx_migrations;
SELECT version, description, installed_on, success
  FROM _sqlx_migrations WHERE version >= 50 ORDER BY version;
SELECT v AS missing_migration
  FROM generate_series(1, 56) v
 WHERE v NOT IN (SELECT version FROM _sqlx_migrations WHERE success);
-- The indexes the reading surfaces depend on, whatever the ledger says.
SELECT indexrelid::regclass AS index, indrelid::regclass AS "table", indisvalid,
       pg_size_pretty(pg_relation_size(indexrelid)) AS size,
       pg_get_indexdef(indexrelid) AS definition
  FROM pg_index
 WHERE indrelid IN ('chapters'::regclass, 'series_sources'::regclass, 'scan_tasks'::regclass,
                    'scan_runs'::regclass, 'merge_decisions'::regclass,
                    'watchlist_entries'::regclass, 'read_progress'::regclass)
 ORDER BY indrelid::regclass::text, indexrelid::regclass::text;

\echo '################ 2. Server settings'
SELECT version();
SELECT name, setting, unit, source
  FROM pg_settings
 WHERE name IN ('shared_buffers', 'work_mem', 'maintenance_work_mem', 'effective_cache_size',
                'random_page_cost', 'effective_io_concurrency', 'max_connections',
                'statement_timeout', 'idle_in_transaction_session_timeout', 'jit',
                'jit_above_cost', 'max_parallel_workers_per_gather', 'shared_preload_libraries',
                'track_io_timing', 'autovacuum_vacuum_scale_factor',
                'autovacuum_analyze_scale_factor', 'autovacuum_max_workers',
                'autovacuum_vacuum_cost_limit', 'checkpoint_timeout', 'max_wal_size',
                'wal_buffers', 'huge_pages', 'default_statistics_target', 'plan_cache_mode')
 ORDER BY name;
SHOW shared_buffers;
SHOW work_mem;
SHOW effective_cache_size;
SHOW random_page_cost;
SHOW max_connections;
SHOW statement_timeout;

\echo '################ 3. Relation sizes'
SELECT c.relname AS "table",
       c.reltuples::bigint AS est_rows,
       pg_size_pretty(pg_total_relation_size(c.oid)) AS total,
       pg_size_pretty(pg_relation_size(c.oid)) AS heap,
       pg_size_pretty(pg_indexes_size(c.oid)) AS indexes,
       pg_size_pretty(COALESCE(pg_total_relation_size(c.reltoastrelid), 0)) AS toast,
       pg_total_relation_size(c.oid) AS total_bytes
  FROM pg_class c
 WHERE c.relkind = 'r' AND c.relnamespace = 'public'::regnamespace
 ORDER BY pg_total_relation_size(c.oid) DESC
 LIMIT 25;
SELECT s.relname AS "table", s.indexrelname AS index,
       pg_size_pretty(pg_relation_size(s.indexrelid)) AS size,
       s.idx_scan, s.last_idx_scan, s.idx_tup_read, s.idx_tup_fetch
  FROM pg_stat_user_indexes s
 WHERE s.relname IN ('chapters', 'series_sources', 'series', 'scan_tasks', 'scan_runs',
                     'merge_decisions', 'watchlist_entries', 'read_progress',
                     'user_provider_early_access', 'series_embedding')
 ORDER BY pg_relation_size(s.indexrelid) DESC;
SELECT pg_size_pretty(pg_database_size(current_database())) AS database_size;

\echo '################ 4. Row churn, vacuum and analyze'
SELECT relname AS "table", n_live_tup, n_dead_tup,
       round(100.0 * n_dead_tup / NULLIF(n_live_tup + n_dead_tup, 0), 1) AS dead_pct,
       n_tup_ins, n_tup_upd, n_tup_hot_upd, n_tup_del, n_mod_since_analyze,
       last_vacuum, last_autovacuum, last_analyze, last_autoanalyze,
       vacuum_count, autovacuum_count, autoanalyze_count, seq_scan, seq_tup_read, idx_scan
  FROM pg_stat_user_tables
 WHERE relname IN ('chapters', 'series_sources', 'series', 'scan_tasks', 'scan_runs',
                   'merge_decisions', 'watchlist_entries', 'read_progress', 'providers',
                   'series_titles', 'series_embedding', 'notifications')
 ORDER BY n_live_tup DESC;
-- The visibility map decides whether an "Index Only Scan" really skips the heap.
SELECT c.relname AS "table", c.relpages, c.relallvisible,
       round(100.0 * c.relallvisible / NULLIF(c.relpages, 0), 1) AS all_visible_pct
  FROM pg_class c
 WHERE c.relname IN ('chapters', 'series_sources', 'scan_tasks', 'watchlist_entries', 'read_progress');
SELECT pid, relid::regclass, phase, heap_blks_total, heap_blks_scanned, index_vacuum_count
  FROM pg_stat_progress_vacuum;

\echo '################ 5. Cache hit ratio and eviction'
SELECT datname, blks_hit, blks_read,
       round(100.0 * blks_hit / NULLIF(blks_hit + blks_read, 0), 2) AS hit_pct,
       temp_files, pg_size_pretty(temp_bytes) AS temp_bytes, deadlocks, conflicts,
       stats_reset
  FROM pg_stat_database WHERE datname = current_database();
SELECT relname AS "table",
       heap_blks_read, heap_blks_hit,
       round(100.0 * heap_blks_hit / NULLIF(heap_blks_hit + heap_blks_read, 0), 2) AS heap_hit_pct,
       idx_blks_read, idx_blks_hit,
       round(100.0 * idx_blks_hit / NULLIF(idx_blks_hit + idx_blks_read, 0), 2) AS idx_hit_pct,
       toast_blks_read, toast_blks_hit
  FROM pg_statio_user_tables
 WHERE relname IN ('chapters', 'series_sources', 'series', 'scan_tasks', 'scan_runs',
                   'merge_decisions', 'watchlist_entries', 'read_progress', 'providers',
                   'series_titles', 'series_embedding')
 ORDER BY heap_blks_read + idx_blks_read DESC;
SELECT indexrelname AS index, idx_blks_read, idx_blks_hit,
       round(100.0 * idx_blks_hit / NULLIF(idx_blks_hit + idx_blks_read, 0), 2) AS hit_pct
  FROM pg_statio_user_indexes
 WHERE relname IN ('chapters', 'series_sources', 'scan_tasks')
 ORDER BY idx_blks_read DESC;
-- Evictions from shared_buffers, per backend type (PG16+).
SELECT backend_type, object, context, reads, read_time, writes, write_time,
       extends, hits, evictions, reuses, fsyncs
  FROM pg_stat_io
 WHERE reads > 0 OR evictions > 0 OR writes > 0
 ORDER BY evictions DESC NULLS LAST, reads DESC NULLS LAST;
SELECT * FROM pg_stat_checkpointer;
SELECT * FROM pg_stat_bgwriter;

SELECT EXISTS (SELECT 1 FROM pg_extension WHERE extname = 'pg_buffercache') AS has_buffercache \gset
\if :has_buffercache
\echo '--- shared_buffers occupancy by relation'
SELECT c.relname, count(*) AS buffers, pg_size_pretty(count(*) * 8192) AS cached,
       round(100.0 * count(*) / (SELECT setting::bigint FROM pg_settings WHERE name = 'shared_buffers'), 1)
           AS pct_of_shared_buffers,
       round(100.0 * count(*) * 8192 / NULLIF(pg_relation_size(c.oid), 0), 1) AS pct_of_relation
  FROM pg_buffercache b
  JOIN pg_class c ON b.relfilenode = pg_relation_filenode(c.oid)
 WHERE b.reldatabase = (SELECT oid FROM pg_database WHERE datname = current_database())
 GROUP BY c.oid, c.relname
 ORDER BY buffers DESC
 LIMIT 20;
\else
\echo '--- pg_buffercache not installed; skipped (see header)'
\endif

\echo '################ 6. Connections right now'
SELECT usename, application_name, client_addr, state, wait_event_type, wait_event,
       count(*) AS n, max(now() - state_change) AS longest_in_state
  FROM pg_stat_activity
 WHERE backend_type = 'client backend'
 GROUP BY 1, 2, 3, 4, 5, 6
 ORDER BY n DESC;
SELECT pid, now() - query_start AS running_for, wait_event_type, wait_event,
       left(regexp_replace(query, '\s+', ' ', 'g'), 160) AS query
  FROM pg_stat_activity
 WHERE state = 'active' AND pid <> pg_backend_pid()
 ORDER BY query_start;

\echo '################ 7. Data shape the plans depend on'
SELECT user_id, count(*) AS watchlist_rows FROM watchlist_entries
 GROUP BY user_id ORDER BY count(*) DESC LIMIT 5;
SELECT count(*) AS scan_tasks_total,
       count(*) FILTER (WHERE state = 'failed') AS failed,
       count(*) FILTER (WHERE state = 'failed' AND acknowledged_at IS NULL) AS failed_open,
       count(*) FILTER (WHERE state IN ('queued', 'claimed', 'running')) AS open,
       min(created_at) AS oldest, max(created_at) AS newest
  FROM scan_tasks;
SELECT count(*) AS scan_runs_total, min(created_at) AS oldest FROM scan_runs;
SELECT count(*) AS merge_decisions_total,
       count(*) FILTER (WHERE undo IS NOT NULL) AS with_undo,
       count(*) FILTER (WHERE flagged_at IS NOT NULL) AS flagged,
       count(*) FILTER (WHERE cardinality(blocked_by) > 0) AS blocked,
       pg_size_pretty(sum(pg_column_size(undo))) AS undo_bytes
  FROM merge_decisions;
SELECT count(*) AS chapters_last_24h FROM chapters WHERE discovered_at > now() - interval '24 hours';
SELECT date_trunc('day', discovered_at) AS day, count(*) AS chapters_discovered
  FROM chapters WHERE discovered_at > now() - interval '14 days'
 GROUP BY 1 ORDER BY 1;

\echo '################ 8. pg_stat_statements (only if loaded)'
SELECT EXISTS (SELECT 1 FROM pg_extension WHERE extname = 'pg_stat_statements') AS has_pgss \gset
\if :has_pgss
SELECT calls, round(total_exec_time::numeric / 1000, 1) AS total_s,
       round(mean_exec_time::numeric, 1) AS mean_ms, round(max_exec_time::numeric, 1) AS max_ms,
       round(stddev_exec_time::numeric, 1) AS stddev_ms, rows,
       shared_blks_hit, shared_blks_read, temp_blks_written,
       round(shared_blk_read_time::numeric, 1) AS read_ms,
       left(regexp_replace(query, '\s+', ' ', 'g'), 200) AS query
  FROM pg_stat_statements
 ORDER BY total_exec_time DESC
 LIMIT 30;
SELECT stats_reset FROM pg_stat_statements_info;
\else
\echo '--- pg_stat_statements not loaded; skipped (see header)'
\endif

\if :{?explain}
\else
\set explain on
\endif
\if :explain
\echo '################ 9. EXPLAIN (ANALYZE, BUFFERS, SETTINGS), twice each'
\echo 'Run 1 is the cache as the crawler left it; run 2 follows immediately, so it is warm.'
\echo 'The generic-plan run is what a pooled connection executes once sqlx has used the'
\echo 'statement 5 times (sqlx keeps prepared statements per connection).'
\echo 'JIT: production runs jit=off. With jit=on it fires when the top-level estimated cost'
\echo 'exceeds jit_above_cost (default 100000); read that off the first line of each plan.'
\if :{?uid}
\else
SELECT user_id AS uid FROM watchlist_entries GROUP BY user_id ORDER BY count(*) DESC LIMIT 1 \gset
\endif
\echo 'uid =' :uid
SELECT count(*) AS watchlist_rows,
       count(*) FILTER (WHERE status IN ('reading', 'planned', 'paused')) AS continue_candidates,
       (SELECT count(*) FROM series_sources ss JOIN watchlist_entries w2 ON w2.series_id = ss.series_id
         WHERE w2.user_id = :'uid') AS watched_sources,
       (SELECT count(*) FROM user_provider_early_access e WHERE e.user_id = :'uid') AS early_access_opt_ins
  FROM watchlist_entries WHERE user_id = :'uid';

-- ---------------------------------------------------------------------------
-- continue_reading: crates/db/src/repo/tracking/dashboard.rs::continue_reading
-- .sqlx/query-7f02e73343e2ca78f927911d688b015d2949ddc20d32aaf3550e541f842f5d4c.json
\echo '### continue_reading'
PREPARE q_7f02e73343e2 AS
SELECT w.series_id, s.canonical_title AS series_title, s.cover_url, COALESCE(rp.last_read_whole_number, 0)::float8 AS "last_read_number!", agg.next_number AS next_number, agg.unread AS "unread!" FROM watchlist_entries w JOIN series s ON s.id = w.series_id LEFT JOIN read_progress rp ON rp.user_id = w.user_id AND rp.series_id = w.series_id CROSS JOIN LATERAL ( SELECT min(c.number_milli) AS next_number, count(DISTINCT c.number_milli / 10000) AS unread FROM series_sources ss JOIN chapters c ON c.series_source_id = ss.id WHERE ss.series_id = w.series_id AND c.number_milli >= (floor(COALESCE(rp.last_read_whole_number, 0))::bigint + 1) * 10000 AND NOT (c.number_milli % 10000 <> 0 AND rp.last_read_part_number IS NOT NULL AND c.number_milli <= (rp.last_read_part_number * 10000)::bigint) AND (c.access = 'free' OR c.unlocks_at <= now() OR ss.provider_id = ANY(ARRAY( SELECT e.provider_id FROM user_provider_early_access e WHERE e.user_id = $1))) ) agg CROSS JOIN LATERAL ( SELECT max((SELECT max(c2.discovered_at) FROM chapters c2 WHERE c2.series_source_id = ss2.id AND (c2.access = 'free' OR c2.unlocks_at <= now() OR ss2.provider_id = ANY(ARRAY( SELECT e.provider_id FROM user_provider_early_access e WHERE e.user_id = $1))))) AS last_activity FROM series_sources ss2 WHERE ss2.series_id = w.series_id ) act WHERE w.user_id = $1 AND w.status IN ('reading','planned','paused') AND agg.unread > 0 ORDER BY act.last_activity DESC NULLS LAST, w.series_id;
\echo '--- continue_reading run 1 (cache as found)'
EXPLAIN (ANALYZE, BUFFERS, SETTINGS) EXECUTE q_7f02e73343e2(:'uid');
\echo '--- continue_reading run 2 (warm)'
EXPLAIN (ANALYZE, BUFFERS, SETTINGS) EXECUTE q_7f02e73343e2(:'uid');
\echo '--- continue_reading generic plan (what a connection runs after 5 executions)'
SET plan_cache_mode = force_generic_plan;
EXPLAIN (ANALYZE, BUFFERS) EXECUTE q_7f02e73343e2(:'uid');
RESET plan_cache_mode;

-- ---------------------------------------------------------------------------
-- feed: crates/db/src/repo/tracking/dashboard.rs::feed (limit 100, as /v1/me/feed calls it)
-- .sqlx/query-262471afeb481ce8dcf1e27b34f777b8ed2e7b39cac0d2cc564c317431a58e2e.json
\echo '### feed'
PREPARE q_262471afeb48 AS
WITH unread AS ( SELECT s.id AS series_id, s.canonical_title AS series_title, c.number_milli, c.title AS chapter_title, p.slug AS provider_slug, p.base_url AS base_url, chapter_url_path(ss.source_path, c.path) AS chapter_path, min(c.discovered_at) OVER (PARTITION BY s.id, c.number_milli) AS discovered_at, row_number() OVER (PARTITION BY s.id, c.number_milli ORDER BY ss.chapter_count DESC, ss.last_scanned_at DESC NULLS LAST, p.slug, ss.id) AS carrier_rank FROM watchlist_entries w JOIN series s ON s.id = w.series_id JOIN series_sources ss ON ss.series_id = w.series_id JOIN providers p ON p.id = ss.provider_id JOIN chapters c ON c.series_source_id = ss.id LEFT JOIN read_progress rp ON rp.user_id = w.user_id AND rp.series_id = w.series_id WHERE w.user_id = $1 AND c.number_milli >= (floor(COALESCE(rp.last_read_whole_number, 0))::bigint + 1) * 10000 AND NOT (c.number_milli % 10000 <> 0 AND rp.last_read_part_number IS NOT NULL AND c.number_milli <= (rp.last_read_part_number * 10000)::bigint) AND (c.access = 'free' OR c.unlocks_at <= now() OR ss.provider_id = ANY(ARRAY( SELECT e.provider_id FROM user_provider_early_access e WHERE e.user_id = $1))) ) SELECT series_id AS "series_id!", series_title AS "series_title!", number_milli AS "chapter_number!", chapter_title, provider_slug AS "provider_slug!", base_url AS "base_url!", chapter_path AS "chapter_path!", discovered_at AS "discovered_at!" FROM unread WHERE carrier_rank = 1 ORDER BY discovered_at DESC, series_id, number_milli DESC LIMIT $2;
\echo '--- feed run 1 (cache as found)'
EXPLAIN (ANALYZE, BUFFERS, SETTINGS) EXECUTE q_262471afeb48(:'uid', 100);
\echo '--- feed run 2 (warm)'
EXPLAIN (ANALYZE, BUFFERS, SETTINGS) EXECUTE q_262471afeb48(:'uid', 100);
\echo '--- feed generic plan (what a connection runs after 5 executions)'
SET plan_cache_mode = force_generic_plan;
EXPLAIN (ANALYZE, BUFFERS) EXECUTE q_262471afeb48(:'uid', 100);
RESET plan_cache_mode;

-- ---------------------------------------------------------------------------
-- me_stats: crates/db/src/repo/tracking/dashboard.rs::me_stats
-- .sqlx/query-3f7e8b4434c911bca7af01ebbda9fdefaa690542dca7d86ae63aab8230a6a978.json
\echo '### me_stats'
PREPARE q_3f7e8b4434c9 AS
WITH watched AS ( SELECT count(*) AS tracking, count(*) FILTER (WHERE status = 'reading') AS reading, count(*) FILTER (WHERE status = 'completed') AS completed FROM watchlist_entries WHERE user_id = $1 ) SELECT (SELECT tracking FROM watched) AS "tracking!", (SELECT reading FROM watched) AS "reading!", (SELECT completed FROM watched) AS "completed!", (SELECT COALESCE(sum(floor(last_read_whole_number)),0)::int8 FROM read_progress WHERE user_id = $1) AS "chapters_read!", (SELECT COALESCE(sum(agg.unread),0)::int8 FROM watchlist_entries w LEFT JOIN read_progress rp ON rp.user_id = w.user_id AND rp.series_id = w.series_id CROSS JOIN LATERAL ( SELECT count(DISTINCT c.number_milli / 10000) AS unread FROM series_sources ss JOIN chapters c ON c.series_source_id = ss.id WHERE ss.series_id = w.series_id AND c.number_milli >= (floor(COALESCE(rp.last_read_whole_number, 0))::bigint + 1) * 10000 AND NOT (c.number_milli % 10000 <> 0 AND rp.last_read_part_number IS NOT NULL AND c.number_milli <= (rp.last_read_part_number * 10000)::bigint) AND (c.access = 'free' OR c.unlocks_at <= now() OR ss.provider_id = ANY(ARRAY( SELECT e.provider_id FROM user_provider_early_access e WHERE e.user_id = $1))) ) agg WHERE w.user_id = $1) AS "unread!";
\echo '--- me_stats run 1 (cache as found)'
EXPLAIN (ANALYZE, BUFFERS, SETTINGS) EXECUTE q_3f7e8b4434c9(:'uid');
\echo '--- me_stats run 2 (warm)'
EXPLAIN (ANALYZE, BUFFERS, SETTINGS) EXECUTE q_3f7e8b4434c9(:'uid');
\echo '--- me_stats generic plan (what a connection runs after 5 executions)'
SET plan_cache_mode = force_generic_plan;
EXPLAIN (ANALYZE, BUFFERS) EXECUTE q_3f7e8b4434c9(:'uid');
RESET plan_cache_mode;

-- ---------------------------------------------------------------------------
-- watchlist_summary: crates/db/src/repo/tracking/watchlist/summary.rs::watchlist_summary
-- .sqlx/query-ef3514bf4b3e11de3c476041ed05ab55374dfbfa78dcc06fd1ff19c4063aabe3.json
\echo '### watchlist_summary'
PREPARE q_ef3514bf4b3e AS
SELECT w.status AS "status!: WatchStatus", count(*) AS "n!", count(*) FILTER (WHERE src.source_degraded) AS "degraded!", COALESCE(sum(ch.unread), 0)::int8 AS "unread!" FROM watchlist_entries w LEFT JOIN read_progress rp ON rp.user_id = w.user_id AND rp.series_id = w.series_id CROSS JOIN LATERAL ( SELECT COALESCE(count(DISTINCT c.number_milli / 10000), 0) AS unread FROM series_sources ss JOIN chapters c ON c.series_source_id = ss.id WHERE ss.series_id = w.series_id AND c.number_milli >= (floor(COALESCE(rp.last_read_whole_number, 0))::bigint + 1) * 10000 AND NOT (c.number_milli % 10000 <> 0 AND rp.last_read_part_number IS NOT NULL AND c.number_milli <= (rp.last_read_part_number * 10000)::bigint) AND (c.access = 'free' OR c.unlocks_at <= now() OR ss.provider_id = ANY(ARRAY( SELECT e.provider_id FROM user_provider_early_access e WHERE e.user_id = $1))) ) ch CROSS JOIN LATERAL ( SELECT COALESCE((array_agg(ss.state <> 'active' OR p.state <> 'active' ORDER BY ss.chapter_count DESC, ss.last_scanned_at DESC NULLS LAST, p.slug))[1], false) AS source_degraded FROM series_sources ss JOIN providers p ON p.id = ss.provider_id WHERE ss.series_id = w.series_id ) src WHERE w.user_id = $1 GROUP BY w.status;
\echo '--- watchlist_summary run 1 (cache as found)'
EXPLAIN (ANALYZE, BUFFERS, SETTINGS) EXECUTE q_ef3514bf4b3e(:'uid');
\echo '--- watchlist_summary run 2 (warm)'
EXPLAIN (ANALYZE, BUFFERS, SETTINGS) EXECUTE q_ef3514bf4b3e(:'uid');
\echo '--- watchlist_summary generic plan (what a connection runs after 5 executions)'
SET plan_cache_mode = force_generic_plan;
EXPLAIN (ANALYZE, BUFFERS) EXECUTE q_ef3514bf4b3e(:'uid');
RESET plan_cache_mode;

-- ---------------------------------------------------------------------------
-- providers_public: crates/db/src/repo/providers.rs::list_public
-- .sqlx/query-8ca72e8868012c03efa72f22a3ea54659110beadd09698d53d4d0680349d7329.json
\echo '### providers_public'
PREPARE q_8ca72e886801 AS
SELECT p.id, p.slug, p.name, (SELECT count(DISTINCT ss.series_id) FROM series_sources ss WHERE ss.provider_id = p.id) AS "series_count!" FROM providers p WHERE p.state <> 'disabled' ORDER BY 4 DESC, p.name ASC;
\echo '--- providers_public run 1 (cache as found)'
EXPLAIN (ANALYZE, BUFFERS, SETTINGS) EXECUTE q_8ca72e886801;
\echo '--- providers_public run 2 (warm)'
EXPLAIN (ANALYZE, BUFFERS, SETTINGS) EXECUTE q_8ca72e886801;

-- ---------------------------------------------------------------------------
-- stats_overview: crates/db/src/repo/stats.rs::system_overview
-- .sqlx/query-9c2d2ba862e2c0ed00cb858d69d78e91e85f9e4d17cdb5380c534a4d4b765731.json
\echo '### stats_overview'
PREPARE q_9c2d2ba862e2 AS
SELECT (SELECT count(*) FROM providers) AS "providers_total!", (SELECT count(*) FROM providers WHERE state = 'active') AS "providers_active!", (SELECT count(*) FROM providers WHERE state = 'disabled') AS "providers_disabled!", (SELECT count(*) FROM providers WHERE state IN ('degraded','challenged','solving','blocked')) AS "providers_unhealthy!", (SELECT count(*) FROM series) AS "series_total!", (SELECT count(*) FROM series_sources) AS "sources_total!", (SELECT count(*) FROM chapters) AS "chapters_total!", (SELECT count(*) FROM chapters WHERE discovered_at > now() - interval '1 hour') AS "chapters_1h!", (SELECT count(*) FROM chapters WHERE discovered_at > now() - interval '24 hours') AS "chapters_24h!", (SELECT count(*) FROM chapters WHERE discovered_at > now() - interval '7 days') AS "chapters_7d!", (SELECT count(*) FROM users) AS "users_total!", (SELECT count(*) FROM merge_candidates WHERE NOT resolved) AS "pending_merges!", (SELECT count(*) FROM scan_runs WHERE state IN ('queued','running')) AS "runs_active!", (SELECT count(*) FROM scan_runs WHERE state = 'running') AS "runs_running!", (SELECT count(*) FROM scan_tasks WHERE state = 'queued') AS "tasks_queued!", (SELECT count(*) FROM scan_tasks WHERE state IN ('claimed','running')) AS "tasks_running!", (SELECT count(*) FROM scan_tasks WHERE state = 'failed' AND finished_at > now() - interval '24 hours') AS "tasks_failed_24h!";
\echo '--- stats_overview run 1 (cache as found)'
EXPLAIN (ANALYZE, BUFFERS, SETTINGS) EXECUTE q_9c2d2ba862e2;
\echo '--- stats_overview run 2 (warm)'
EXPLAIN (ANALYZE, BUFFERS, SETTINGS) EXECUTE q_9c2d2ba862e2;

-- ---------------------------------------------------------------------------
-- provider_health: crates/db/src/repo/stats.rs::provider_stats
-- .sqlx/query-92c51d6e14157c9ba21ddf355a58ea23d49b20eab61fbfd18775506827cc4875.json
\echo '### provider_health'
PREPARE q_92c51d6e1415 AS
WITH src AS ( SELECT provider_id, count(*) AS source_count, count(DISTINCT series_id) AS series_count, count(*) FILTER (WHERE state <> 'active') AS blocked_sources, max(last_scanned_at) AS last_scanned_at FROM series_sources GROUP BY provider_id ), ch AS ( SELECT ss.provider_id, count(*) AS chapter_count, count(*) FILTER (WHERE c.discovered_at > now() - interval '24 hours') AS chapters_24h, count(*) FILTER (WHERE c.discovered_at > now() - interval '7 days') AS chapters_7d, max(c.discovered_at) AS last_chapter_at FROM series_sources ss JOIN chapters c ON c.series_source_id = ss.id GROUP BY ss.provider_id ), lr AS ( SELECT DISTINCT ON (provider_id) provider_id, state AS run_state, created_at AS run_at FROM scan_runs WHERE provider_id IS NOT NULL ORDER BY provider_id, created_at DESC ) SELECT p.id AS provider_id, p.slug AS slug, p.name AS name, p.state::text AS "state!", p.adapter::text AS "adapter!", COALESCE(src.series_count, 0) AS "series_count!", COALESCE(src.source_count, 0) AS "source_count!", COALESCE(src.blocked_sources, 0) AS "blocked_sources!", COALESCE(ch.chapter_count, 0) AS "chapter_count!", COALESCE(ch.chapters_24h, 0) AS "chapters_24h!", COALESCE(ch.chapters_7d, 0) AS "chapters_7d!", ch.last_chapter_at AS "last_chapter_at?", src.last_scanned_at AS "last_scanned_at?", p.last_full_scan_at, lr.run_state::text AS "last_run_state?", lr.run_at AS "last_run_at?" FROM providers p LEFT JOIN src ON src.provider_id = p.id LEFT JOIN ch  ON ch.provider_id  = p.id LEFT JOIN lr  ON lr.provider_id  = p.id ORDER BY 9 DESC, p.name ASC;
\echo '--- provider_health run 1 (cache as found)'
EXPLAIN (ANALYZE, BUFFERS, SETTINGS) EXECUTE q_92c51d6e1415;
\echo '--- provider_health run 2 (warm)'
EXPLAIN (ANALYZE, BUFFERS, SETTINGS) EXECUTE q_92c51d6e1415;

-- ---------------------------------------------------------------------------
-- browse_count: crates/db/src/repo/catalog/browse.rs::count_filtered (unfiltered Discover)
-- .sqlx/query-eb51848454fa512a84716ce4d820fe8b55b72a7ee28697111f7bed6646818861.json
\echo '### browse_count'
PREPARE q_eb51848454fa AS
SELECT count(*) AS "total!" FROM series s WHERE ($1::content_type IS NULL OR s.content_type = $1) AND ($2::series_status IS NULL OR s.status = $2) AND ($3::int IS NULL OR s.release_year >= $3) AND ($4::int IS NULL OR s.release_year <= $4) AND ($5::text IS NULL OR EXISTS ( SELECT 1 FROM series_sources ss JOIN providers p ON p.id = ss.provider_id WHERE ss.series_id = s.id AND p.slug = $5)) AND ($6::int IS NULL OR ( SELECT COALESCE(max(ss.chapter_count),0) FROM series_sources ss WHERE ss.series_id = s.id) >= $6) AND (cardinality($7::text[]) = 0 OR NOT EXISTS ( SELECT unnest($7::text[]) EXCEPT SELECT t.slug FROM series_tags stg JOIN tags t ON t.id = stg.tag_id WHERE stg.series_id = s.id)) AND (cardinality($8::text[]) = 0 OR NOT EXISTS ( SELECT 1 FROM series_tags stg JOIN tags t ON t.id = stg.tag_id WHERE stg.series_id = s.id AND t.slug = ANY($8::text[]))) AND (NOT s.adult_gated OR $9) AND ($10::uuid IS NULL OR $11::bool IS NULL OR $11 = EXISTS ( SELECT 1 FROM watchlist_entries w WHERE w.series_id = s.id AND w.user_id = $10));
\echo '--- browse_count run 1 (cache as found)'
EXPLAIN (ANALYZE, BUFFERS, SETTINGS) EXECUTE q_eb51848454fa(NULL, NULL, NULL, NULL, NULL, NULL, '{}', '{}', false, NULL, NULL);
\echo '--- browse_count run 2 (warm)'
EXPLAIN (ANALYZE, BUFFERS, SETTINGS) EXECUTE q_eb51848454fa(NULL, NULL, NULL, NULL, NULL, NULL, '{}', '{}', false, NULL, NULL);
\echo '--- browse_count generic plan (what a connection runs after 5 executions)'
SET plan_cache_mode = force_generic_plan;
EXPLAIN (ANALYZE, BUFFERS) EXECUTE q_eb51848454fa(NULL, NULL, NULL, NULL, NULL, NULL, '{}', '{}', false, NULL, NULL);
RESET plan_cache_mode;

-- ---------------------------------------------------------------------------
-- browse_page_recent: crates/db/src/repo/catalog/browse.rs::fetch_page_by_recency (unfiltered, page 1)
-- .sqlx/query-36494e9da4333f95c78698f03f72ae0744918913bdcc8cc3360c2917f9f5e518.json
\echo '### browse_page_recent'
PREPARE q_36494e9da433 AS
SELECT s.id, s.canonical_title, s.normalized_title, s.description, s.cover_url, s.content_type AS "content_type: ContentType", s.status AS "status: SeriesStatus", s.release_year, s.created_at, s.updated_at, (SELECT count(DISTINCT ss.provider_id) FROM series_sources ss WHERE ss.series_id = s.id) AS "source_count!" FROM series s WHERE ($1::content_type IS NULL OR s.content_type = $1) AND ($2::series_status IS NULL OR s.status = $2) AND ($3::int IS NULL OR s.release_year >= $3) AND ($4::int IS NULL OR s.release_year <= $4) AND ($5::text IS NULL OR EXISTS ( SELECT 1 FROM series_sources ss JOIN providers p ON p.id = ss.provider_id WHERE ss.series_id = s.id AND p.slug = $5)) AND ($6::int IS NULL OR ( SELECT COALESCE(max(ss.chapter_count),0) FROM series_sources ss WHERE ss.series_id = s.id) >= $6) AND (cardinality($7::text[]) = 0 OR NOT EXISTS ( SELECT unnest($7::text[]) EXCEPT SELECT t.slug FROM series_tags stg JOIN tags t ON t.id = stg.tag_id WHERE stg.series_id = s.id)) AND (cardinality($8::text[]) = 0 OR NOT EXISTS ( SELECT 1 FROM series_tags stg JOIN tags t ON t.id = stg.tag_id WHERE stg.series_id = s.id AND t.slug = ANY($8::text[]))) AND (NOT s.adult_gated OR $9) AND ($10::uuid IS NULL OR $11::bool IS NULL OR $11 = EXISTS ( SELECT 1 FROM watchlist_entries w WHERE w.series_id = s.id AND w.user_id = $10)) ORDER BY s.updated_at DESC, s.id DESC LIMIT $12 OFFSET $13;
\echo '--- browse_page_recent run 1 (cache as found)'
EXPLAIN (ANALYZE, BUFFERS, SETTINGS) EXECUTE q_36494e9da433(NULL, NULL, NULL, NULL, NULL, NULL, '{}', '{}', false, NULL, NULL, 48, 0);
\echo '--- browse_page_recent run 2 (warm)'
EXPLAIN (ANALYZE, BUFFERS, SETTINGS) EXECUTE q_36494e9da433(NULL, NULL, NULL, NULL, NULL, NULL, '{}', '{}', false, NULL, NULL, 48, 0);
\echo '--- browse_page_recent generic plan (what a connection runs after 5 executions)'
SET plan_cache_mode = force_generic_plan;
EXPLAIN (ANALYZE, BUFFERS) EXECUTE q_36494e9da433(NULL, NULL, NULL, NULL, NULL, NULL, '{}', '{}', false, NULL, NULL, 48, 0);
RESET plan_cache_mode;

-- ---------------------------------------------------------------------------
-- merge_decisions: crates/db/src/repo/matching/decisions.rs::list_merge_decisions (default page)
-- .sqlx/query-f670e90b2e715cfe53b8ccb9ff65ba4ef63031ddd5686dea1124498d48ddab88.json
\echo '### merge_decisions'
PREPARE q_f670e90b2e71 AS
SELECT d.id, d.decided_at, d.sweep_id, d.trigger, d.actor_id AS actor, d.left_id, d.right_id, d.left_title, d.right_title, d.verdict, d.reason, d.blocked_by, d.outcome, d.survivor_id, d.absorbed_id, d.score, d.base_score, d.signals, d.terms, d.evidence, d.policy, (d.undo IS NOT NULL AND d.reverted_at IS NULL) AS "revertible!", u.undo_rows AS "undo_rows!", u.undo_breakdown AS "undo_breakdown!", d.reverted_at, d.reverted_by, d.revert_reason, d.flagged_at, d.flagged_by, d.flag_reason FROM merge_decisions d CROSS JOIN LATERAL ( SELECT COALESCE(sum(s.rows), 0)::bigint AS undo_rows, COALESCE(jsonb_agg( jsonb_build_object('kind', s.kind, 'rows', s.rows) ORDER BY s.rows DESC, s.kind ) FILTER (WHERE s.rows > 0), '[]'::jsonb) AS undo_breakdown FROM ( SELECT e.k AS kind, CASE WHEN jsonb_typeof(e.v) = 'array' THEN jsonb_array_length(e.v) ELSE 0 END AS rows FROM jsonb_each(COALESCE(d.undo, '{}'::jsonb)) AS e(k, v) ) AS s ) AS u WHERE ($3::text IS NULL OR d.outcome = $3) AND ($4::uuid IS NULL OR d.left_id = $4 OR d.right_id = $4) AND (NOT $5::boolean OR (d.undo IS NOT NULL AND d.reverted_at IS NULL)) AND (NOT $6::boolean OR d.flagged_at IS NOT NULL) AND (NOT $7::boolean OR cardinality(d.blocked_by) > 0) ORDER BY d.decided_at DESC LIMIT $1 OFFSET $2;
\echo '--- merge_decisions run 1 (cache as found)'
EXPLAIN (ANALYZE, BUFFERS, SETTINGS) EXECUTE q_f670e90b2e71(50, 0, NULL, NULL, false, false, false);
\echo '--- merge_decisions run 2 (warm)'
EXPLAIN (ANALYZE, BUFFERS, SETTINGS) EXECUTE q_f670e90b2e71(50, 0, NULL, NULL, false, false, false);
\echo '--- merge_decisions generic plan (what a connection runs after 5 executions)'
SET plan_cache_mode = force_generic_plan;
EXPLAIN (ANALYZE, BUFFERS) EXECUTE q_f670e90b2e71(50, 0, NULL, NULL, false, false, false);
RESET plan_cache_mode;

-- ---------------------------------------------------------------------------
-- merge_decisions_flagged: crates/db/src/repo/matching/decisions.rs::list_merge_decisions (flagged only)
-- .sqlx/query-f670e90b2e715cfe53b8ccb9ff65ba4ef63031ddd5686dea1124498d48ddab88.json
\echo '### merge_decisions_flagged'
\echo '--- merge_decisions_flagged run 1 (cache as found)'
EXPLAIN (ANALYZE, BUFFERS, SETTINGS) EXECUTE q_f670e90b2e71(50, 0, NULL, NULL, false, true, false);
\echo '--- merge_decisions_flagged run 2 (warm)'
EXPLAIN (ANALYZE, BUFFERS, SETTINGS) EXECUTE q_f670e90b2e71(50, 0, NULL, NULL, false, true, false);
\echo '--- merge_decisions_flagged generic plan (what a connection runs after 5 executions)'
SET plan_cache_mode = force_generic_plan;
EXPLAIN (ANALYZE, BUFFERS) EXECUTE q_f670e90b2e71(50, 0, NULL, NULL, false, true, false);
RESET plan_cache_mode;

-- ---------------------------------------------------------------------------
-- scan_summary: crates/db/src/repo/scans.rs::scan_summary (no provider, no window)
-- .sqlx/query-e84b2d3150a3c716ca94ccd408323481071df95a5f61402216ca301c9affb748.json
\echo '### scan_summary'
PREPARE q_e84b2d3150a3 AS
WITH matched AS ( SELECT r.state, r.total_tasks, r.done_tasks, r.failed_tasks, r.started_at, r.finished_at, r.created_at FROM scan_runs r LEFT JOIN providers p ON p.id = r.provider_id WHERE ($1::text IS NULL OR p.slug = $1) AND ($2::timestamptz IS NULL OR r.created_at >= $2) ), open AS ( SELECT count(*) AS n FROM scan_tasks t JOIN scan_runs r ON r.id = t.run_id LEFT JOIN providers p ON p.id = r.provider_id WHERE t.state = 'failed' AND t.acknowledged_at IS NULL AND ($1::text IS NULL OR p.slug = $1) AND ($2::timestamptz IS NULL OR t.finished_at >= $2) ) SELECT count(*) AS "runs_total!", count(*) FILTER (WHERE state = 'queued') AS "runs_queued!", count(*) FILTER (WHERE state = 'running') AS "runs_running!", count(*) FILTER (WHERE state = 'completed') AS "runs_completed!", count(*) FILTER (WHERE state = 'failed') AS "runs_failed!", count(*) FILTER (WHERE state = 'cancelled') AS "runs_cancelled!", COALESCE(sum(total_tasks), 0) AS "tasks_total!", COALESCE(sum(done_tasks), 0) AS "tasks_done!", COALESCE(sum(failed_tasks), 0) AS "tasks_failed!", (SELECT n FROM open) AS "failures_open!", COALESCE(sum(EXTRACT(EPOCH FROM (COALESCE(finished_at, now()) - started_at))), 0) ::float8 AS "busy_seconds!", min(created_at) AS first_run_at, max(created_at) AS last_run_at FROM matched;
\echo '--- scan_summary run 1 (cache as found)'
EXPLAIN (ANALYZE, BUFFERS, SETTINGS) EXECUTE q_e84b2d3150a3(NULL, NULL);
\echo '--- scan_summary run 2 (warm)'
EXPLAIN (ANALYZE, BUFFERS, SETTINGS) EXECUTE q_e84b2d3150a3(NULL, NULL);
\echo '--- scan_summary generic plan (what a connection runs after 5 executions)'
SET plan_cache_mode = force_generic_plan;
EXPLAIN (ANALYZE, BUFFERS) EXECUTE q_e84b2d3150a3(NULL, NULL);
RESET plan_cache_mode;

-- ---------------------------------------------------------------------------
-- provider_scan_health: crates/db/src/repo/scans.rs::provider_scan_health (no window)
-- .sqlx/query-d1383787d154456f9ddd23840fe8db4bacfada258e0ba2f89b00813c58d99d61.json
\echo '### provider_scan_health'
PREPARE q_d1383787d154 AS
SELECT p.slug, p.name, count(r.id) AS "runs!", count(r.id) FILTER (WHERE r.state IN ('queued','running')) AS "runs_active!", count(r.id) FILTER (WHERE r.state = 'failed') AS "runs_failed!", COALESCE(sum(r.done_tasks), 0) AS "tasks_done!", COALESCE(sum(r.failed_tasks), 0) AS "tasks_failed!", COALESCE(f.open_count, 0) AS "failures_open!", max(r.created_at) AS last_run_at, f.latest_at AS last_failure_at FROM providers p LEFT JOIN scan_runs r ON r.provider_id = p.id AND ($1::timestamptz IS NULL OR r.created_at >= $1) LEFT JOIN LATERAL ( SELECT count(*) AS open_count, max(t.finished_at) AS latest_at FROM scan_tasks t JOIN scan_runs r2 ON r2.id = t.run_id WHERE r2.provider_id = p.id AND t.state = 'failed' AND t.acknowledged_at IS NULL AND ($1::timestamptz IS NULL OR t.finished_at >= $1) ) f ON true GROUP BY p.id, p.slug, p.name, f.open_count, f.latest_at HAVING count(r.id) > 0 OR COALESCE(f.open_count, 0) > 0 ORDER BY COALESCE(f.open_count, 0) DESC, COALESCE(sum(r.failed_tasks), 0) DESC, max(r.created_at) DESC NULLS LAST, p.slug LIMIT $2;
\echo '--- provider_scan_health run 1 (cache as found)'
EXPLAIN (ANALYZE, BUFFERS, SETTINGS) EXECUTE q_d1383787d154(NULL, 50);
\echo '--- provider_scan_health run 2 (warm)'
EXPLAIN (ANALYZE, BUFFERS, SETTINGS) EXECUTE q_d1383787d154(NULL, 50);
\echo '--- provider_scan_health generic plan (what a connection runs after 5 executions)'
SET plan_cache_mode = force_generic_plan;
EXPLAIN (ANALYZE, BUFFERS) EXECUTE q_d1383787d154(NULL, 50);
RESET plan_cache_mode;

-- ---------------------------------------------------------------------------
-- active_run_activity: crates/db/src/repo/scans.rs::active_run_activity
-- .sqlx/query-da2a4748fb46c7a5896ca30d1e63fafecd77d04c083c341ee4010cd5c24bae63.json
\echo '### active_run_activity'
PREPARE q_da2a4748fb46 AS
SELECT r.id AS "run_id!", count(t.id) FILTER (WHERE t.state = 'queued') AS "queued_tasks!", count(t.id) FILTER (WHERE t.state = 'claimed') AS "running_tasks!", min(t.claimed_at) FILTER (WHERE t.state = 'claimed') AS oldest_claim_at, COALESCE( array_agg(DISTINCT t.kind) FILTER (WHERE t.state = 'claimed'), ARRAY[]::text[] ) AS "kinds!", count(DISTINCT t.worker_id) FILTER (WHERE t.state = 'claimed') AS "workers!", s.stage, s.stage_at, s.stage_done, s.stage_total, s.stage_detail, min(t.created_at) FILTER (WHERE t.state = 'queued') AS waiting_since FROM scan_runs r LEFT JOIN scan_tasks t ON t.run_id = r.id LEFT JOIN LATERAL ( SELECT h.stage, h.stage_at, h.stage_done, h.stage_total, h.stage_detail FROM scan_tasks h WHERE h.run_id = r.id AND h.state = 'claimed' ORDER BY h.claimed_at LIMIT 1 ) s ON true WHERE r.state IN ('queued','running') GROUP BY r.id, s.stage, s.stage_at, s.stage_done, s.stage_total, s.stage_detail;
\echo '--- active_run_activity run 1 (cache as found)'
EXPLAIN (ANALYZE, BUFFERS, SETTINGS) EXECUTE q_da2a4748fb46;
\echo '--- active_run_activity run 2 (warm)'
EXPLAIN (ANALYZE, BUFFERS, SETTINGS) EXECUTE q_da2a4748fb46;

-- ---------------------------------------------------------------------------
-- failed_tasks: crates/db/src/repo/scans.rs::failed_tasks_filtered (default page)
-- .sqlx/query-133958ede2699af691e6f247f0d2e1105a5a829653d4b5a6f93527856b711fda.json
\echo '### failed_tasks'
PREPARE q_133958ede269 AS
SELECT t.id, t.run_id, p.slug AS "provider_slug?", r.mode::text AS "mode!", t.kind, t.error, t.attempts, t.finished_at, t.acknowledged_at FROM scan_tasks t JOIN scan_runs r ON r.id = t.run_id LEFT JOIN providers p ON p.id = r.provider_id WHERE t.state = 'failed' AND ($1::text IS NULL OR p.slug = $1) AND ($2::timestamptz IS NULL OR t.finished_at >= $2) AND ($3::bool OR t.acknowledged_at IS NULL) ORDER BY t.finished_at DESC NULLS LAST LIMIT $4;
\echo '--- failed_tasks run 1 (cache as found)'
EXPLAIN (ANALYZE, BUFFERS, SETTINGS) EXECUTE q_133958ede269(NULL, NULL, false, 25);
\echo '--- failed_tasks run 2 (warm)'
EXPLAIN (ANALYZE, BUFFERS, SETTINGS) EXECUTE q_133958ede269(NULL, NULL, false, 25);
\echo '--- failed_tasks generic plan (what a connection runs after 5 executions)'
SET plan_cache_mode = force_generic_plan;
EXPLAIN (ANALYZE, BUFFERS) EXECUTE q_133958ede269(NULL, NULL, false, 25);
RESET plan_cache_mode;

-- ---------------------------------------------------------------------------
-- failure_groups: crates/db/src/repo/scans.rs::failure_groups (default page)
-- .sqlx/query-be2b8afcd02a47e38ae5ca8a50b6f0d873add387e3189bf6da46c763f07adefd.json
\echo '### failure_groups'
PREPARE q_be2b8afcd02a AS
SELECT t.error, count(*) AS "count!", count(*) FILTER (WHERE t.acknowledged_at IS NOT NULL) AS "cleared!", array_remove(array_agg(DISTINCT p.slug), NULL) AS "providers!", array_agg(DISTINCT t.kind) AS "kinds!", max(t.finished_at) AS latest_at FROM scan_tasks t JOIN scan_runs r ON r.id = t.run_id LEFT JOIN providers p ON p.id = r.provider_id WHERE t.state = 'failed' AND ($1::text IS NULL OR p.slug = $1) AND ($2::timestamptz IS NULL OR t.finished_at >= $2) AND ($3::bool OR t.acknowledged_at IS NULL) GROUP BY t.error ORDER BY count(*) DESC, max(t.finished_at) DESC NULLS LAST LIMIT $4;
\echo '--- failure_groups run 1 (cache as found)'
EXPLAIN (ANALYZE, BUFFERS, SETTINGS) EXECUTE q_be2b8afcd02a(NULL, NULL, false, 25);
\echo '--- failure_groups run 2 (warm)'
EXPLAIN (ANALYZE, BUFFERS, SETTINGS) EXECUTE q_be2b8afcd02a(NULL, NULL, false, 25);
\echo '--- failure_groups generic plan (what a connection runs after 5 executions)'
SET plan_cache_mode = force_generic_plan;
EXPLAIN (ANALYZE, BUFFERS) EXECUTE q_be2b8afcd02a(NULL, NULL, false, 25);
RESET plan_cache_mode;

-- ---------------------------------------------------------------------------
-- scan_runs_list: crates/db/src/repo/scans.rs::list_runs_filtered (no filter)
-- .sqlx/query-4382a963c0e8f30a160fc5b7219ad7acabe1865e326280bae0e267113638635d.json
\echo '### scan_runs_list'
PREPARE q_4382a963c0e8 AS
WITH matched AS ( SELECT r.*, p.slug AS provider_slug FROM scan_runs r LEFT JOIN providers p ON p.id = r.provider_id WHERE ($1::text IS NULL OR p.slug = $1) AND ($2::scan_mode IS NULL OR r.mode = $2) AND ($3::run_state IS NULL OR r.state = $3) AND ($4::timestamptz IS NULL OR r.created_at >= $4) ) SELECT m.id, m.provider_id, m.provider_slug AS "provider_slug?", m.mode AS "mode: ScanMode", m.state AS "state: RunState", m.total_tasks, m.done_tasks, m.failed_tasks, m.started_at, m.finished_at, m.created_at, (SELECT count(*) FROM matched) AS "total!" FROM matched m ORDER BY CASE WHEN $5::text = 'failures' THEN m.failed_tasks END DESC NULLS LAST, CASE WHEN $5::text = 'duration' THEN EXTRACT(EPOCH FROM (COALESCE(m.finished_at, now()) - m.started_at)) END DESC NULLS LAST, CASE WHEN $5::text = 'oldest' THEN m.created_at END ASC, m.created_at DESC LIMIT $6 OFFSET $7;
\echo '--- scan_runs_list run 1 (cache as found)'
EXPLAIN (ANALYZE, BUFFERS, SETTINGS) EXECUTE q_4382a963c0e8(NULL, NULL, NULL, NULL, 'newest', 50, 0);
\echo '--- scan_runs_list run 2 (warm)'
EXPLAIN (ANALYZE, BUFFERS, SETTINGS) EXECUTE q_4382a963c0e8(NULL, NULL, NULL, NULL, 'newest', 50, 0);
\echo '--- scan_runs_list generic plan (what a connection runs after 5 executions)'
SET plan_cache_mode = force_generic_plan;
EXPLAIN (ANALYZE, BUFFERS) EXECUTE q_4382a963c0e8(NULL, NULL, NULL, NULL, 'newest', 50, 0);
RESET plan_cache_mode;

DEALLOCATE ALL;
\endif

\echo '################ done'
