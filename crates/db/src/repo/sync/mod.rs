//! External-sync persistence: OAuth accounts and canonical-series mappings per provider.
//!
//! Token columns hold **ciphertext only** — sealed by `tankovault_auth::Sealer` before reaching
//! this layer.

pub mod accounts;
pub mod admin_views;
pub mod conflicts;
pub mod decisions;
pub mod history;
pub mod mappings;
pub mod remote_entries;
pub mod snapshots;

pub use accounts::*;
pub use admin_views::*;
pub use conflicts::*;
pub use decisions::*;
pub use history::*;
pub use mappings::*;
pub use remote_entries::*;
pub use snapshots::*;
