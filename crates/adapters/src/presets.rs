//! Built-in provider presets, ready to seed (`xtask seed`). Each Madara preset stores only the
//! selector overrides where the site deviates from [`madara_default_config`](crate::madara_default_config).

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

/// The provider presets bundled with this build: `demonicscans` and `kunmanga` are custom Rust
/// adapters (bespoke layout / hybrid JSON+HTML), `manhuaus` is the shared Madara adapter with a
/// few selector overrides.
#[must_use]
pub fn builtin() -> Vec<ProviderPreset> {
    vec![
        // Bespoke PHP layout, driven by `DemonicScansAdapter`, dispatched on this slug.
        ProviderPreset {
            slug: "demonicscans",
            name: "Demonic Scans",
            base_url: "https://demonicscans.org",
            adapter: AdapterKind::Custom,
            config: json!({}),
            politeness: Politeness::default(),
        },
        // Standard Madara. Only three deviations from the defaults.
        ProviderPreset {
            slug: "manhuaus",
            name: "Manhuaus",
            base_url: "https://manhuaus.com",
            adapter: AdapterKind::Madara,
            config: json!({
                "catalog": {
                    // Paginates as `/manga/page/{n}/` (page 1 redirects to `/manga/`).
                    "path": "/manga/page/{page}/",
                    // `next` is null on purpose: this theme's paginator is an always-rendered
                    // AJAX button, not a page marker, so any selector here either loops forever
                    // or (as a stale `link[rel=next]` once did) matches nothing and silently
                    // truncates the scan. `list_catalog` falls back instead to "another page
                    // exists while this one yielded items", exact here since the 404 shell past
                    // the last page renders zero items.
                    "next": null
                },
                "series": {
                    // Covers are lazy-loaded — the real URL lives in `data-src`.
                    "cover": "div.summary_image img@data-src"
                }
            }),
            politeness: Politeness::default(),
        },
        // Hybrid: Madara HTML for catalogue/series, JSON API for chapters; a custom adapter
        // reuses the Madara selectors below and overrides only chapter fetching.
        ProviderPreset {
            slug: "kunmanga",
            name: "KunManga",
            base_url: "https://www.kunmanga.co.uk",
            adapter: AdapterKind::Custom,
            config: json!({
                // No `catalog` block: HTML listing is server-clamped at page 100 with an
                // always-rendered "Next", so `list_catalog` walks the sitemap shards instead.
                "series": {
                    // Only reliable release-year signal on this site: one link into the year
                    // archive.
                    "release": "a[href*=\"manga-release\"]"
                }
            }),
            // Sized for KunManga's much larger catalogue. `rps`/`concurrency` are enforced per
            // worker process, so at the shipped two replicas this is 4 rps / 8 in flight
            // aggregate — exactly `MAX_RPS`/`MAX_CONCURRENCY`. Raising replica count without
            // lowering these silently exceeds the policy ceiling. `crawl_delay_ms` stays 0:
            // robots.txt sets no Crawl-delay, and 429/`Retry-After` now drives backoff.
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
        // A shipped budget outside policy ceilings would be silently clamped; catch it here.
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
