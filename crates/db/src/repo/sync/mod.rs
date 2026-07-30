//! External-sync persistence (design §15): OAuth accounts and canonical-series
//! mappings for a third-party provider such as `AniList`.
//!
//! Token columns hold **ciphertext only** — the sync service seals them with
//! [`tankovault_auth::SecretBox`] before they reach this layer, so nothing here ever handles
//! plaintext credentials. The `provider` column is the external service key (e.g.
//! `"anilist"`), mirroring the shape used by [`tracking`](super::tracking) entries.
//!
//! # Layout (ARCH-5b)
//!
//! Three lifecycles serving two audiences used to share one 1,007-line module:
//!
//! | module | owns | read by |
//! |---|---|---|
//! | [`snapshots`] | the three-way merge's common ancestor | `services/sync` |
//! | [`conflicts`] | conflicts queued under the `ask_me` policy | `services/sync` |
//! | [`history`] | the user-facing transparency log | `services/sync` |
//! | [`accounts`] | linked accounts and their auto-sync settings | `services/sync` |
//! | [`mappings`] | the series ⇆ external id correspondence | `services/sync` |
//! | [`remote_entries`] | what the provider's list actually held | `services/sync` |
//! | [`admin_views`] | the admin console's read models | `services/api` |
//!
//! The modules are public and the glob re-exports below keep every existing
//! `repo::sync::…` path resolving.

pub mod accounts;
pub mod admin_views;
pub mod conflicts;
pub mod history;
pub mod mappings;
pub mod remote_entries;
pub mod snapshots;

pub use accounts::*;
pub use admin_views::*;
pub use conflicts::*;
pub use history::*;
pub use mappings::*;
pub use remote_entries::*;
pub use snapshots::*;
