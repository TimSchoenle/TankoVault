//! Public catalogue response bodies served without authentication.
//!
//! Here for the same reason as [`crate::admin`]: the shape was a repository row struct
//! carrying `ToSchema`, so a `SELECT` column rename rewrote the public API with no compile
//! error. See that module's header for the full reasoning.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

/// A provider entry for the Discover filter list: identity plus how many distinct series it
/// carries, so the UI can show "Provider (N)" options without exposing operator-only
/// config/politeness.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[schema(as = PublicProvider)]
pub struct PublicProviderView {
    pub id: Uuid,
    pub slug: String,
    pub name: String,
    /// Distinct canonical series with at least one source on this provider.
    pub series_count: i64,
}
