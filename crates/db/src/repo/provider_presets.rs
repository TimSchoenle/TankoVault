//! The preset catalogue mirror: the provider definitions this build ships, written down once
//! per rollout by `bootstrap seed-providers` so every tier can read them as rows.
//!
//! Nothing here is operator state. The installer owns the table wholesale — it upserts what the
//! build ships and retires what it no longer does — so an edit made here would survive exactly
//! until the next rollout.

use crate::error::DbResult;
use serde_json::Value as Json;
use sqlx::types::Json as SqlxJson;
use sqlx::{FromRow, PgExecutor};
use tankovault_domain::{AdapterKind, Politeness, PresetDefinition};
use time::OffsetDateTime;

/// Column projection for `provider_presets`.
#[derive(FromRow)]
struct PresetRow {
    slug: String,
    name: String,
    base_url: String,
    adapter: AdapterKind,
    config: Json,
    politeness: SqlxJson<Politeness>,
    updated_at: OffsetDateTime,
}

impl From<PresetRow> for PresetDefinition {
    fn from(r: PresetRow) -> Self {
        Self {
            slug: r.slug,
            name: r.name,
            base_url: r.base_url,
            adapter: r.adapter,
            config: r.config,
            politeness: r.politeness.0.clamped(),
            updated_at: r.updated_at,
        }
    }
}

/// One shipped preset, as the installer hands it over.
pub struct NewPreset {
    pub slug: String,
    pub name: String,
    pub base_url: String,
    pub adapter: AdapterKind,
    pub config: Json,
    pub politeness: Politeness,
}

/// Record one shipped preset, replacing whatever was there.
///
/// # Errors
/// `Sqlx` only.
pub async fn upsert<'e, E: PgExecutor<'e>>(exec: E, preset: &NewPreset) -> DbResult<()> {
    sqlx::query!(
        "INSERT INTO provider_presets (slug, name, base_url, adapter, config, politeness) \
         VALUES ($1, $2, $3, $4, $5, $6) \
         ON CONFLICT (slug) DO UPDATE SET name = EXCLUDED.name, base_url = EXCLUDED.base_url, \
             adapter = EXCLUDED.adapter, config = EXCLUDED.config, \
             politeness = EXCLUDED.politeness, updated_at = now()",
        preset.slug,
        preset.name,
        preset.base_url,
        preset.adapter as AdapterKind,
        preset.config,
        SqlxJson(preset.politeness.clone().clamped()) as _,
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// Drop catalogue entries this build no longer ships, returning their slugs for the install log.
///
/// Providers installed from a retired preset are deliberately left alone: their `preset_slug`
/// dangles, which is what the console reports as "no longer shipped". Deleting the row instead
/// would take a working provider — and its whole catalogue — down with a preset the project
/// merely stopped maintaining.
///
/// # Errors
/// `Sqlx` only.
pub async fn retire_missing<'e, E: PgExecutor<'e>>(
    exec: E,
    shipped: &[String],
) -> DbResult<Vec<String>> {
    let rows = sqlx::query_scalar!(
        "DELETE FROM provider_presets WHERE slug <> ALL($1) RETURNING slug",
        shipped,
    )
    .fetch_all(exec)
    .await?;
    Ok(rows)
}

/// The whole catalogue, by slug.
///
/// # Errors
/// `Sqlx` only; a deployment whose install job has not run since the upgrade is `Ok(vec![])`.
pub async fn list<'e, E: PgExecutor<'e>>(exec: E) -> DbResult<Vec<PresetDefinition>> {
    let rows = sqlx::query_as!(
        PresetRow,
        "SELECT slug, name, base_url, adapter AS \"adapter: AdapterKind\", \
                config AS \"config: Json\", politeness AS \"politeness: SqlxJson<Politeness>\", \
                updated_at \
         FROM provider_presets ORDER BY slug",
    )
    .fetch_all(exec)
    .await?;
    Ok(rows.into_iter().map(PresetDefinition::from).collect())
}

/// One catalogue entry, or `None` when this build no longer ships it.
///
/// # Errors
/// `Sqlx` only.
pub async fn get<'e, E: PgExecutor<'e>>(exec: E, slug: &str) -> DbResult<Option<PresetDefinition>> {
    let row = sqlx::query_as!(
        PresetRow,
        "SELECT slug, name, base_url, adapter AS \"adapter: AdapterKind\", \
                config AS \"config: Json\", politeness AS \"politeness: SqlxJson<Politeness>\", \
                updated_at \
         FROM provider_presets WHERE slug = $1",
        slug,
    )
    .fetch_optional(exec)
    .await?;
    Ok(row.map(PresetDefinition::from))
}
