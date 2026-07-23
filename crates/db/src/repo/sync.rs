//! External-sync persistence (design §15): OAuth accounts and canonical-series
//! mappings for a third-party provider such as `AniList`.
//!
//! Token columns hold **ciphertext only** — the sync service seals them with
//! [`tankovault_auth::SecretBox`] before they reach this layer, so nothing here ever handles
//! plaintext credentials. The `provider` column is the external service key (e.g.
//! `"anilist"`), mirroring the shape used by [`tracking`](super::tracking) entries.

use crate::error::DbResult;
use serde::Serialize;
use serde_json::Value as Json;
use sqlx::{FromRow, PgExecutor};
use tankovault_domain::{SeriesId, UserId};
use time::OffsetDateTime;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Three-way merge snapshot, conflicts and history (design v2 §B.2/§B.3)
// ---------------------------------------------------------------------------

/// The "common ancestor" snapshot recorded at the last successful reconciliation of a mapped
/// series, so the engine can tell which side(s) actually changed since (design v2 §B.3).
#[derive(Debug, Clone, FromRow)]
pub struct SyncSnapshot {
    pub last_synced_local_progress: Option<f64>,
    pub last_synced_remote_progress: Option<f64>,
    pub last_synced_local_status: Option<String>,
    pub last_synced_remote_status: Option<String>,
    pub last_synced_at: Option<OffsetDateTime>,
}

/// Fetch the stored three-way-merge snapshot for a mapped series, if any.
pub async fn get_snapshot<'e, E: PgExecutor<'e>>(
    exec: E,
    series_id: SeriesId,
    provider: &str,
) -> DbResult<Option<SyncSnapshot>> {
    let row = sqlx::query_as::<_, SyncSnapshot>(
        "SELECT last_synced_local_progress, last_synced_remote_progress, \
                last_synced_local_status, last_synced_remote_status, last_synced_at \
         FROM sync_mappings WHERE series_id = $1 AND provider = $2",
    )
    .bind(series_id.as_uuid())
    .bind(provider)
    .fetch_optional(exec)
    .await?;
    Ok(row)
}

/// Record the agreed values as the new three-way-merge snapshot after a reconciliation
/// (design v2 §B.3). The `sync_mappings` row must already exist.
#[allow(clippy::too_many_arguments)]
pub async fn record_snapshot<'e, E: PgExecutor<'e>>(
    exec: E,
    series_id: SeriesId,
    provider: &str,
    local_progress: f64,
    remote_progress: f64,
    local_status: &str,
    remote_status: &str,
) -> DbResult<()> {
    sqlx::query(
        "UPDATE sync_mappings SET \
            last_synced_local_progress  = $3, \
            last_synced_remote_progress = $4, \
            last_synced_local_status    = $5, \
            last_synced_remote_status   = $6, \
            last_synced_at              = now() \
         WHERE series_id = $1 AND provider = $2",
    )
    .bind(series_id.as_uuid())
    .bind(provider)
    .bind(local_progress)
    .bind(remote_progress)
    .bind(local_status)
    .bind(remote_status)
    .execute(exec)
    .await?;
    Ok(())
}

/// Queue a genuine, unresolved conflict for the `ask_me` policy (design v2 §B.3). Idempotent:
/// the unique partial index guarantees at most one pending row per
/// `(user, series, provider, field)`, so re-detection never double-queues.
#[allow(clippy::too_many_arguments)]
pub async fn insert_conflict<'e, E: PgExecutor<'e>>(
    exec: E,
    user_id: UserId,
    series_id: SeriesId,
    provider: &str,
    field: &str,
    local_value: &str,
    remote_value: &str,
) -> DbResult<()> {
    sqlx::query(
        "INSERT INTO sync_conflicts \
            (user_id, series_id, provider, field, local_value, remote_value) \
         VALUES ($1,$2,$3,$4,$5,$6) \
         ON CONFLICT (user_id, series_id, provider, field) WHERE resolved_at IS NULL \
         DO NOTHING",
    )
    .bind(user_id.as_uuid())
    .bind(series_id.as_uuid())
    .bind(provider)
    .bind(field)
    .bind(local_value)
    .bind(remote_value)
    .execute(exec)
    .await?;
    Ok(())
}

