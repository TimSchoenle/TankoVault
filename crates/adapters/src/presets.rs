//! Built-in provider presets: ready-to-seed configurations for the sites this build
//! ships support for. `xtask seed` inserts these, and the admin console can copy them.
//!
//! Each Madara preset carries only the selector overrides where the site deviates from
//! [`madara_default_config`](crate::madara_default_config) — onboarding a Madara site is
//! data, not code (design §7). A site with a bespoke layout instead ships a custom Rust
//! adapter and an empty config. See `docs/PROVIDERS.md` for the config-vs-code split.

use serde_json::{Value, json};
use tankovault_domain::{AdapterKind, Politeness};

/// A ready-to-seed provider definition: identity, domain, adapter kind, the selector
/// overrides merged onto the adapter defaults (empty for a fully custom adapter), and the
/// crawl budget the site's size warrants.
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
    /// Seed crawl budget. Operators may tune it downward in the console at any time; the
    /// hard ceilings in [`tankovault_domain::Politeness`] bound it upward regardless.
    pub politeness: Politeness,
}

/// The provider presets bundled with this build.
///
/// - `demonicscans` — **custom Rust adapter** (bespoke PHP layout); no selector config.
/// - `kunmanga` — **custom adapter**: Madara-shaped series HTML, a JSON chapter API, and a
///   sitemap-driven catalogue (its HTML listing is clamped at page 100 server-side).
/// - `manhuaus` — the shared **Madara** adapter plus the handful of selector overrides
///   where the site deviates from the Madara defaults.
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
            politeness: Politeness::default(),
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
            politeness: Politeness::default(),
        },
        // Hybrid: Madara-shaped catalogue/series HTML, but chapters come from a JSON API
        // (`/api/comics/{slug}/chapters`), so it ships a **custom adapter** that reuses the
        // Madara selectors below for HTML parsing and overrides chapter fetching.
        ProviderPreset {
            slug: "kunmanga",
            name: "KunManga",
            base_url: "https://www.kunmanga.co.uk",
            adapter: AdapterKind::Custom,
            config: json!({
                // No `catalog` block: this site's HTML listing is unusable for enumeration
                // (server-clamped at page 100, with an always-rendered "Next"), so
                // `KunMangaAdapter::list_catalog` walks the sitemap shards instead and reads
                // no catalogue selectors at all. See that method for the full reasoning.
                "series": {
                    // The only reliable release-year signal found on this site: a single
                    // link per series page into the year archive.
                    "release": "a[href*=\"manga-release\"]"
                }
            }),
            // Sized for the catalogue, not for a typical site. KunManga carries ~88k series
            // (~175k requests for a full pass, versus ~1.3k series at manhuaus), so the
            // default 1 rps would put a full scan in the two-day range. These are the
            // highest values that still respect the policy ceilings — `rps` and
            // `concurrency` are enforced **per worker process**, so at the shipped two
            // replicas this is 4 rps / 8 in flight aggregate, exactly
            // `Politeness::MAX_RPS` / `MAX_CONCURRENCY`. Raise replica count and these must
            // come down. `crawl_delay_ms` stays 0: the site's robots.txt sets no
            // Crawl-delay, and the site's own 429/`Retry-After` now drives backoff.
            politeness: Politeness {
                rps: 2.0,
                concurrency: 4,
                ..Politeness::default()
            },
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::build_adapter;

    #[test]
    fn every_preset_crawl_budget_is_within_policy() {
        // A preset ships a crawl budget; shipping one the policy ceilings would silently
        // clamp is a packaging bug, and shipping a non-positive one would stall the crawler.
        for p in builtin() {
            assert!(
                p.politeness.rps > 0.0
                    && p.politeness.rps <= tankovault_domain::politeness::MAX_RPS,
                "{}: rps {} outside (0, {}]",
                p.slug,
                p.politeness.rps,
                tankovault_domain::politeness::MAX_RPS
            );
            assert!(
                p.politeness.concurrency > 0
                    && p.politeness.concurrency <= tankovault_domain::politeness::MAX_CONCURRENCY,
                "{}: concurrency {} outside (0, {}]",
                p.slug,
                p.politeness.concurrency,
                tankovault_domain::politeness::MAX_CONCURRENCY
            );
            assert_eq!(
                p.politeness.clone().clamped(),
                p.politeness,
                "{}: clamping changed the shipped budget",
                p.slug
            );
        }
    }

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
