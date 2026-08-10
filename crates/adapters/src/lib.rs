//! Provider adapter framework: [`SourceAdapter`] is the trait every provider implements, and
//! [`GenericConfigAdapter`] drives config-driven providers from `providers.config` selectors.

mod astro;
mod comick;
mod config;
mod demonicscans;
mod diagnostics;
mod error;
mod factory;
mod flamecomics;
mod generic;
mod heancms;
pub mod html;
mod json;
mod kunmanga;
mod madara;
mod mangadex;
mod manganato;
mod mangathemesia;
pub mod presets;
mod types;
mod webtoons;

pub use astro::{AstroFlavour, AstroIslandAdapter};
pub use comick::ComickAdapter;
pub use config::AdapterConfig;
pub use demonicscans::DemonicScansAdapter;
pub use error::AdapterError;
pub use factory::build_adapter;
pub use flamecomics::FlameComicsAdapter;
pub use heancms::HeanCmsAdapter;
pub use generic::GenericConfigAdapter;
pub use kunmanga::KunMangaAdapter;
pub use madara::madara_default_config;
pub use mangadex::MangaDexAdapter;
pub use manganato::{ManganatoAdapter, manganato_default_config};
pub use mangathemesia::mangathemesia_default_config;
pub use presets::{ProviderPreset, builtin as builtin_presets};
pub use webtoons::WebtoonsAdapter;
pub use types::{
    CatalogItem, CatalogPage, ChapterAccess, ChapterMeta, Ctx, LatestUpdate, SeriesMeta,
    SourceAdapter,
};

/// Seams for the out-of-workspace fuzz crate, which can only reach `pub` items.
///
/// [`json::parse_json_body`] stays `pub(crate)` and is re-exported here `#[doc(hidden)]` instead,
/// so its candidate scan (a past quadratic-DoS site) stays fuzzable without becoming public API.
#[doc(hidden)]
pub mod __fuzz {
    use crate::error::AdapterError;
    use tankovault_fetch::FetchResponse;

    /// [`crate::json::parse_json_body`] monomorphised at `T = serde_json::Value`: the oracle is
    /// the candidate scan's time/memory, not the parse result.
    ///
    /// # Errors
    /// Whatever `parse_json_body` returns.
    pub fn parse_json_body_value(
        what: &str,
        resp: &FetchResponse,
    ) -> Result<serde_json::Value, AdapterError> {
        crate::json::parse_json_body(what, resp)
    }
}