/// One pending conflict awaiting the user's decision (design v2 §B.6 `GET /v1/me/sync/conflicts`).
#[derive(Debug, Clone, Serialize, FromRow)]
pub struct ConflictRow {
    pub id: Uuid,
    pub series_id: Uuid,
    pub series_title: String,
    pub provider: String,
    pub field: String,
    pub local_value: String,
    pub remote_value: String,
    #[serde(with = "time::serde::rfc3339")]
    pub detected_at: OffsetDateTime,
}

/// Every pending (unresolved) conflict for a user, across all providers, newest first.
pub async fn list_pending_conflicts<'e, E: PgExecutor<'e>>(
    exec: E,
    user_id: UserId,
) -> DbResult<Vec<ConflictRow>> {
    let rows = sqlx::query_as::<_, ConflictRow>(
        "SELECT c.id, c.series_id, s.canonical_title AS series_title, c.provider, c.field, \
                c.local_value, c.remote_value, c.detected_at \
         FROM sync_conflicts c JOIN series s ON s.id = c.series_id \
         WHERE c.user_id = $1 AND c.resolved_at IS NULL \
         ORDER BY c.detected_at DESC",
    )
    .bind(user_id.as_uuid())
    .fetch_all(exec)
    .await?;
    Ok(rows)
}

/// A pending conflict's identity + values, used to apply a user's resolution.
#[derive(Debug, Clone, FromRow)]
pub struct ConflictDetail {
    pub series_id: Uuid,
    pub provider: String,
    pub field: String,
    pub local_value: String,
    pub remote_value: String,
}

/// Fetch a single pending conflict scoped to its owner, if it exists and is unresolved.
pub async fn get_pending_conflict<'e, E: PgExecutor<'e>>(
    exec: E,
    user_id: UserId,
    conflict_id: Uuid,
) -> DbResult<Option<ConflictDetail>> {
    let row = sqlx::query_as::<_, ConflictDetail>(
        "SELECT series_id, provider, field, local_value, remote_value \
         FROM sync_conflicts \
         WHERE id = $1 AND user_id = $2 AND resolved_at IS NULL",
    )
    .bind(conflict_id)
    .bind(user_id.as_uuid())
    .fetch_optional(exec)
    .await?;
    Ok(row)
}

/// Mark a conflict resolved with the chosen side (`local` | `remote`). Returns `true` if a
/// pending row was updated.
pub async fn resolve_conflict<'e, E: PgExecutor<'e>>(
    exec: E,
    user_id: UserId,
    conflict_id: Uuid,
    resolution: &str,
) -> DbResult<bool> {
    let result = sqlx::query(
        "UPDATE sync_conflicts SET resolved_at = now(), resolution = $3 \
         WHERE id = $1 AND user_id = $2 AND resolved_at IS NULL",
    )
    .bind(conflict_id)
    .bind(user_id.as_uuid())
    .bind(resolution)
    .execute(exec)
    .await?;
    Ok(result.rows_affected() > 0)
}

/// Count a user's pending conflicts, for the account panel badge and the admin console.
pub async fn count_pending_conflicts<'e, E: PgExecutor<'e>>(
    exec: E,
    user_id: UserId,
) -> DbResult<i64> {
    let (n,): (i64,) = sqlx::query_as(
        "SELECT count(*) FROM sync_conflicts WHERE user_id = $1 AND resolved_at IS NULL",
    )
    .bind(user_id.as_uuid())
    .fetch_one(exec)
    .await?;
    Ok(n)
}

/// Append one row to the user-facing sync history (design v2 §B.2): a transparency log of what
/// the automatic engine actually did.
pub async fn append_history<'e, E: PgExecutor<'e>>(
    exec: E,
    user_id: UserId,
    series_id: SeriesId,
    provider: &str,
    action: &str,
    detail: &Json,
) -> DbResult<()> {
    sqlx::query(
        "INSERT INTO sync_history (user_id, series_id, provider, action, detail) \
         VALUES ($1,$2,$3,$4,$5)",
    )
    .bind(user_id.as_uuid())
    .bind(series_id.as_uuid())
    .bind(provider)
    .bind(action)
    .bind(detail)
    .execute(exec)
    .await?;
    Ok(())
}

