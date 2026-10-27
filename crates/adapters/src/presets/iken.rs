//! Presets on the Iken platform.

use serde_json::json;
use tankovault_domain::{AdapterKind, Politeness};

use super::BuiltinPreset;

/// An Iken-hosted site. The API host is `api.` prefixed onto the reader host on every one of
/// them, but it is stated rather than derived: the adapter must not have to guess where a
/// deployment put its API, and the config is the one place a move is fixable without a release.
fn iken(slug: &'static str, name: &'static str, base_url: &'static str) -> BuiltinPreset {
    BuiltinPreset {
        slug,
        name,
        base_url,
        adapter: AdapterKind::Custom,
        config: json!({ "api": base_url.replace("https://", "https://api.") }),
        politeness: Politeness::default(),
    }
}

/// The Iken platform: one hosted back end behind several scanlator sites, each serving its JSON
/// from a sibling host the preset names. `base_url` stays the reader host so stored paths remain
/// openable. Not a `family`, because a family merges *selectors* onto defaults and there is no
/// markup here to select.
pub(super) fn iken_platform() -> Vec<BuiltinPreset> {
    vec![
        iken("vortexscans", "Vortex Scans", "https://vortexscans.org"),
        iken("magustoon", "Magus Manga", "https://magustoon.org"),
        iken("nyxscans", "Nyx Scans", "https://nyxscans.com"),
        iken("kencomics", "Ken Scans", "https://kencomics.com"),
        iken("sanascans", "Sana Scans", "https://sanascans.com"),
        iken("orionscans", "Orion Scans", "https://orion-scans.com"),
        iken("renascans", "Rena Scans", "https://renascans.net"),
        iken("kaynscans", "Kayn Scans", "https://kaynscan.org"),
        iken("hijalascans", "Hijala Scans", "https://en-hijala.com"),
    ]
}
