//! Public catalogue response bodies served without authentication. They live here for the
//! reason given in [`crate::admin`].

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

/// A provider entry for the Discover filter list: its identity and how many series it carries,
/// without the operator-only configuration and politeness.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[schema(as = PublicProvider)]
pub struct PublicProviderView {
    /// The provider, which is what a browse filter carries.
    pub id: Uuid,
    /// Its URL-safe key, which is what appears in a shareable filter link.
    pub slug: String,
    /// Its display name.
    pub name: String,
    /// Distinct canonical series with at least one source on this provider.
    pub series_count: i64,
}