/// One row of the user-facing sync history (design v2 §B.6 `GET /v1/me/sync/history`).
#[derive(Debug, Clone, Serialize, FromRow)]
pub struct HistoryRow {
    pub id: Uuid,
    pub series_id: Uuid,
    pub series_title: String,
    pub provider: String,
    pub action: String,
    pub detail: Json,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

/// A page of a user's sync history, newest first, optionally filtered by series and/or provider.
pub async fn list_history<'e, E: PgExecutor<'e>>(
    exec: E,
    user_id: UserId,
    series_id: Option<SeriesId>,
    provider: Option<&str>,
    limit: i64,
    offset: i64,
) -> DbResult<Vec<HistoryRow>> {
    let rows = sqlx::query_as::<_, HistoryRow>(
        "SELECT h.id, h.series_id, s.canonical_title AS series_title, h.provider, h.action, \
                h.detail, h.created_at \
         FROM sync_history h JOIN series s ON s.id = h.series_id \
         WHERE h.user_id = $1 \
           AND ($2::uuid IS NULL OR h.series_id = $2) \
           AND ($3::text IS NULL OR h.provider = $3) \
         ORDER BY h.created_at DESC \
         LIMIT $4 OFFSET $5",
    )
    .bind(user_id.as_uuid())
    .bind(series_id.map(|s| s.as_uuid()))
    .bind(provider)
    .bind(limit)
    .bind(offset)
    .fetch_all(exec)
    .await?;
    Ok(rows)
}

/// A stored external-provider account. `access_token`/`refresh_token` are AES-GCM
/// ciphertext (see module docs); callers decrypt with the sync service's data key.
#[derive(Debug, Clone)]
pub struct ExternalAccount {
    pub user_id: UserId,
    pub provider: String,
    pub access_token: Vec<u8>,
    pub refresh_token: Option<Vec<u8>>,
    pub expires_at: Option<OffsetDateTime>,
    /// The provider's display name for the linked account (e.g. an `AniList` username), kept
    /// current on link and on every sync so the UI can show "Connected as X" without an
    /// extra round-trip.
    pub external_username: Option<String>,
    /// When this account last completed a pull or push.
    pub last_synced_at: Option<OffsetDateTime>,
    /// The most recent sync failure message, if any. Cleared on the next successful sync
    /// (`mark_synced`); set by `record_sync_error`. Admin-visible only (design: Sync console
    /// tab) — never surfaced on the user-facing status endpoint.
    pub last_error: Option<String>,
    /// Whether automatic sync (reactive push + scheduled reconciliation) is enabled for this
    /// account (design v2 §B.2). Seeded `true` on link.
    pub auto_sync_enabled: bool,
    /// The persisted per-account conflict policy token (design v2 §B.3): one of
    /// `local_wins` | `remote_wins` | `newest_wins` | `ask_me`. Seeded from the service
    /// default on link.
    pub conflict_policy: String,
}

/// Insert or replace a user's account for `provider`. Idempotent on `(user_id, provider)`,
/// so re-linking (e.g. a token refresh) overwrites the prior ciphertext in place.
pub async fn upsert_account<'e, E: PgExecutor<'e>>(
    exec: E,
    user_id: UserId,
    provider: &str,
    access_token: &[u8],
    refresh_token: Option<&[u8]>,
    expires_at: Option<OffsetDateTime>,
) -> DbResult<()> {
    sqlx::query(
        "INSERT INTO external_accounts \
            (user_id, provider, access_token, refresh_token, expires_at) \
         VALUES ($1,$2,$3,$4,$5) \
         ON CONFLICT (user_id, provider) DO UPDATE \
            SET access_token  = EXCLUDED.access_token, \
                refresh_token = EXCLUDED.refresh_token, \
                expires_at    = EXCLUDED.expires_at",
    )
    .bind(user_id.as_uuid())
    .bind(provider)
    .bind(access_token)
    .bind(refresh_token)
    .bind(expires_at)
    .execute(exec)
    .await?;
    Ok(())
}

/// Fetch a user's account for `provider`, if linked.
pub async fn get_account<'e, E: PgExecutor<'e>>(
    exec: E,
    user_id: UserId,
    provider: &str,
) -> DbResult<Option<ExternalAccount>> {
    #[derive(FromRow)]
    struct Row {
        user_id: Uuid,
        provider: String,
        access_token: Vec<u8>,
        refresh_token: Option<Vec<u8>>,
        expires_at: Option<OffsetDateTime>,
        external_username: Option<String>,
        last_synced_at: Option<OffsetDateTime>,
        last_error: Option<String>,
        auto_sync_enabled: bool,
        conflict_policy: String,
    }
    let row: Option<Row> = sqlx::query_as(
        "SELECT user_id, provider, access_token, refresh_token, expires_at, \
                external_username, last_synced_at, last_error, \
                auto_sync_enabled, conflict_policy \
         FROM external_accounts WHERE user_id = $1 AND provider = $2",
    )
    .bind(user_id.as_uuid())
    .bind(provider)
    .fetch_optional(exec)
    .await?;
    Ok(row.map(|r| ExternalAccount {
        user_id: UserId::from_uuid(r.user_id),
        provider: r.provider,
        access_token: r.access_token,
        refresh_token: r.refresh_token,
        expires_at: r.expires_at,
        external_username: r.external_username,
        last_synced_at: r.last_synced_at,
        last_error: r.last_error,
        auto_sync_enabled: r.auto_sync_enabled,
        conflict_policy: r.conflict_policy,
    }))
}

/// Update a linked account's automatic-sync policy (design v2 §B.2/§B.6). Either field may be
/// left `None` to keep its current value. Seeds are set at link time via `seed_account_policy`.
pub async fn update_account_settings<'e, E: PgExecutor<'e>>(
    exec: E,
    user_id: UserId,
    provider: &str,
    auto_sync_enabled: Option<bool>,
    conflict_policy: Option<&str>,
) -> DbResult<()> {
    sqlx::query(
        "UPDATE external_accounts \
         SET auto_sync_enabled = COALESCE($3, auto_sync_enabled), \
             conflict_policy   = COALESCE($4, conflict_policy) \
         WHERE user_id = $1 AND provider = $2",
    )
    .bind(user_id.as_uuid())
    .bind(provider)
    .bind(auto_sync_enabled)
    .bind(conflict_policy)
    .execute(exec)
    .await?;
    Ok(())
}

/// Seed the per-account conflict policy from the service default the first time an account is
/// linked, without disturbing an existing user choice (design v2 §B.1: the env default is only
/// the seed). No-op once the user has set anything.
pub async fn seed_account_policy<'e, E: PgExecutor<'e>>(
    exec: E,
    user_id: UserId,
    provider: &str,
    default_policy: &str,
) -> DbResult<()> {
    sqlx::query(
        "UPDATE external_accounts SET conflict_policy = $3 \
         WHERE user_id = $1 AND provider = $2 AND conflict_policy = 'newest_wins'",
    )
    .bind(user_id.as_uuid())
    .bind(provider)
    .bind(default_policy)
    .execute(exec)
    .await?;
    Ok(())
}

/// Every linked account with automatic sync enabled, as `(user_id, provider)` pairs, for the
/// scheduled reconciliation loop (design v2 §B.4).
pub async fn list_auto_sync_accounts<'e, E: PgExecutor<'e>>(
    exec: E,
) -> DbResult<Vec<(UserId, String)>> {
    #[derive(FromRow)]
    struct Row {
        user_id: Uuid,
        provider: String,
    }
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT user_id, provider FROM external_accounts \
         WHERE auto_sync_enabled ORDER BY user_id, provider",
    )
    .fetch_all(exec)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| (UserId::from_uuid(r.user_id), r.provider))
        .collect())
}

