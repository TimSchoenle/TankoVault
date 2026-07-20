//! Built-in provider presets: ready-to-seed configurations for the sites this build
//! ships support for. `xtask seed` inserts these, and the admin console can copy them.
//!
//! Each Madara preset carries only the selector overrides where the site deviates from
//! [`madara_default_config`](crate::madara_default_config) — onboarding a Madara site is
//! data, not code (design §7). A site with a bespoke layout instead ships a custom Rust
//! adapter and an empty config. See `docs/PROVIDERS.md` for the config-vs-code split.

use tankovault_domain::AdapterKind;
use serde_json::{Value, json};

/// A ready-to-seed provider definition: identity, domain, adapter kind, and the selector
/// overrides merged onto the adapter defaults (empty for a fully custom adapter).
pub struct ProviderPreset {
    /// Stable slug (rate-limit + custom-adapter dispatch key).
    pub slug: &'static str,
    /// Human-readable display name.
    pub name: &'static str,
    /// Domain root; every stored relative link resolves against it.
    pub base_url: &'static str,
    /// Which adapter drives the site.
    pub adapter: AdapterKind,
    /// `providers.config` overrides (merged onto Madara defaults; ignored for `Custom`).
    pub config: Value,
}

/// The provider presets bundled with this build.
///
/// - `demonicscans` — **custom Rust adapter** (bespoke PHP layout); no selector config.
/// - `manhuaus`, `kunmanga` — the shared **Madara** adapter plus the handful of selector
///   overrides where each site deviates from the Madara defaults.
#[must_use]
pub fn builtin() -> Vec<ProviderPreset> {
    vec![
        // Bespoke PHP layout — driven by `DemonicScansAdapter`, dispatched on this slug.
        ProviderPreset {
            slug: "demonicscans",
            name: "Demonic Scans",
            base_url: "https://demonicscans.org",
            adapter: AdapterKind::Custom,
            config: json!({}),
        },
        // Standard Madara. Only two deviations from the defaults.
        ProviderPreset {
            slug: "manhuaus",
            name: "Manhuaus",
            base_url: "https://manhuaus.com",
            adapter: AdapterKind::Madara,
            config: json!({
                "catalog": {
                    // Paginates as `/manga/page/{n}/`; the theme omits `a.nextpostslink`,
                    // so the <head> rel=next link is the reliable has-next marker.
                    "path": "/manga/page/{page}/",
                    "next": "link[rel=next]"
                },
                "series": {
                    // Covers are lazy-loaded — the real URL lives in `data-src`.
                    "cover": "div.summary_image img@data-src"
                }
            }),
        },
        // Madara with an ad-injected catalogue and Bootstrap pagination.
        ProviderPreset {
            slug: "kunmanga",
            name: "KunManga",
            base_url: "https://www.kunmanga.co.uk",
            adapter: AdapterKind::Madara,
            config: json!({
                "catalog": {
                    "path": "/manga/page/{page}",
                    // Skip injected advertisement tiles; the paginator's aria-labelled
                    // "Next" control marks additional pages.
                    "item": "div.page-item-detail:not(.custom-item-ad)",
                    "next": "a[aria-label=\"Next\"]"
                },
                "chapters": {
                    // Chapter rows are <div>, not the Madara-default <li>.
                    "container": "div.wp-manga-chapter"
                }
            }),
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::build_adapter;

    #[test]
    fn every_preset_builds_an_adapter() {
        // A preset that cannot be turned into a live adapter is a packaging bug.
        for p in builtin() {
            build_adapter(p.adapter, p.slug, &p.config)
                .unwrap_or_else(|e| panic!("preset {:?} failed to build: {e}", p.slug));
        }
    }

    #[test]
    fn slugs_are_unique() {
        let mut slugs: Vec<_> = builtin().iter().map(|p| p.slug).collect();
        slugs.sort_unstable();
        let len = slugs.len();
        slugs.dedup();
        assert_eq!(slugs.len(), len, "preset slugs must be unique");
    }
}
