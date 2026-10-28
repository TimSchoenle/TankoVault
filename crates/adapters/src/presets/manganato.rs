//! Owns the presets for the Manganato network: one layout served from several domains.

use serde_json::json;
use tankovault_domain::AdapterKind;

use super::{BuiltinPreset, bulk_budget};

/// Returns the Manganato clone family: six domains serving the same application, each its own
/// provider because each has its own rate limit, health state and reader-facing links.
pub(super) fn manganato_family() -> Vec<BuiltinPreset> {
    vec![
        manganato("natomanga", "NatoManga", "https://www.natomanga.com"),
        manganato(
            "mangakakalot",
            "Mangakakalot",
            "https://www.mangakakalot.gg",
        ),
        manganato("nelomanga", "NeloManga", "https://www.nelomanga.net"),
        manganato("mangabats", "Mangabat", "https://www.mangabats.com"),
        manganato("manganatogg", "Manganato", "https://manganato.gg"),
        manganato(
            "mangakakalove",
            "MangaKakaLove",
            "https://www.mangakakalove.com",
        ),
    ]
}

/// Returns the preset for one domain of the Manganato application, at the catalogue-scale crawl
/// budget the family shares.
///
/// Per worker *process*, so at the shipped two replicas this is the policy ceiling.
fn manganato(slug: &'static str, name: &'static str, base_url: &'static str) -> BuiltinPreset {
    BuiltinPreset {
        slug,
        name,
        base_url,
        adapter: AdapterKind::Manganato,
        config: json!({}),
        politeness: bulk_budget(),
    }
}