/// Record a fresh sync timestamp and (when known) the provider's display name for a linked
/// account. Called after linking (captures the username) and after every pull/push (bumps
/// `last_synced_at`), so the UI can render "Connected as X - last sync Ym ago" without ever
/// calling the external provider on page load. A `None` username leaves the stored one as-is.
pub async fn mark_synced<'e, E: PgExecutor<'e>>(
    exec: E,
    user_id: UserId,
    provider: &str,
    username: Option<&str>,
    synced_at: OffsetDateTime,
) -> DbResult<()> {
    sqlx::query(
        "UPDATE external_accounts \
         SET external_username = COALESCE($3, external_username), last_synced_at = $4, \
             last_error = NULL \
         WHERE user_id = $1 AND provider = $2",
    )
    .bind(user_id.as_uuid())
    .bind(provider)
    .bind(username)
    .bind(synced_at)
    .execute(exec)
    .await?;
    Ok(())
}

/// Record a sync failure for a linked account (admin Sync console tab). Overwritten by the
/// next successful `mark_synced`, which clears it back to `NULL`.
pub async fn record_sync_error<'e, E: PgExecutor<'e>>(
    exec: E,
    user_id: UserId,
    provider: &str,
    error: &str,
) -> DbResult<()> {
    sqlx::query(
        "UPDATE external_accounts SET last_error = $3 WHERE user_id = $1 AND provider = $2",
    )
    .bind(user_id.as_uuid())
    .bind(provider)
    .bind(error)
    .execute(exec)
    .await?;
    Ok(())
}

