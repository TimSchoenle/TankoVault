//! Sites with an adapter of their own in Rust.

use serde_json::json;
use tankovault_domain::{AdapterKind, Politeness};

use super::{BuiltinPreset, bulk_budget};

/// Providers driven by a bespoke adapter, each for a reason selectors cannot express.
pub(super) fn custom_code() -> Vec<BuiltinPreset> {
    vec![
        // Bespoke PHP layout, driven by `DemonicScansAdapter`, dispatched on this slug.
        BuiltinPreset {
            slug: "demonicscans",
            name: "Demonic Scans",
            base_url: "https://demonicscans.org",
            adapter: AdapterKind::Custom,
            config: json!({}),
            politeness: Politeness::default(),
        },
        // Hybrid: Madara HTML for catalogue/series, JSON API for chapters; a custom adapter
        // reuses the Madara selectors below and overrides only chapter fetching.
        BuiltinPreset {
            slug: "kunmanga",
            name: "KunManga",
            base_url: "https://www.kunmanga.co.uk",
            adapter: AdapterKind::Custom,
            config: json!({
                // No `catalog` block: HTML listing is server-clamped at page 100 with an
                // always-rendered "Next", so `list_catalog` walks the sitemap shards instead.
                "latest": {
                    // This theme renders no `div.page-item-detail`, so the inherited Madara
                    // defaults matched nothing and every fast scan read an empty feed. The
                    // site's updates live in the home page's "Manga Updates!" slider.
                    "item": "div.manga-item",
                    // The anchor wraps only the cover image: no link text, so the title is
                    // readable solely from the image's `alt` (site suffix and all).
                    "title": "img@alt",
                    // Explicitly none, to override the inherited `span.chapter a`: the slider
                    // carries no chapter label, and a selector that can never match reads as a
                    // live rule rather than an absent one. Costs nothing downstream — both
                    // consumers of `list_latest` re-ingest by `path` and ignore the rest.
                    "chapter": null
                },
                "series": {
                    // Only reliable release-year signal on this site: one link into the year
                    // archive.
                    "release": "a[href*=\"manga-release\"]"
                }
            }),
            // Sized for KunManga's much larger catalogue. `rps`/`concurrency` are enforced per
            // worker process, so at the shipped two replicas this is 8 rps / 16 in flight
            // aggregate — exactly `MAX_RPS`/`MAX_CONCURRENCY`. Raising replica count without
            // lowering these silently exceeds the policy ceiling. `crawl_delay_ms` stays 0:
            // robots.txt sets no Crawl-delay, and 429/`Retry-After` now drives backoff.
            politeness: Politeness {
                rps: 4.0,
                concurrency: 8,
                ..Politeness::default()
            },
        },
        // Two more installs of the platform `kunmanga` runs on, found by the shape of their
        // sitemap index (`sitemap-comic-{n}.xml`) and confirmed by the chapter API answering.
        // Same adapter, same reasons: the HTML listing is server-clamped and the series page
        // renders only the newest rows, so both the walk and the chapter list come from
        // elsewhere. `zazamanga` is the same platform and is *not* here — that install still
        // server-renders its whole chapter list, so it stays a Madara config row.
        zinmanga_platform("zinmanga", "Zinmanga", "https://www.zinmanga.net"),
        zinmanga_platform(
            "kunmangaonline",
            "Kun Manga Online",
            "https://www.kunmanga.online",
        ),
        // The reader host and the API host differ; `base_url` is the reader one so stored paths
        // stay openable, and the adapter names the API absolutely.
        BuiltinPreset {
            slug: "mangadex",
            name: "MangaDex",
            base_url: "https://mangadex.org",
            adapter: AdapterKind::Custom,
            config: json!({}),
            // The API documents ~5 requests/second per IP, enforced at the load balancer. At the
            // shipped two worker replicas this is 4 rps aggregate, comfortably inside it.
            politeness: Politeness {
                rps: 2.0,
                concurrency: 4,
                ..Politeness::default()
            },
        },
        // HeanCMS. `api` is required by the adapter and cannot be derived from `base_url`.
        BuiltinPreset {
            slug: "omegascans",
            name: "Omega Scans",
            base_url: "https://omegascans.org",
            adapter: AdapterKind::Custom,
            config: json!({ "api": "https://api.omegascans.org" }),
            politeness: Politeness::default(),
        },
        // Iken; the other Iken sites are in `iken.rs`.
        BuiltinPreset {
            slug: "asura",
            name: "Asura Scans",
            base_url: "https://asurascans.com",
            adapter: AdapterKind::Custom,
            config: json!({}),
            politeness: Politeness::default(),
        },
        BuiltinPreset {
            slug: "hivetoons",
            name: "Hive Toons",
            base_url: "https://hivetoons.org",
            adapter: AdapterKind::Custom,
            config: json!({}),
            politeness: Politeness::default(),
        },
        BuiltinPreset {
            slug: "flamecomics",
            name: "Flame Comics",
            base_url: "https://flamecomics.xyz",
            adapter: AdapterKind::Custom,
            config: json!({}),
            politeness: Politeness::default(),
        },
        // The Init Manga WordPress theme. Selectors would cover catalogue, feed and metadata,
        // but its series page server-renders only the newest 24 chapter rows — the rest are a
        // REST call keyed by the numeric post id, which only that page states.
        BuiltinPreset {
            slug: "mgread",
            name: "Mgread",
            base_url: "https://mgread.io",
            adapter: AdapterKind::Custom,
            config: json!({}),
            // ~6 400 series over 266 catalogue pages, served straight from the origin with no
            // bot management in front of it. The default budget walks that in one full scan
            // without leaning on a host that is answering every request itself.
            politeness: Politeness::default(),
        },
        // The only licensed source here. Its `robots.txt` disallows `/*/search`, which is why
        // the adapter enumerates by genre.
        BuiltinPreset {
            slug: "webtoons",
            name: "WEBTOON",
            base_url: "https://www.webtoons.com",
            adapter: AdapterKind::Custom,
            config: json!({}),
            politeness: Politeness::default(),
        },
    ]
}

/// A site on the platform `kunmanga` runs on: Madara-shaped series markup, a JSON chapter API,
/// and a catalogue reachable only through the sitemap.
///
/// The config mirrors `kunmanga`'s because the platform is the same one; see that preset for why
/// each override exists.
fn zinmanga_platform(
    slug: &'static str,
    name: &'static str,
    base_url: &'static str,
) -> BuiltinPreset {
    BuiltinPreset {
        slug,
        name,
        base_url,
        adapter: AdapterKind::Custom,
        config: json!({
            // No `latest` override, unlike `kunmanga`: these two installs render the theme's own
            // `div.page-item-detail` cards on the home page, where kunmanga.co.uk renders a
            // bespoke slider. Inheriting kunmanga's override here read an empty feed on every
            // fast scan — a valid parse of the wrong markup, which nothing reports.
            "series": { "release": "a[href*=\"manga-release\"]" }
        }),
        politeness: bulk_budget(),
    }
}
