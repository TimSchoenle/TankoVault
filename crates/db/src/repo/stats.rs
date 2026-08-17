//! Operator-console read models (design §17.2.7): system-wide rollups and per-provider
//! crawl statistics. These are read-only aggregates over the catalogue, scan, and user
//! tables, computed on demand for the admin dashboard — no denormalised counters to keep
//! in sync. Every query is a single static statement (`SQLx` 0.9 rejects non-`'static` SQL).

use crate::error::DbResult;
use serde::Serialize;
use sqlx::{FromRow, PgExecutor};
use time::OffsetDateTime;
use uuid::Uuid;

/// System-wide rollup for the console header (single-row query).
#[derive(Debug, Clone, Serialize, FromRow)]
pub struct SystemStats {
    pub providers_total: i64,
    pub providers_active: i64,
    pub providers_disabled: i64,
    /// Providers in a non-serving health state (degraded/challenged/solving/blocked).
    pub providers_unhealthy: i64,
    pub series_total: i64,
    pub sources_total: i64,
    pub chapters_total: i64,
    pub chapters_1h: i64,
    pub chapters_24h: i64,
    pub chapters_7d: i64,
    pub users_total: i64,
    pub pending_merges: i64,
    /// Scan runs currently queued or running.
    pub runs_active: i64,
    pub runs_running: i64,
    pub tasks_queued: i64,
    pub tasks_running: i64,
    pub tasks_failed_24h: i64,
}

/// One row of the per-provider statistics table. Enum columns are text-cast; the provider's
/// identity fields are joined in so the console renders the table from one fetch.
#[derive(Debug, Clone, Serialize, FromRow)]
pub struct ProviderStat {
    pub provider_id: Uuid,
    pub slug: String,
    pub name: String,
    /// Provider health state token (`active` | `disabled` | `blocked` | …).
    pub state: String,
    /// Adapter implementation token (`madara` | `generic_config` | `custom`).
    pub adapter: String,
    /// Distinct series that have at least one source on this provider.
    pub series_count: i64,
    /// Source links (series ↔ provider joins) this provider owns.
    pub source_count: i64,
    /// Source links currently in a non-active state.
    pub blocked_sources: i64,
    pub chapter_count: i64,
    pub chapters_24h: i64,
    pub chapters_7d: i64,
    #[serde(with = "time::serde::rfc3339::option")]
    pub last_chapter_at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub last_scanned_at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub last_full_scan_at: Option<OffsetDateTime>,
    /// State of the provider's most recent scan run, if any.
    pub last_run_state: Option<String>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub last_run_at: Option<OffsetDateTime>,
}

/// Compute the system-wide rollup for the console header.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only — no other variant is reachable. Every column is a
/// `count(*)` scalar subquery, so the row always exists and an empty database yields zeros
/// rather than [`crate::DbError::NotFound`].
pub async fn system_overview<'e, E: PgExecutor<'e>>(exec: E) -> DbResult<SystemStats> {
    let stats = sqlx::query_as!(
        SystemStats,
        "SELECT \
           (SELECT count(*) FROM providers) AS \"providers_total!\", \
           (SELECT count(*) FROM providers WHERE state = 'active') AS \"providers_active!\", \
           (SELECT count(*) FROM providers WHERE state = 'disabled') AS \"providers_disabled!\", \
           (SELECT count(*) FROM providers \
              WHERE state IN ('degraded','challenged','solving','blocked')) AS \"providers_unhealthy!\", \
           (SELECT count(*) FROM series) AS \"series_total!\", \
           (SELECT count(*) FROM series_sources) AS \"sources_total!\", \
           (SELECT count(*) FROM chapters) AS \"chapters_total!\", \
           (SELECT count(*) FROM chapters WHERE discovered_at > now() - interval '1 hour') AS \"chapters_1h!\", \
           (SELECT count(*) FROM chapters WHERE discovered_at > now() - interval '24 hours') AS \"chapters_24h!\", \
           (SELECT count(*) FROM chapters WHERE discovered_at > now() - interval '7 days') AS \"chapters_7d!\", \
           (SELECT count(*) FROM users) AS \"users_total!\", \
           (SELECT count(*) FROM merge_candidates WHERE NOT resolved) AS \"pending_merges!\", \
           (SELECT count(*) FROM scan_runs WHERE state IN ('queued','running')) AS \"runs_active!\", \
           (SELECT count(*) FROM scan_runs WHERE state = 'running') AS \"runs_running!\", \
           (SELECT count(*) FROM scan_tasks WHERE state = 'queued') AS \"tasks_queued!\", \
           (SELECT count(*) FROM scan_tasks WHERE state IN ('claimed','running')) AS \"tasks_running!\", \
           (SELECT count(*) FROM scan_tasks \
              WHERE state = 'failed' AND finished_at > now() - interval '24 hours') AS \"tasks_failed_24h!\"",
    )
    .fetch_one(exec)
    .await?;
    Ok(stats)
}

