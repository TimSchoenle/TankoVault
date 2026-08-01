//! Provider repository: the single source of truth for a site's domain + config.

use crate::error::{DbError, DbResult};
use serde_json::Value as Json;
use sqlx::types::Json as SqlxJson;
use sqlx::{FromRow, PgExecutor};
use tankovault_domain::{AdapterKind, Politeness, Provider, ProviderId, ProviderState};
use time::OffsetDateTime;
use uuid::Uuid;

/// Column projection for `providers`, read back as the native enum types.
#[derive(FromRow)]
struct ProviderRow {
    id: Uuid,
    slug: String,
    name: String,
    base_url: String,
    adapter: AdapterKind,
    config: Json,
    state: ProviderState,
    politeness: SqlxJson<Politeness>,
    last_full_scan_at: Option<OffsetDateTime>,
    created_at: OffsetDateTime,
    updated_at: OffsetDateTime,
}

impl From<ProviderRow> for Provider {
    fn from(r: ProviderRow) -> Self {
        Self {
            id: ProviderId::from_uuid(r.id),
            slug: r.slug,
            name: r.name,
            base_url: r.base_url,
            adapter: r.adapter,
            config: r.config,
            state: r.state,
            politeness: r.politeness.0.clamped(),
            last_full_scan_at: r.last_full_scan_at,
            created_at: r.created_at,
            updated_at: r.updated_at,
        }
    }
}

/// Parameters for creating a provider.
pub struct NewProvider {
    pub slug: String,
    pub name: String,
    pub base_url: String,
    pub adapter: AdapterKind,
    pub config: Json,
    pub politeness: Politeness,
}

/// Insert a provider, returning the created row.
///
/// # Errors
/// `Conflict` on a duplicate slug, otherwise `Sqlx`.
pub async fn create<'e, E: PgExecutor<'e>>(exec: E, new: NewProvider) -> DbResult<Provider> {
    let id = ProviderId::new();
    let row = sqlx::query_as!(
        ProviderRow,
        "INSERT INTO providers (id, slug, name, base_url, adapter, config, politeness) \
         VALUES ($1, $2, $3, $4, $5, $6, $7) \
         RETURNING id, slug, name, base_url, adapter AS \"adapter: AdapterKind\", \
                   config AS \"config: Json\", state AS \"state: ProviderState\", \
                   politeness AS \"politeness: SqlxJson<Politeness>\", \
                   last_full_scan_at, created_at, updated_at",
        id.as_uuid(),
        new.slug,
        new.name,
        new.base_url,
        new.adapter as AdapterKind,
        new.config,
        SqlxJson(new.politeness.clamped()) as _,
    )
    .fetch_one(exec)
    .await
    .map_err(|e| {
        let de = DbError::from(e);
        if de.is_unique_violation() {
            DbError::Conflict(format!("provider slug already exists: {}", new.slug))
        } else {
            de
        }
    })?;
    Ok(row.into())
}

/// List all providers, most recently updated first.
///
/// # Errors
/// `Sqlx` only; an empty table is `Ok(vec![])`.
pub async fn list<'e, E: PgExecutor<'e>>(exec: E) -> DbResult<Vec<Provider>> {
    let rows = sqlx::query_as!(
        ProviderRow,
        "SELECT id, slug, name, base_url, adapter AS \"adapter: AdapterKind\", \
                config AS \"config: Json\", state AS \"state: ProviderState\", \
                politeness AS \"politeness: SqlxJson<Politeness>\", \
                last_full_scan_at, created_at, updated_at \
         FROM providers ORDER BY updated_at DESC",
    )
    .fetch_all(exec)
    .await?;
    Ok(rows.into_iter().map(Provider::from).collect())
}

/// Fetch one provider by id.
///
/// # Errors
/// `NotFound` (404) when no provider has that id, otherwise `Sqlx`.
pub async fn get<'e, E: PgExecutor<'e>>(exec: E, id: ProviderId) -> DbResult<Provider> {
    let row = sqlx::query_as!(
        ProviderRow,
        "SELECT id, slug, name, base_url, adapter AS \"adapter: AdapterKind\", \
                config AS \"config: Json\", state AS \"state: ProviderState\", \
                politeness AS \"politeness: SqlxJson<Politeness>\", \
                last_full_scan_at, created_at, updated_at \
         FROM providers WHERE id = $1",
        id.as_uuid(),
    )
    .fetch_optional(exec)
    .await?;
    Ok(row.ok_or(DbError::NotFound)?.into())
}

/// Fetch one provider by slug.
///
/// # Errors
/// `NotFound` (404) when no provider has that slug, otherwise `Sqlx`.
pub async fn get_by_slug<'e, E: PgExecutor<'e>>(exec: E, slug: &str) -> DbResult<Provider> {
    let row = sqlx::query_as!(
        ProviderRow,
        "SELECT id, slug, name, base_url, adapter AS \"adapter: AdapterKind\", \
                config AS \"config: Json\", state AS \"state: ProviderState\", \
                politeness AS \"politeness: SqlxJson<Politeness>\", \
                last_full_scan_at, created_at, updated_at \
         FROM providers WHERE slug = $1",
        slug,
    )
    .fetch_optional(exec)
    .await?;
    Ok(row.ok_or(DbError::NotFound)?.into())
}

