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
/// [`DbError::Conflict`] on a duplicate slug; otherwise a driver error.
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
/// [`DbError::Sqlx`] only — no other variant is reachable. An empty table is an empty `Vec`.
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
/// [`DbError::NotFound`] — a 404 — when no provider has that id; otherwise [`DbError::Sqlx`].
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
/// [`DbError::NotFound`] — a 404 — when no provider has that slug; otherwise
/// [`DbError::Sqlx`].
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

/// Update the mutable provider fields (name, `base_url`, config, politeness).
///
/// Changing `base_url` is the **domain-migration** action: every stored relative link
/// re-resolves against the new domain with zero data rewrite (design §5).
///
/// # Errors
/// [`DbError::NotFound`] — a 404 — when `id` matches no row; otherwise [`DbError::Sqlx`].
/// Not [`DbError::Conflict`]: `slug` is the unique column and this statement does not touch
/// it. Note that `politeness` is stored through [`Politeness::clamped`], so an out-of-range
/// rate is corrected rather than rejected.
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

/// Transition a provider's health state (circuit breaker / solver lifecycle).
///
/// # Errors
/// [`DbError::NotFound`] — a 404 — when `id` matches no row; otherwise [`DbError::Sqlx`].
/// No transition is rejected here: this is an unconditional assignment, so the circuit
/// breaker's legal-transition rules live in its caller, not in the write.
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

/// Delete a provider by id.
///
/// Its `sources` are removed by the `ON DELETE CASCADE` foreign key; `scan_runs` retain
/// their history with `provider_id` set to NULL (`ON DELETE SET NULL`).
///
/// # Errors
/// [`DbError::NotFound`] if no provider has that id.
pub async fn delete<'e, E: PgExecutor<'e>>(exec: E, id: ProviderId) -> DbResult<()> {
    let result = sqlx::query!("DELETE FROM providers WHERE id = $1", id.as_uuid())
        .execute(exec)
        .await?;
    if result.rows_affected() == 0 {
        return Err(DbError::NotFound);
    }
    Ok(())
}

/// Fetch several providers by id in one round trip.
///
/// The series-detail handler needs one provider per source group and used to call [`get`] in
/// a loop — a textbook N+1 against a table that is small, operator-managed reference data.
/// Rows come back in whatever order the planner produces; callers look up by id.
///
/// # Errors
/// Propagates any database failure. Ids with no row are simply absent from the result.
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

/// A public-facing provider entry for the Discover filter list (frontend §9.3
/// `GET /v1/providers`): identity plus how many distinct series it carries, so the UI can
/// show "Provider (N)" options without exposing operator-only config/politeness.
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
/// [`DbError::Sqlx`] only — no other variant is reachable. Every provider that is not
/// `disabled` appears, including unhealthy ones, so an empty `Vec` means the deployment has
/// no enabled provider rather than that the filter matched nothing.
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
/// [`DbError::Sqlx`] only — no other variant is reachable. A provider deleted while its scan
/// was running updates nothing and still returns `Ok(())`, so this cannot report that the
/// stamp was lost.
pub async fn mark_full_scanned<'e, E: PgExecutor<'e>>(exec: E, id: ProviderId) -> DbResult<()> {
    sqlx::query!(
        "UPDATE providers SET last_full_scan_at = now(), updated_at = now() WHERE id = $1",
        id.as_uuid(),
    )
    .execute(exec)
    .await?;
    Ok(())
}
