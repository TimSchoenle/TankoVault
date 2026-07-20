//! # tankovault-adapters
//!
//! The provider adapter framework (design §7). Two layers:
//! 1. [`SourceAdapter`] — the behavioural contract every provider satisfies.
//! 2. Config-driven adapters — [`GenericConfigAdapter`] reads CSS selectors and
//!    pagination from `providers.config`, so a Madara-like site is a one-row insert
//!    ([`build_adapter`] + [`madara_default_config`]). A custom site is a small struct
//!    implementing the trait, dispatched by the factory.
//!
//! Adapters own no transport — the fetch stack is injected via [`Ctx`]. All parsing is
//! covered by fixture tests so a markup change fails a test, not production data.

mod config;
mod demonicscans;
mod error;
mod factory;
mod generic;
pub mod html;
mod madara;
pub mod presets;
mod types;

pub use config::AdapterConfig;
pub use demonicscans::DemonicScansAdapter;
pub use error::AdapterError;
pub use factory::build_adapter;
pub use generic::GenericConfigAdapter;
pub use madara::madara_default_config;
pub use presets::{ProviderPreset, builtin as builtin_presets};
pub use types::{
    CatalogItem, CatalogPage, ChapterMeta, Ctx, LatestUpdate, SeriesMeta, SourceAdapter,
};
