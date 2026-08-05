//! The `metadata` section as far as every writer of series metadata cares: who owns each field.

use serde::Deserialize;
use tankovault_domain::MetadataPriority;

/// Per-field source authority, read by **both** the worker's ingest path and external sync's
/// enrichment writer.
///
/// Shared deliberately, for the same reason as [`crate::MatchingConfig`]: the two paths write
/// the same columns, and a priority only one of them consults is not a priority. The sync
/// service composes this section with its own enrichment-sweep tunables.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct MetadataPriorityConfig {
    /// Per-field source authority order (default: `AniList` before the adapters).
    #[serde(default)]
    pub priority: MetadataPriority,
}
