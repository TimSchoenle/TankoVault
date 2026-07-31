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
mod diagnostics;
mod error;
mod factory;
mod generic;
pub mod html;
mod json;
mod kunmanga;
mod madara;
pub mod presets;
mod types;

pub use config::AdapterConfig;
pub use demonicscans::DemonicScansAdapter;
pub use error::AdapterError;
pub use factory::build_adapter;
pub use generic::GenericConfigAdapter;
pub use kunmanga::KunMangaAdapter;
pub use madara::madara_default_config;
pub use presets::{ProviderPreset, builtin as builtin_presets};
pub use types::{
    CatalogItem, CatalogPage, ChapterMeta, Ctx, LatestUpdate, SeriesMeta, SourceAdapter,
};

/// Seams the out-of-workspace fuzz crate needs, and that nothing else should use.
///
/// `fuzz/fuzz_targets/*` is a separate, nightly-only workspace (`fuzz/README.md`), so it can
/// only reach `pub` items. Everything the fuzz targets drive is already public except one
/// thing: [`json::parse_json_body`] is `pub(crate)` because it is an implementation detail of
/// the JSON-shaped adapters — and F-02, a *verified* quadratic DoS in its candidate scan, is
/// exactly the bug class a libFuzzer `-timeout` oracle exists to catch.
///
/// So it is re-exported here rather than promoted: `#[doc(hidden)]`, under a name no reader
/// will mistake for API, monomorphised at the single type a fuzz target needs. Widening
/// `parse_json_body` itself would have made a private helper part of this crate's contract to
/// serve a test.
#[doc(hidden)]
pub mod __fuzz {
    use crate::error::AdapterError;
    use tankovault_fetch::FetchResponse;

    /// [`crate::json::parse_json_body`] at `T = serde_json::Value`.
    ///
    /// The choice of `T` does not weaken the target. A *successful* parse is uninteresting
    /// (`Value` accepts any well-formed JSON); the oracle is how much time and memory the
    /// candidate scan spends reaching an answer at all, and that scan runs over the body
    /// before `T` is ever consulted.
    ///
    /// # Errors
    /// Whatever `parse_json_body` returns; a fuzz target discards it.
    pub fn parse_json_body_value(
        what: &str,
        resp: &FetchResponse,
    ) -> Result<serde_json::Value, AdapterError> {
        crate::json::parse_json_body(what, resp)
    }
}
