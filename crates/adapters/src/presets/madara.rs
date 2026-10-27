//! Presets on the Madara `WordPress` theme.

use serde_json::json;
use tankovault_domain::{AdapterKind, Politeness};

use super::{BuiltinPreset, MADARA, plain};

/// Providers on the Madara `WordPress` theme: config only, overriding just what differs.
#[expect(
    clippy::too_many_lines,
    reason = "one entry per site, and the family's list is the point: splitting it by \
              expansion batch would put half the family somewhere nobody adding a site looks"
)]
pub(super) fn madara_family() -> Vec<BuiltinPreset> {
    vec![
        // Standard Madara; covers are the only deviation from the defaults.
        BuiltinPreset {
            slug: "manhuaus",
            name: "Manhuaus",
            base_url: "https://manhuaus.com",
            adapter: AdapterKind::Madara,
            config: json!({
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
                    // on the path. Unlike the rest of the family this one *does* render the
                    // theme's `a.nextpostslink`, so it names the marker rather than leaning on
                    // the family default's yielded-items fallback.
                    "path": "/search/page/{page}/",
                    "next": "a.nextpostslink"
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
        // Plain Madara, no bot management in front of it: the defaults apply unchanged.
        plain(
            "mangaread",
            "MangaRead",
            "https://www.mangaread.org",
            AdapterKind::Madara,
        ),
        BuiltinPreset {
            slug: "manhuaplus",
            name: "Manhua Plus",
            base_url: "https://manhuaplus.com",
            adapter: AdapterKind::Madara,
            config: json!({
                "series": { "cover": "div.summary_image img@data-src" }
            }),
            politeness: Politeness::default(),
        },
        // The 2026-08 expansion. Only one of the candidates survived, and that is a finding
        // rather than a shortfall: most live Madara installs render their chapter list from
        // `POST {series}/ajax/chapters/` and serve a series page with no `li.wp-manga-chapter`
        // in it at all. The fetch stack is GET-only by construction, so such a site parses a
        // perfect catalogue and a perfect feed and then ingests zero chapters — silently,
        // because an empty list is a valid answer. They stay out until that capability exists.
        BuiltinPreset {
            slug: "yakshacomics",
            name: "Yaksha Comics",
            base_url: "https://yakshacomics.com",
            adapter: AdapterKind::Madara,
            config: json!({
                "catalog": {
                    // The one site here that must name a marker, because the family default's
                    // fallback cannot terminate on this host: it answers **every** 404 with a
                    // Cloudflare challenge the solver cannot clear, so the empty page that ends
                    // a walk is never reachable — the request that should say "stop" fails
                    // instead, and the scan task is retried forever after ingesting the whole
                    // catalogue correctly. This install does render `link[rel=next]`, present on
                    // every page but the last, so the walk ends without asking for a page past
                    // the end at all.
                    "next": "link[rel=\"next\"]"
                }
            }),
            politeness: Politeness::default(),
        },
        // The 2026-08-26 expansion. Every entry here was checked for a **server-rendered**
        // chapter list before it was written down, because the AJAX limitation above is the
        // family's real filter: of ~70 live English installs surveyed, three in four render no
        // `wp-manga-chapter` at all and would have ingested a perfect catalogue and zero
        // chapters. The ones that survived are below.
        plain("bunmanga", "Bun Manga", "https://bunmanga.com", MADARA),
        BuiltinPreset {
            slug: "linkmanga",
            name: "LinkManga",
            base_url: "https://linkmanga.com",
            adapter: MADARA,
            // A page past the end answers `200` and renders a *popular-titles* grid in the
            // archive's own container, so the yielded-items fallback sees twenty items and
            // never stops. The `<head>` marker is the one thing that does go away there.
            config: json!({ "catalog": { "next": "link[rel=\"next\"]" } }),
            politeness: Politeness::default(),
        },
        madara_in("madaradex", "MadaraDex", "https://madaradex.org", "title"),
        madara_in(
            "mangadistrict",
            "Manga District",
            "https://mangadistrict.com",
            "series",
        ),
        plain("mangahe", "MangaHe", "https://mangahe.com", MADARA),
        plain("mangazin", "Zin Manga", "https://mangazin.org", MADARA),
        plain("manhuahot", "ManhuaHot", "https://manhuahot.com", MADARA),
        plain("s2manga", "S2Manga", "https://s2read.com", MADARA),
        BuiltinPreset {
            slug: "toongod",
            name: "ToonGod",
            base_url: "https://www.toongod.org",
            adapter: MADARA,
            config: json!({
                "catalog": { "path": "/webtoon/page/{page}/" },
                // The one install here whose home page renders no card at all; the archive's
                // first page is the feed instead.
                "latest": { "path": "/webtoon/" }
            }),
            politeness: Politeness::default(),
        },
        madara_in(
            "webtoonscan",
            "WebtoonScan",
            "https://webtoonscan.com",
            "manhwa",
        ),
        madara_in(
            "webtoonxyz",
            "Webtoon XYZ",
            "https://www.webtoon.xyz",
            "read",
        ),
        BuiltinPreset {
            slug: "zazamanga",
            name: "Zazamanga",
            base_url: "https://www.zazamanga.com",
            adapter: MADARA,
            // Same platform as `zinmanga`/`kunmangaonline` (shared cover CDN), but this install
            // still server-renders the whole chapter list — as `div`, not the theme's `li` — so
            // it stays a config row rather than taking their sitemap adapter, whose JSON chapter
            // API this host answers with a 404.
            config: json!({
                "catalog": {
                    // Each page renders its twelve cards **twice**, as a grid copy and a list
                    // copy. Only the list copy carries `h3 a`, so on the family default half the
                    // harvested items had no title at all. This class is the list copy's.
                    "item": "div.page-item-detail.manga",
                    // The listing is clamped server-side at page 100 — every higher page number
                    // returns the byte-identical page-100 document, on every route and every
                    // sort — while the paginator advertises ~7 000 pages, so the theme's own
                    // next-marker is still enabled there and no content test can tell the
                    // difference. The cap is the only thing that ends this walk. It bounds the
                    // reachable catalogue at 1 200 of the site's ~78 000 series; the rest are
                    // not addressable through any listing this host serves.
                    "pages": 100
                },
                "chapters": { "container": "div.wp-manga-chapter" }
            }),
            politeness: Politeness::default(),
        },
    ]
}

/// A Madara install that renamed the theme's archive directory.
///
/// Only the catalogue moves: every install here still renders its updates on the home page, so
/// the family's `latest` block is untouched — unlike `MangaThemesia`, where the feed *is* the
/// renamed listing re-sorted.
fn madara_in(
    slug: &'static str,
    name: &'static str,
    base_url: &'static str,
    directory: &str,
) -> BuiltinPreset {
    BuiltinPreset {
        slug,
        name,
        base_url,
        adapter: MADARA,
        config: json!({ "catalog": { "path": format!("/{directory}/page/{{page}}/") } }),
        politeness: Politeness::default(),
    }
}