/// Unlink a user's account for `provider`. Returns `true` if a row was removed.
pub async fn delete_account<'e, E: PgExecutor<'e>>(
    exec: E,
    user_id: UserId,
    provider: &str,
) -> DbResult<bool> {
    let result = sqlx::query("DELETE FROM external_accounts WHERE user_id = $1 AND provider = $2")
        .bind(user_id.as_uuid())
        .bind(provider)
        .execute(exec)
        .await?;
    Ok(result.rows_affected() > 0)
}

/// Record (or refresh) the mapping between a canonical series and its external id at
/// `provider`. Idempotent on `(series_id, provider)`.
pub async fn upsert_mapping<'e, E: PgExecutor<'e>>(
    exec: E,
    series_id: SeriesId,
    provider: &str,
    external_id: &str,
) -> DbResult<()> {
    sqlx::query(
        "INSERT INTO sync_mappings (series_id, provider, external_id) \
         VALUES ($1,$2,$3) \
         ON CONFLICT (series_id, provider) DO UPDATE \
            SET external_id = EXCLUDED.external_id, updated_at = now()",
    )
    .bind(series_id.as_uuid())
    .bind(provider)
    .bind(external_id)
    .execute(exec)
    .await?;
    Ok(())
}

/// Resolve a provider's external id to a canonical series, if already mapped. Used to
/// short-circuit title re-matching on subsequent syncs.
pub async fn mapping_series_for_external<'e, E: PgExecutor<'e>>(
    exec: E,
    provider: &str,
    external_id: &str,
) -> DbResult<Option<SeriesId>> {
    let id: Option<Uuid> = sqlx::query_scalar(
        "SELECT series_id FROM sync_mappings WHERE provider = $1 AND external_id = $2",
    )
    .bind(provider)
    .bind(external_id)
    .fetch_optional(exec)
    .await?;
    Ok(id.map(SeriesId::from_uuid))
}

/// Resolve a canonical series to its external id at `provider`, if mapped. Used by push
/// to target the correct remote entry.
pub async fn mapping_external_for_series<'e, E: PgExecutor<'e>>(
    exec: E,
    series_id: SeriesId,
    provider: &str,
) -> DbResult<Option<String>> {
    let ext: Option<String> = sqlx::query_scalar(
        "SELECT external_id FROM sync_mappings WHERE series_id = $1 AND provider = $2",
    )
    .bind(series_id.as_uuid())
    .bind(provider)
    .fetch_optional(exec)
    .await?;
    Ok(ext)
}

/// List the provider slugs a user has linked an account for. Used by the targeted single-series
/// sync push to fan out only to providers the user actually has, without probing the whole
/// provider registry.
pub async fn list_linked_providers<'e, E: PgExecutor<'e>>(
    exec: E,
    user_id: UserId,
) -> DbResult<Vec<String>> {
    let providers: Vec<String> = sqlx::query_scalar(
        "SELECT provider FROM external_accounts WHERE user_id = $1 ORDER BY provider",
    )
    .bind(user_id.as_uuid())
    .fetch_all(exec)
    .await?;
    Ok(providers)
}

/// Remove a series↔external mapping for `provider`. Returns `true` if a row was removed.
/// The next pull/push re-resolves the series from scratch (title match or search).
pub async fn delete_mapping<'e, E: PgExecutor<'e>>(
    exec: E,
    series_id: SeriesId,
    provider: &str,
) -> DbResult<bool> {
    let result = sqlx::query("DELETE FROM sync_mappings WHERE series_id = $1 AND provider = $2")
        .bind(series_id.as_uuid())
        .bind(provider)
        .execute(exec)
        .await?;
    Ok(result.rows_affected() > 0)
}