/// Update the mutable provider fields (name, `base_url`, config, politeness). Changing
/// `base_url` re-resolves every stored relative link with zero data rewrite.
///
/// # Errors
/// `NotFound` (404) when `id` matches no row, otherwise `Sqlx`. `politeness` is stored via
/// [`Politeness::clamped`] — an out-of-range rate is corrected, not rejected.
pub async fn update<'e, E: PgExecutor<'e>>(
    exec: E,
    id: ProviderId,
    name: &str,
    base_url: &str,
    config: &Json,
    politeness: Politeness,
) -> DbResult<Provider> {
    let row = sqlx::query_as!(
        ProviderRow,
        "UPDATE providers SET name = $2, base_url = $3, config = $4, politeness = $5, \
         updated_at = now() WHERE id = $1 \
         RETURNING id, slug, name, base_url, adapter AS \"adapter: AdapterKind\", \
                   config AS \"config: Json\", state AS \"state: ProviderState\", \
                   politeness AS \"politeness: SqlxJson<Politeness>\", \
                   last_full_scan_at, created_at, updated_at",
        id.as_uuid(),
        name,
        base_url,
        config,
        SqlxJson(politeness.clamped()) as _,
    )
    .fetch_optional(exec)
    .await?;
    Ok(row.ok_or(DbError::NotFound)?.into())
}

/// Transition a provider's health state. Unconditional — legal-transition rules live in the
/// caller, not here.
///
/// # Errors
/// `NotFound` (404) when `id` matches no row, otherwise `Sqlx`.
pub async fn set_state<'e, E: PgExecutor<'e>>(
    exec: E,
    id: ProviderId,
    state: ProviderState,
) -> DbResult<()> {
    let result = sqlx::query!(
        "UPDATE providers SET state = $2, updated_at = now() WHERE id = $1",
        id.as_uuid(),
        state as ProviderState,
    )
    .execute(exec)
    .await?;
    if result.rows_affected() == 0 {
        return Err(DbError::NotFound);
    }
    Ok(())
}

/// Delete a provider by id. `sources` cascade; `scan_runs` keep their history with
/// `provider_id` set NULL.
///
/// # Errors
/// `NotFound` if no provider has that id.
pub async fn delete<'e, E: PgExecutor<'e>>(exec: E, id: ProviderId) -> DbResult<()> {
    let result = sqlx::query!("DELETE FROM providers WHERE id = $1", id.as_uuid())
        .execute(exec)
        .await?;
    if result.rows_affected() == 0 {
        return Err(DbError::NotFound);
    }
    Ok(())
}

/// Fetch several providers by id in one round trip (avoids an N+1 loop over [`get`]). Rows
/// come back in planner order; callers look up by id.
///
/// # Errors
/// `Sqlx` only; ids with no row are absent from the result.
pub async fn get_many<'e, E: PgExecutor<'e>>(
    exec: E,
    ids: &[ProviderId],
) -> DbResult<Vec<Provider>> {
    let ids: Vec<Uuid> = ids.iter().map(|id| id.as_uuid()).collect();
    let rows = sqlx::query_as!(
        ProviderRow,
        "SELECT id, slug, name, base_url, adapter AS \"adapter: AdapterKind\",                 config AS \"config: Json\", state AS \"state: ProviderState\",                 politeness AS \"politeness: SqlxJson<Politeness>\",                 last_full_scan_at, created_at, updated_at          FROM providers WHERE id = ANY($1)",
        &ids,
    )
    .fetch_all(exec)
    .await?;
    Ok(rows.into_iter().map(Into::into).collect())
}

/// A public-facing provider entry for the Discover filter list: identity plus series count,
/// without exposing operator-only config/politeness.
#[derive(Debug, Clone, serde::Serialize, FromRow)]
pub struct PublicProvider {
    pub id: Uuid,
    pub slug: String,
    pub name: String,
    /// Distinct canonical series with at least one source on this provider.
    pub series_count: i64,
}

/// List providers for the public Discover filter, richest first, hiding disabled ones.
///
/// # Errors
/// `Sqlx` only; unhealthy (but not disabled) providers still appear.
pub async fn list_public<'e, E: PgExecutor<'e>>(exec: E) -> DbResult<Vec<PublicProvider>> {
    let rows = sqlx::query_as!(
        PublicProvider,
        "SELECT p.id, p.slug, p.name, \
                (SELECT count(DISTINCT ss.series_id) FROM series_sources ss \
                   WHERE ss.provider_id = p.id) AS \"series_count!\" \
         FROM providers p WHERE p.state <> 'disabled' \
         ORDER BY 4 DESC, p.name ASC",
    )
    .fetch_all(exec)
    .await?;
    Ok(rows)
}

/// Stamp the completion time of a full scan.
///
/// # Errors
/// `Sqlx` only; a provider deleted mid-scan updates nothing and still returns `Ok(())`.
pub async fn mark_full_scanned<'e, E: PgExecutor<'e>>(exec: E, id: ProviderId) -> DbResult<()> {
    sqlx::query!(
        "UPDATE providers SET last_full_scan_at = now(), updated_at = now() WHERE id = $1",
        id.as_uuid(),
    )
    .execute(exec)
    .await?;
    Ok(())
}
