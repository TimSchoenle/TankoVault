//! Provider repository: the single source of truth for a site's domain + config.

use crate::error::{DbError, DbResult};
use tankovault_domain::{AdapterKind, Politeness, Provider, ProviderId, ProviderState};
use serde_json::Value as Json;
use sqlx::types::Json as SqlxJson;
use sqlx::{FromRow, PgExecutor};
use std::str::FromStr;
use time::OffsetDateTime;
use uuid::Uuid;

/// Column projection for `providers`, with enum columns cast to `text`.
#[derive(FromRow)]
struct ProviderRow {
    id: Uuid,
    slug: String,
    name: String,
    base_url: String,
    adapter: String,
    config: Json,
    state: String,
    politeness: SqlxJson<Politeness>,
    robots_txt: Option<String>,
    robots_at: Option<OffsetDateTime>,
    last_full_scan_at: Option<OffsetDateTime>,
    created_at: OffsetDateTime,
    updated_at: OffsetDateTime,
}

impl TryFrom<ProviderRow> for Provider {
    type Error = DbError;
    fn try_from(r: ProviderRow) -> Result<Self, Self::Error> {
        Ok(Self {
            id: ProviderId::from_uuid(r.id),
            slug: r.slug,
            name: r.name,
            base_url: r.base_url,
            adapter: AdapterKind::from_str(&r.adapter)?,
            config: r.config,
            state: ProviderState::from_str(&r.state)?,
            politeness: r.politeness.0.clamped(),
            robots_txt: r.robots_txt,
            robots_at: r.robots_at,
            last_full_scan_at: r.last_full_scan_at,
            created_at: r.created_at,
            updated_at: r.updated_at,
        })
    }
}

/// Full `SELECT ... FROM providers` projection as a string literal, so composed
/// queries stay static (`sqlx 0.9` rejects dynamically-built SQL strings).
macro_rules! provider_select {
    () => {
        "SELECT id, slug, name, base_url, adapter::text AS adapter, config, \
         state::text AS state, politeness, robots_txt, robots_at, last_full_scan_at, \
         created_at, updated_at FROM providers"
    };
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
/// [`DbError::Conflict`] on a duplicate slug; otherwise a driver error.
pub async fn create<'e, E: PgExecutor<'e>>(exec: E, new: NewProvider) -> DbResult<Provider> {
    let id = ProviderId::new();
    let row: ProviderRow = sqlx::query_as(
        "INSERT INTO providers (id, slug, name, base_url, adapter, config, politeness) \
         VALUES ($1, $2, $3, $4, $5::adapter_kind, $6, $7) \
         RETURNING id, slug, name, base_url, adapter::text AS adapter, config, \
                   state::text AS state, politeness, robots_txt, robots_at, \
                   last_full_scan_at, created_at, updated_at",
    )
    .bind(id.as_uuid())
    .bind(&new.slug)
    .bind(&new.name)
    .bind(&new.base_url)
    .bind(new.adapter.as_str())
    .bind(&new.config)
    .bind(SqlxJson(new.politeness.clamped()))
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
    row.try_into()
}

/// List all providers, most recently updated first.
pub async fn list<'e, E: PgExecutor<'e>>(exec: E) -> DbResult<Vec<Provider>> {
    let rows: Vec<ProviderRow> =
        sqlx::query_as(concat!(provider_select!(), " ORDER BY updated_at DESC"))
            .fetch_all(exec)
            .await?;
    rows.into_iter().map(Provider::try_from).collect()
}

/// Fetch one provider by id.
pub async fn get<'e, E: PgExecutor<'e>>(exec: E, id: ProviderId) -> DbResult<Provider> {
    let row: Option<ProviderRow> = sqlx::query_as(concat!(provider_select!(), " WHERE id = $1"))
        .bind(id.as_uuid())
        .fetch_optional(exec)
        .await?;
    row.ok_or(DbError::NotFound)?.try_into()
}

/// Fetch one provider by slug.
pub async fn get_by_slug<'e, E: PgExecutor<'e>>(exec: E, slug: &str) -> DbResult<Provider> {
    let row: Option<ProviderRow> = sqlx::query_as(concat!(provider_select!(), " WHERE slug = $1"))
        .bind(slug)
        .fetch_optional(exec)
        .await?;
    row.ok_or(DbError::NotFound)?.try_into()
}

/// Update the mutable provider fields (name, `base_url`, config, politeness).
///
/// Changing `base_url` is the **domain-migration** action: every stored relative link
/// re-resolves against the new domain with zero data rewrite (design §5).
pub async fn update<'e, E: PgExecutor<'e>>(
    exec: E,
    id: ProviderId,
    name: &str,
    base_url: &str,
    config: &Json,
    politeness: Politeness,
) -> DbResult<Provider> {
    let row: Option<ProviderRow> = sqlx::query_as(
        "UPDATE providers SET name = $2, base_url = $3, config = $4, politeness = $5, \
         updated_at = now() WHERE id = $1 \
         RETURNING id, slug, name, base_url, adapter::text AS adapter, config, \
                   state::text AS state, politeness, robots_txt, robots_at, \
                   last_full_scan_at, created_at, updated_at",
    )
    .bind(id.as_uuid())
    .bind(name)
    .bind(base_url)
    .bind(config)
    .bind(SqlxJson(politeness.clamped()))
    .fetch_optional(exec)
    .await?;
    row.ok_or(DbError::NotFound)?.try_into()
}

/// Transition a provider's health state (circuit breaker / solver lifecycle).
pub async fn set_state<'e, E: PgExecutor<'e>>(
    exec: E,
    id: ProviderId,
    state: ProviderState,
) -> DbResult<()> {
    let result = sqlx::query(
        "UPDATE providers SET state = $2::provider_state, updated_at = now() WHERE id = $1",
    )
    .bind(id.as_uuid())
    .bind(state.as_str())
    .execute(exec)
    .await?;
    if result.rows_affected() == 0 {
        return Err(DbError::NotFound);
    }
    Ok(())
}

/// Delete a provider by id.
///
/// Its `sources` are removed by the `ON DELETE CASCADE` foreign key; `scan_runs` retain
/// their history with `provider_id` set to NULL (`ON DELETE SET NULL`).
///
/// # Errors
/// [`DbError::NotFound`] if no provider has that id.
pub async fn delete<'e, E: PgExecutor<'e>>(exec: E, id: ProviderId) -> DbResult<()> {
    let result = sqlx::query("DELETE FROM providers WHERE id = $1")
        .bind(id.as_uuid())
        .execute(exec)
        .await?;
    if result.rows_affected() == 0 {
        return Err(DbError::NotFound);
    }
    Ok(())
}

/// Cache a provider's fetched robots.txt.
pub async fn set_robots<'e, E: PgExecutor<'e>>(
    exec: E,
    id: ProviderId,
    robots_txt: &str,
) -> DbResult<()> {
    sqlx::query("UPDATE providers SET robots_txt = $2, robots_at = now() WHERE id = $1")
        .bind(id.as_uuid())
        .bind(robots_txt)
        .execute(exec)
        .await?;
    Ok(())
}

/// Stamp the completion time of a full scan.
pub async fn mark_full_scanned<'e, E: PgExecutor<'e>>(exec: E, id: ProviderId) -> DbResult<()> {
    sqlx::query("UPDATE providers SET last_full_scan_at = now(), updated_at = now() WHERE id = $1")
        .bind(id.as_uuid())
        .execute(exec)
        .await?;
    Ok(())
}