/// One row of the admin Sync console's "Linked accounts" table. The automatic-sync policy
/// columns and pending-conflict count (design v2 §B.7) are read-only operator visibility —
/// they are user settings, never operator-overridable.
#[derive(Debug, Clone, Serialize, FromRow, utoipa::ToSchema)]
pub struct AdminAccountRow {
    pub user_id: Uuid,
    pub username: String,
    pub provider: String,
    pub external_username: Option<String>,
    #[serde(with = "time::serde::rfc3339::option")]
    #[schema(value_type = Option<String>)]
    pub last_synced_at: Option<OffsetDateTime>,
    pub last_error: Option<String>,
    pub auto_sync_enabled: bool,
    pub conflict_policy: String,
    pub pending_conflicts: i64,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String)]
    pub created_at: OffsetDateTime,
}

/// All linked external accounts across every user, newest-error-first then most-recently
/// synced (design: admin Sync console tab).
pub async fn admin_list_accounts<'e, E: PgExecutor<'e>>(
    exec: E,
    limit: i64,
) -> DbResult<Vec<AdminAccountRow>> {
    let rows = sqlx::query_as::<_, AdminAccountRow>(
        "SELECT ea.user_id, u.username, ea.provider, ea.external_username, \
                ea.last_synced_at, ea.last_error, ea.auto_sync_enabled, ea.conflict_policy, \
                (SELECT count(*) FROM sync_conflicts sc \
                   WHERE sc.user_id = ea.user_id AND sc.resolved_at IS NULL) AS pending_conflicts, \
                ea.created_at \
         FROM external_accounts ea JOIN users u ON u.id = ea.user_id \
         ORDER BY (ea.last_error IS NOT NULL) DESC, ea.last_synced_at DESC NULLS LAST \
         LIMIT $1",
    )
    .bind(limit)
    .fetch_all(exec)
    .await?;
    Ok(rows)
}

/// One row of the admin Sync console's "Series mappings" table.
#[derive(Debug, Clone, Serialize, FromRow, utoipa::ToSchema)]
pub struct AdminMappingRow {
    pub series_id: Uuid,
    pub series_title: String,
    pub provider: String,
    pub external_id: String,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String)]
    pub updated_at: OffsetDateTime,
}

/// All series↔external mappings across every provider, most recently updated first.
pub async fn admin_list_mappings<'e, E: PgExecutor<'e>>(
    exec: E,
    limit: i64,
) -> DbResult<Vec<AdminMappingRow>> {
    let rows = sqlx::query_as::<_, AdminMappingRow>(
        "SELECT sm.series_id, s.canonical_title AS series_title, sm.provider, \
                sm.external_id, sm.updated_at \
         FROM sync_mappings sm JOIN series s ON s.id = sm.series_id \
         ORDER BY sm.updated_at DESC \
         LIMIT $1",
    )
    .bind(limit)
    .fetch_all(exec)
    .await?;
    Ok(rows)
}

/// Every external mapping recorded for a single canonical series (one row per provider),
/// used by the admin console's per-series "manga info" editor to show what the series is
/// synced to (or not) across all external providers.
pub async fn admin_list_mappings_for_series<'e, E: PgExecutor<'e>>(
    exec: E,
    series_id: SeriesId,
) -> DbResult<Vec<AdminMappingRow>> {
    let rows = sqlx::query_as::<_, AdminMappingRow>(
        "SELECT sm.series_id, s.canonical_title AS series_title, sm.provider, \
                sm.external_id, sm.updated_at \
         FROM sync_mappings sm JOIN series s ON s.id = sm.series_id \
         WHERE sm.series_id = $1 \
         ORDER BY sm.provider",
    )
    .bind(series_id.as_uuid())
    .fetch_all(exec)
    .await?;
    Ok(rows)
}

/// One row of the admin Sync console's "Assign queue" — a canonical series that has **no**
/// external mapping for the given provider yet, so an operator can review and assign one.
#[derive(Debug, Clone, Serialize, FromRow, utoipa::ToSchema)]
pub struct UnmappedSeriesRow {
    pub series_id: Uuid,
    pub series_title: String,
    /// How many local sources back this series (a proxy for how confident a match is worth).
    pub source_count: i64,
}

