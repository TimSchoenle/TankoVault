//! The console header rollup and the per-provider crawl table (design §17.2.7).
//!
//! Read-only aggregates over the catalogue, scan and user tables, computed per request. There
//! is no denormalised counter to keep in sync, so a figure here cannot drift from the rows it
//! counts. Every query is a single static statement, which `SQLx` 0.9 requires.

use crate::error::DbResult;
use serde::Serialize;
use sqlx::{FromRow, PgExecutor};
use time::OffsetDateTime;
use uuid::Uuid;

/// System-wide rollup for the console header (single-row query).
#[derive(Debug, Clone, Serialize, FromRow)]
pub struct SystemStats {
    /// Every provider row, whatever its state.
    pub providers_total: i64,
    /// Providers in `active`.
    pub providers_active: i64,
    /// Providers an operator turned off.
    pub providers_disabled: i64,
    /// Providers in a non-serving health state (degraded/challenged/solving/blocked).
    pub providers_unhealthy: i64,
    /// Canonical series, not provider pages.
    pub series_total: i64,
    /// Series-to-provider links across every provider.
    pub sources_total: i64,
    /// Chapter links held, part releases counted separately.
    pub chapters_total: i64,
    /// Chapters first seen in the last hour, by `discovered_at` rather than publication date.
    pub chapters_1h: i64,
    /// Chapters first seen in the last 24 hours.
    pub chapters_24h: i64,
    /// Chapters first seen in the last 7 days.
    pub chapters_7d: i64,
    /// Registered accounts, suspended ones included.
    pub users_total: i64,
    /// Merge candidates nobody has resolved yet.
    pub pending_merges: i64,
    /// Scan runs currently queued or running.
    pub runs_active: i64,
    /// The subset of `runs_active` that has actually started.
    pub runs_running: i64,
    /// Tasks waiting for a worker across every run.
    pub tasks_queued: i64,
    /// Tasks claimed or fetching.
    pub tasks_running: i64,
    /// Tasks that failed in the last 24 hours, by the time they settled.
    pub tasks_failed_24h: i64,
}

/// One row of the per-provider statistics table. Enum columns are text-cast; the provider's
/// identity fields are joined in so the console renders the table from one fetch.
#[derive(Debug, Clone, Serialize, FromRow)]
pub struct ProviderStat {
    /// The provider these figures are for.
    pub provider_id: Uuid,
    /// Its slug, joined in so the table needs one fetch.
    pub slug: String,
    /// Its display name, which is also the tiebreak in the row order.
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
    /// Chapter links under this provider's sources.
    pub chapter_count: i64,
    /// Chapters first seen here in the last 24 hours.
    pub chapters_24h: i64,
    /// Chapters first seen here in the last 7 days.
    pub chapters_7d: i64,
    /// When a chapter was last discovered here, `None` for a provider with no chapters.
    #[serde(with = "time::serde::rfc3339::option")]
    pub last_chapter_at: Option<OffsetDateTime>,
    /// The most recent scan across this provider's sources, `None` until one finishes.
    #[serde(with = "time::serde::rfc3339::option")]
    pub last_scanned_at: Option<OffsetDateTime>,
    /// When a full archive rebuild last completed, `None` if none ever has.
    #[serde(with = "time::serde::rfc3339::option")]
    pub last_full_scan_at: Option<OffsetDateTime>,
    /// State of the provider's most recent scan run, if any.
    pub last_run_state: Option<String>,
    /// When the most recent run was created, `None` for a provider never scanned.
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
