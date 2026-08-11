//! Built-in provider presets, ready to seed (`xtask seed`). Each Madara preset stores only the
//! selector overrides where the site deviates from [`madara_default_config`](crate::madara_default_config).

use serde_json::{Value, json};
use tankovault_domain::{AdapterKind, Politeness};

/// A ready-to-seed provider definition: identity, domain, adapter kind, the selector
/// overrides merged onto the adapter defaults (empty for a fully custom adapter), and the
/// crawl budget the site's size warrants.
pub struct BuiltinPreset {
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

/// Every provider preset bundled with this build.
///
/// Grouped by how each is onboarded — the grouping is the point: a site on a shared theme is a
/// config row, and only a genuinely bespoke one costs a Rust module.
///
/// This is the definition side of the preset link: `bootstrap seed-providers` records the list
/// in `provider_presets` and re-applies it to every locked provider, so editing an entry here
/// reaches deployments that already carry the row.
#[must_use]
pub fn builtin() -> Vec<BuiltinPreset> {
    let mut all = madara_family();
    all.extend(mangathemesia_family());
    all.extend(manganato_family());
    all.extend(selector_only());
    all.extend(custom_code());
    all
}

/// Providers on the Madara `WordPress` theme: config only, overriding just what differs.
fn madara_family() -> Vec<BuiltinPreset> {
    vec![
        // Standard Madara. Only three deviations from the defaults.
        BuiltinPreset {
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
        // Large manhwa catalogue. Cloudflare-gated, so every fetch goes through the solver.
        BuiltinPreset {
            slug: "toonily",
            name: "Toonily",
            base_url: "https://toonily.com",
            adapter: AdapterKind::Madara,
            config: json!({
                "catalog": {
                    // `/manga/` is not a listing on this site; `/search/` is, and it paginates
                    // on the path. Its paginator does render `a.nextpostslink`, so the default
                    // marker is kept rather than falling back to the yielded-items heuristic.
                    "path": "/search/page/{page}/"
                },
                "series": {
                    // The `<h1>` also holds the theme's HOT/NEW/END badge, so the default
                    // "all descendant text" reading stores `Solo Leveling END` as the
                    // canonical title — which is what the matching key is built from.
                    "title": "div.post-title h1@text"
                    // No `cover` override: this origin is always reached through the solver
                    // (Cloudflare), and the rendered page carries a resolved `src`. Overriding
                    // to `data-src` — correct for manhuaplus below — finds nothing here.
                }
            }),
            politeness: Politeness::default(),
        },
        // Plain Madara, no bot management in front of it — the defaults apply unchanged apart
        // from the path shape.
        BuiltinPreset {
            slug: "mangaread",
            name: "MangaRead",
            base_url: "https://www.mangaread.org",
            adapter: AdapterKind::Madara,
            config: json!({
                "catalog": { "path": "/manga/page/{page}/" }
            }),
            politeness: Politeness::default(),
        },
        BuiltinPreset {
            slug: "manhuaplus",
            name: "Manhua Plus",
            base_url: "https://manhuaplus.com",
            adapter: AdapterKind::Madara,
            config: json!({
                "catalog": { "path": "/manga/page/{page}/" },
                "series": { "cover": "div.summary_image img@data-src" }
            }),
            politeness: Politeness::default(),
        },
    ]
}

/// Providers on the `MangaThemesia` theme.
fn mangathemesia_family() -> Vec<BuiltinPreset> {
    vec![BuiltinPreset {
        slug: "rizzfables",
        name: "Rizz Fables",
        base_url: "https://rizzfables.com",
        adapter: AdapterKind::MangaThemesia,
        config: json!({
            "catalog": {
                // This site lists its whole catalogue — 88 series — on one page and ignores
                // `?page=`, answering every page number with the same document. The theme's
                // paginator markup is commented out for the same reason. Without `pages: 1`
                // the yielded-items fallback never goes false, so the walk re-fetched and
                // re-ingested page 1 until the planner's cap: 20 000 requests for 88 series,
                // and no error anywhere, because every page "succeeded".
                "pages": 1
            }
        }),
        politeness: Politeness::default(),
    }]
}

/// The Manganato clone family — three domains serving the same application, each its own
/// provider because each has its own rate limit, health state and reader-facing links.
fn manganato_family() -> Vec<BuiltinPreset> {
    vec![
        // Three domains serving the same application; each is its own provider because each
        // has its own rate limit, health state and reader-facing links.
        BuiltinPreset {
            slug: "natomanga",
            name: "NatoManga",
            base_url: "https://www.natomanga.com",
            adapter: AdapterKind::Manganato,
            config: json!({}),
            // ~89k series across ~3.7k catalogue pages, and Cloudflare in front: sized like
            // kunmanga rather than the default. Per worker *process*, so at two replicas this
            // is 8 rps aggregate — the policy ceiling.
            politeness: Politeness {
                rps: 4.0,
                concurrency: 8,
                ..Politeness::default()
            },
        },
        BuiltinPreset {
            slug: "mangakakalot",
            name: "Mangakakalot",
            base_url: "https://www.mangakakalot.gg",
            adapter: AdapterKind::Manganato,
            config: json!({}),
            politeness: Politeness {
                rps: 4.0,
                concurrency: 8,
                ..Politeness::default()
            },
        },
        BuiltinPreset {
            slug: "nelomanga",
            name: "NeloManga",
            base_url: "https://www.nelomanga.net",
            adapter: AdapterKind::Manganato,
            config: json!({}),
            politeness: Politeness {
                rps: 4.0,
                concurrency: 8,
                ..Politeness::default()
            },
        },
    ]
}

/// Bespoke layouts that still reduce to selectors, so they need no Rust of their own.
fn selector_only() -> Vec<BuiltinPreset> {
    let mut all = vec![tcbscans()];
    all.push(weebcentral());
    all.push(mangapill());
    all
}

/// Nineteen series, all weekly and high-demand, with no theme underneath.
fn tcbscans() -> BuiltinPreset {
    BuiltinPreset {
        slug: "tcbscans",
        name: "TCB Scans",
        base_url: "https://tcbonepiecechapters.com",
        adapter: AdapterKind::GenericConfig,
        config: json!({
            "catalog": {
                // One page, and the site serves it for any page number. `pages: 1` is what
                // stops the walk — the yielded-items fallback would re-fetch it forever.
                "path": "/projects",
                "pages": 1,
                "item": "a[href^=\"/mangas/\"]",
                "link": "self",
                "title": "img@alt",
                "next": null
            },
            "latest": {
                // `/projects`, not `/`: the home page lists the newest *chapters*, so a feed
                // read from it registered chapter URLs as series paths — and a chapter page
                // has no chapter list, so every one of those series ingested zero chapters.
                // Re-reading all 19 series on a fast scan is free at this catalogue size.
                "path": "/projects",
                "item": "a[href^=\"/mangas/\"]",
                "link": "self",
                "title": "img@alt",
                "chapter": null
            },
            "series": {
                "title": "h1.font-bold",
                "desc": "p.leading-6",
                "cover": "img@src"
            },
            "chapters": {
                "container": "a[href^=\"/chapters/\"]",
                "link": "self",
                "number_from": "text",
                "title": "div.text-gray-500"
            }
        }),
        politeness: Politeness::default(),
    }
}

/// Server-rendered, offset-paginated, and the only site here serving its chapter list from a
/// URL of its own — all three expressible as config.
fn weebcentral() -> BuiltinPreset {
    BuiltinPreset {
        slug: "weebcentral",
        name: "Weeb Central",
        base_url: "https://weebcentral.com",
        adapter: AdapterKind::GenericConfig,
        config: json!({
            "catalog": {
                // Offset paging, not page numbers; `page_size` is what `{offset}` needs.
                "path": "/search/data?offset={offset}&limit=32&sort=Alphabet&order=Ascending&display_mode=Full%20Display",
                "page_size": 32,
                "item": "article",
                "link": "a[href*=\"/series/\"]",
                "title": "a[href*=\"/series/\"]",
                "next": null
            },
            "latest": {
                "path": "/search/data?offset=0&limit=32&sort=Latest%20Updates&order=Descending&display_mode=Full%20Display",
                "item": "article",
                "link": "a[href*=\"/series/\"]",
                "title": "a[href*=\"/series/\"]",
                "chapter": null
            },
            "series": {
                "title": "h1",
                "desc": "li.list-disc, p.whitespace-pre-wrap",
                "cover": "section img@src",
                "status": { "row": "li", "label": "strong", "match": "Status", "value": "span" },
                "author": { "row": "li", "label": "strong", "match": "Author(s)", "value": "span" },
                "tags": "a[href*=\"included_tag=\"]",
                "release": { "row": "li", "label": "strong", "match": "Released", "value": "span" }
            },
            "chapters": {
                // The series page renders no chapter rows at all — they are swapped in from
                // this endpoint. Reading the series page instead yields zero chapters, which
                // no error path reports because an empty list is a valid answer.
                // `{seg:1}` and not `{slug}`: the series path is `/series/{id}/{Name}`, so
                // the *last* segment is the display name and the endpoint is keyed by the
                // id. Keyed by the name it answers 200 with an empty list, and every series
                // ingests zero chapters without anything failing.
                "path": "/series/{seg:1}/full-chapter-list",
                "container": "div.flex.items-center",
                "link": "a[href*=\"/chapters/\"]",
                "number_from": "text",
                "date": "time@datetime"
            }
        }),
        politeness: Politeness::default(),
    }
}

/// No browsable catalogue: search is the only listing and it caps at 100 rows, so enumeration
/// is the sitemap `robots.txt` advertises.
fn mangapill() -> BuiltinPreset {
    BuiltinPreset {
        slug: "mangapill",
        name: "MangaPill",
        base_url: "https://mangapill.com",
        adapter: AdapterKind::GenericConfig,
        config: json!({
            "catalog": {
                "mode": "sitemap",
                "path": "/static/sitemaps/sitemap{page}.xml",
                // In sitemap mode `item` is the substring that marks a `<loc>` as a series.
                "item": "/manga/",
                "link": "",
                "title": "",
                "next": null
            },
            "latest": {
                "path": "/chapters",
                "item": "div.grid > div",
                "link": "a[href*=\"/manga/\"]",
                "title": "a[href*=\"/manga/\"]",
                "chapter": "a[href*=\"/chapters/\"]"
            },
            "series": {
                "title": "h1",
                "desc": "p.text-sm",
                "cover": "img@data-src",
                "tags": "a[href*=\"genre=\"]"
            },
            "chapters": {
                "container": "div#chapters a",
                "link": "self",
                "number_from": "text"
            }
        }),
        politeness: Politeness::default(),
    }
}

/// Providers driven by a bespoke adapter, each for a reason selectors cannot express.
fn custom_code() -> Vec<BuiltinPreset> {
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
        // The reader host and the API host differ for both of these; `base_url` is the reader
        // one so stored paths stay openable, and the adapters name the API absolutely.
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
        BuiltinPreset {
            slug: "comick",
            name: "ComicK",
            base_url: "https://comick.dev",
            adapter: AdapterKind::Custom,
            config: json!({}),
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