/// Series lacking a mapping for `provider`, richest (most sources) first so the operator
/// works the most-connected — and therefore highest-value — entries at the top of the
/// assign queue. An optional case-insensitive title `query` narrows the list.
pub async fn admin_list_unmapped<'e, E: PgExecutor<'e>>(
    exec: E,
    provider: &str,
    query: Option<&str>,
    limit: i64,
) -> DbResult<Vec<UnmappedSeriesRow>> {
    let like = query
        .map(|q| format!("%{}%", q.trim()))
        .filter(|q| q.len() > 2);
    let rows = sqlx::query_as::<_, UnmappedSeriesRow>(
        "SELECT s.id AS series_id, s.canonical_title AS series_title, \
                count(ss.id) AS source_count \
         FROM series s \
         LEFT JOIN series_sources ss ON ss.series_id = s.id \
         WHERE NOT EXISTS ( \
                 SELECT 1 FROM sync_mappings sm \
                 WHERE sm.series_id = s.id AND sm.provider = $1) \
           AND ($2::text IS NULL OR s.canonical_title ILIKE $2) \
         GROUP BY s.id, s.canonical_title \
         ORDER BY count(ss.id) DESC, s.canonical_title \
         LIMIT $3",
    )
    .bind(provider)
    .bind(like)
    .bind(limit)
    .fetch_all(exec)
    .await?;
    Ok(rows)
}

// ---------------------------------------------------------------------------
// Remote-entry snapshots (design §15, admin "match every loaded entry" queue)
// ---------------------------------------------------------------------------

/// Upsert one fetched remote entry snapshot. Called for **every** entry a pull sees, matched
/// or not: `series_id` is the canonical series it resolved to, or `None` for the unmatched
/// queue the admin console works. Overwrites the previous snapshot for that (user, provider,
/// external id) so the stored status/progress stay current with the provider.
#[allow(clippy::too_many_arguments)]
pub async fn upsert_remote_entry<'e, E: PgExecutor<'e>>(
    exec: E,
    user_id: UserId,
    provider: &str,
    external_id: &str,
    title: &str,
    status: &str,
    progress: f64,
    content_type: &str,
    start_year: Option<i32>,
    updated_at: OffsetDateTime,
    series_id: Option<SeriesId>,
) -> DbResult<()> {
    sqlx::query(
        "INSERT INTO sync_remote_entries \
           (user_id, provider, external_id, title, status, progress, content_type, \
            start_year, updated_at, series_id, fetched_at) \
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10, now()) \
         ON CONFLICT (user_id, provider, external_id) DO UPDATE SET \
            title = EXCLUDED.title, status = EXCLUDED.status, progress = EXCLUDED.progress, \
            content_type = EXCLUDED.content_type, start_year = EXCLUDED.start_year, \
            updated_at = EXCLUDED.updated_at, series_id = EXCLUDED.series_id, \
            fetched_at = now()",
    )
    .bind(user_id.as_uuid())
    .bind(provider)
    .bind(external_id)
    .bind(title)
    .bind(status)
    .bind(progress)
    .bind(content_type)
    .bind(start_year)
    .bind(updated_at)
    .bind(series_id.map(|s| s.as_uuid()))
    .execute(exec)
    .await?;
    Ok(())
}

/// One row of the admin console's "Unmatched remote entries" queue: a fetched provider entry
/// the auto-matcher could not confidently link to a local series.
#[derive(Debug, Clone, Serialize, FromRow, utoipa::ToSchema)]
pub struct RemoteEntryRow {
    pub user_id: Uuid,
    pub username: String,
    pub provider: String,
    pub external_id: String,
    pub title: String,
    pub status: String,
    pub progress: f64,
    pub content_type: String,
    pub start_year: Option<i32>,
}

/// Unmatched remote entries for `provider`, alphabetically by title. An optional
/// case-insensitive `query` narrows the list. This is the reverse of [`admin_list_unmapped`]:
/// it works from the *remote* side so an operator can reconcile every loaded entry.
pub async fn admin_list_unmatched_remote<'e, E: PgExecutor<'e>>(
    exec: E,
    provider: &str,
    query: Option<&str>,
    limit: i64,
) -> DbResult<Vec<RemoteEntryRow>> {
    let like = query
        .map(|q| format!("%{}%", q.trim()))
        .filter(|q| q.len() > 2);
    let rows = sqlx::query_as::<_, RemoteEntryRow>(
        "SELECT re.user_id, u.username, re.provider, re.external_id, re.title, re.status, \
                re.progress, re.content_type, re.start_year \
         FROM sync_remote_entries re JOIN users u ON u.id = re.user_id \
         WHERE re.series_id IS NULL AND re.provider = $1 \
           AND ($2::text IS NULL OR re.title ILIKE $2) \
         ORDER BY re.title \
         LIMIT $3",
    )
    .bind(provider)
    .bind(like)
    .bind(limit)
    .fetch_all(exec)
    .await?;
    Ok(rows)
}