/// Per-provider crawl statistics, richest (most chapters) first. Providers with no sources
/// yet still appear with zeroed counts so newly-added ones are visible in the table.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only — no other variant is reachable.
pub async fn provider_stats<'e, E: PgExecutor<'e>>(exec: E) -> DbResult<Vec<ProviderStat>> {
    let rows = sqlx::query_as!(
        ProviderStat,
        "WITH src AS ( \
            SELECT provider_id, \
                   count(*) AS source_count, \
                   count(DISTINCT series_id) AS series_count, \
                   count(*) FILTER (WHERE state <> 'active') AS blocked_sources, \
                   max(last_scanned_at) AS last_scanned_at \
            FROM series_sources GROUP BY provider_id \
         ), ch AS ( \
            SELECT ss.provider_id, \
                   count(*) AS chapter_count, \
                   count(*) FILTER (WHERE c.discovered_at > now() - interval '24 hours') AS chapters_24h, \
                   count(*) FILTER (WHERE c.discovered_at > now() - interval '7 days') AS chapters_7d, \
                   max(c.discovered_at) AS last_chapter_at \
            FROM series_sources ss JOIN chapters c ON c.series_source_id = ss.id \
            GROUP BY ss.provider_id \
         ), lr AS ( \
            SELECT DISTINCT ON (provider_id) provider_id, \
                   state AS run_state, created_at AS run_at \
            FROM scan_runs WHERE provider_id IS NOT NULL \
            ORDER BY provider_id, created_at DESC \
         ) \
         SELECT p.id AS provider_id, p.slug AS slug, p.name AS name, \
                p.state::text AS \"state!\", p.adapter::text AS \"adapter!\", \
                COALESCE(src.series_count, 0) AS \"series_count!\", \
                COALESCE(src.source_count, 0) AS \"source_count!\", \
                COALESCE(src.blocked_sources, 0) AS \"blocked_sources!\", \
                COALESCE(ch.chapter_count, 0) AS \"chapter_count!\", \
                COALESCE(ch.chapters_24h, 0) AS \"chapters_24h!\", \
                COALESCE(ch.chapters_7d, 0) AS \"chapters_7d!\", \
                ch.last_chapter_at AS \"last_chapter_at?\", \
                src.last_scanned_at AS \"last_scanned_at?\", \
                p.last_full_scan_at, \
                lr.run_state::text AS \"last_run_state?\", \
                lr.run_at AS \"last_run_at?\" \
         FROM providers p \
         LEFT JOIN src ON src.provider_id = p.id \
         LEFT JOIN ch  ON ch.provider_id  = p.id \
         LEFT JOIN lr  ON lr.provider_id  = p.id \
         ORDER BY 9 DESC, p.name ASC",
    )
    .fetch_all(exec)
    .await?;
    Ok(rows)
}