/// The stored snapshot for one remote entry, used to import it (status + progress) when an
/// operator hand-assigns it to a series without waiting for the next pull.
#[derive(Debug, Clone, FromRow)]
pub struct RemoteEntrySnapshot {
    pub title: String,
    pub status: String,
    pub progress: f64,
    pub updated_at: OffsetDateTime,
}

/// Fetch one stored remote-entry snapshot, if present.
pub async fn get_remote_entry<'e, E: PgExecutor<'e>>(
    exec: E,
    user_id: UserId,
    provider: &str,
    external_id: &str,
) -> DbResult<Option<RemoteEntrySnapshot>> {
    let row = sqlx::query_as::<_, RemoteEntrySnapshot>(
        "SELECT title, status, progress, updated_at FROM sync_remote_entries \
         WHERE user_id = $1 AND provider = $2 AND external_id = $3",
    )
    .bind(user_id.as_uuid())
    .bind(provider)
    .bind(external_id)
    .fetch_optional(exec)
    .await?;
    Ok(row)
}

/// A local-series candidate for the admin "match every loaded entry" suggestions: the
/// trigram similarity plus the display fields the console needs to rank, preview and inspect
/// it. The `matcher` crate turns `similarity`/`content_type`/`release_year` into a final
/// score; `title`/`source_count` are for the operator's eyes only.
#[derive(Debug, Clone, FromRow)]
pub struct SeriesCandidateRow {
    pub series_id: Uuid,
    pub title: String,
    pub normalized_title: String,
    pub content_type: String,
    pub release_year: Option<i32>,
    pub source_count: i64,
    /// Best trigram similarity in `[0,1]` across the canonical + alternative titles.
    pub similarity: f32,
}

/// Trigram-similar local series for a remote entry's `normalized` title, richest signal
/// first, enriched with the display title and source count so the admin console can rank,
/// preview and inspect each suggestion. Mirrors [`matching::find_candidates`](super::matching)
/// but also returns the canonical display title and `source_count` (that lookup returns only
/// normalized titles). The caller (sync suggest endpoint) applies the `matcher` score on top.
pub async fn suggest_series_candidates<'e, E: PgExecutor<'e>>(
    exec: E,
    normalized: &str,
    limit: i64,
) -> DbResult<Vec<SeriesCandidateRow>> {
    let rows = sqlx::query_as::<_, SeriesCandidateRow>(
        "SELECT s.id AS series_id, s.canonical_title AS title, s.normalized_title, \
                s.content_type::text AS content_type, s.release_year, \
                (SELECT count(*) FROM series_sources ss WHERE ss.series_id = s.id) \
                    AS source_count, \
                GREATEST( \
                  similarity(s.normalized_title, $1), \
                  COALESCE((SELECT MAX(similarity(st.normalized, $1)) \
                            FROM series_titles st WHERE st.series_id = s.id), 0) \
                ) AS similarity \
         FROM series s \
         WHERE s.normalized_title % $1 \
            OR EXISTS (SELECT 1 FROM series_titles st \
                       WHERE st.series_id = s.id AND st.normalized % $1) \
         ORDER BY similarity DESC \
         LIMIT $2",
    )
    .bind(normalized)
    .bind(limit)
    .fetch_all(exec)
    .await?;
    Ok(rows)
}

/// Record that a remote entry now resolves to `series_id` (removing it from the unmatched
/// queue) after an operator assignment.
pub async fn mark_remote_entry_matched<'e, E: PgExecutor<'e>>(
    exec: E,
    user_id: UserId,
    provider: &str,
    external_id: &str,
    series_id: SeriesId,
) -> DbResult<()> {
    sqlx::query(
        "UPDATE sync_remote_entries SET series_id = $4 \
         WHERE user_id = $1 AND provider = $2 AND external_id = $3",
    )
    .bind(user_id.as_uuid())
    .bind(provider)
    .bind(external_id)
    .bind(series_id.as_uuid())
    .execute(exec)
    .await?;
    Ok(())
}
