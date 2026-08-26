//! Built-in provider presets, ready to seed (`xtask seed`). Each Madara preset stores only the
//! selector overrides where the site deviates from [`madara_default_config`](crate::madara_default_config).

use serde_json::{Value, json};
use tankovault_domain::{AdapterKind, Politeness};

/// A ready-to-seed provider definition.
///
/// Identity, domain and adapter kind, the selector overrides merged onto the adapter defaults
/// (empty for a fully custom adapter), and the crawl budget the site's size warrants.
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
    all.extend(keyoapp_family());
    all.extend(selector_only());
    all.extend(iken_platform());
    all.extend(custom_code());
    all
}

/// Shorthands for the two families that name most of the presets below.
const THEMESIA: AdapterKind = AdapterKind::MangaThemesia;
const KEYOAPP: AdapterKind = AdapterKind::Keyoapp;

/// A site that runs a family theme with no deviation from its defaults at all.
///
/// Most of a family's sites are this: the platform is hosted or the theme is installed
/// unmodified, so the row is pure identity. Spelling them as a helper keeps the deviations that
/// *do* exist visible instead of buried in repeated boilerplate.
fn plain(
    slug: &'static str,
    name: &'static str,
    base_url: &'static str,
    adapter: AdapterKind,
) -> BuiltinPreset {
    BuiltinPreset {
        slug,
        name,
        base_url,
        adapter,
        config: json!({}),
        politeness: Politeness::default(),
    }
}

/// Providers on the Madara `WordPress` theme: config only, overriding just what differs.
fn madara_family() -> Vec<BuiltinPreset> {
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
    ]
}

/// Providers on the `MangaThemesia` theme.
fn mangathemesia_family() -> Vec<BuiltinPreset> {
    vec![
        BuiltinPreset {
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
                    "path": "/series/?page={page}",
                    "pages": 1
                },
                "latest": { "path": "/series/?order=update" }
            }),
            politeness: Politeness::default(),
        },
        // The 2026-08 expansion: the ex-Asura scanlator sites, all on the theme itself. Three
        // things vary across them and nothing else does: a `catalog`/`latest` pair where the
        // install renamed the theme's listing directory (`themesia_in`), the series template's
        // info block (`infotable_series`), and the coin plugin's lock selector where the site
        // sells early access (`coin_gated_chapters`). They are independent, so a site carrying
        // two of them says so at the call site rather than in a helper named after the pair.
        themesia(
            "akazascans",
            "Akaza Scans",
            "https://akazascans.org",
            json!({ "series": infotable_series(), "chapters": coin_gated_chapters() }),
        ),
        // This one and `kingofshojo` below serve the same catalogue from two domains. Both are
        // kept, for the reason the Manganato clones are: each has its own rate limit, health
        // state and reader-facing links, and the matcher collapses the duplicated series into
        // one entry with two sources — which is exactly what a second mirror is worth.
        plain(
            "arenascans",
            "Arena Scans",
            "https://arenascan.com",
            THEMESIA,
        ),
        plain("ragescans", "Rage Scans", "https://ragescans.com", THEMESIA),
        themesia(
            "rokaricomics",
            "Rokari Comics",
            "https://rokaricomics.com",
            json!({ "series": infotable_series(), "chapters": coin_gated_chapters() }),
        ),
        themesia(
            "kingofshojo",
            "King of Shojo",
            "https://kingofshojo.com",
            json!({ "series": infotable_series() }),
        ),
        plain(
            "noxenscans",
            "Noxen Scans",
            "https://noxenscan.com",
            THEMESIA,
        ),
        themesia(
            "mangatrend",
            "Manga Trend",
            "https://mangatrend.org",
            json!({ "series": infotable_series() }),
        ),
        themesia_in(
            "violetscans",
            "Violet Scans",
            "https://violetscans.org",
            "comics",
        ),
        themesia_in(
            "thunderscans",
            "Thunder Scans",
            "https://en-thunderscans.com",
            "comics",
        ),
        themesia_in("razure", "Razure", "https://razure.org", "series"),
    ]
}

/// A `MangaThemesia` preset carrying `config` verbatim, for a site whose deviations do not fit
/// one named helper. The fragments below are the deviations, and they compose because each owns
/// a different top-level section.
fn themesia(
    slug: &'static str,
    name: &'static str,
    base_url: &'static str,
    config: Value,
) -> BuiltinPreset {
    BuiltinPreset {
        slug,
        name,
        base_url,
        adapter: THEMESIA,
        config,
        politeness: Politeness::default(),
    }
}

/// The `chapters` fragment for an install running the coin plugin that sells early access.
///
/// The plugin adds one `span.text-gold` — a coin glyph and the price — to a paid row and leaves
/// the rest of the row alone, so the theme defaults still read it and that badge is the only
/// thing separating a paid chapter from a free one. Without the selector every paid chapter
/// ingested as free, and readers were sent to a paywall as the next unread page.
///
/// No `unlock` selector, because the badge states a price and never a date: a locked row keeps
/// an unknown unlock time and stays locked until a scan sees the badge gone. Same policy as
/// `keyoapp`.
///
/// Not every install of the plugin has this shape. `violetscans` and `thunderscans` render a
/// paid row as a modal trigger with no `href` at all, so the row has no chapter URL to store
/// and is dropped before access is ever read — a different bug that this selector cannot fix.
fn coin_gated_chapters() -> Value {
    json!({ "locked": "span.text-gold" })
}

/// A `MangaThemesia` install that renamed the theme's listing directory. That one rename moves
/// both the catalogue and the feed, since the feed is the same listing re-sorted.
fn themesia_in(
    slug: &'static str,
    name: &'static str,
    base_url: &'static str,
    directory: &str,
) -> BuiltinPreset {
    BuiltinPreset {
        slug,
        name,
        base_url,
        adapter: THEMESIA,
        config: json!({
            "catalog": { "path": format!("/{directory}/?page={{page}}") },
            "latest": { "path": format!("/{directory}/?page=1&order=update") }
        }),
        politeness: Politeness::default(),
    }
}

/// The `series` fragment for an install whose template renders the info block as a two-column
/// `table.infotable` and the genres as `div.seriestugenre`, in place of the theme's
/// `div.imptdt` rows and `span.mgen` list.
///
/// Four of the family's presets ship it and the rest ship the stock block, so it is a `series`
/// override rather than a family: catalogue, feed, title, cover, description and chapter list
/// are the theme's own markup either way. On the stock defaults these installs parsed a title, a
/// cover and a description and nothing else — no status, no genres, no credits, and no error,
/// because a selector that matches nothing is how the theme's optional rows are meant to read.
///
/// The table also carries a `Type` row (Manga/Manhwa/Manhua) that stays unread:
/// `GenericConfigAdapter` publishes `ContentType::Unknown` unconditionally and there is no
/// config field to point at it.
fn infotable_series() -> Value {
    /// One labelled row of `table.infotable`, matched by the label cell's text.
    fn row(label: &str) -> Value {
        json!({
            "row": "table.infotable tr",
            // Explicitly null, not omitted: the family default's `author` is itself a labelled
            // row and the config merge is per-key, so an omitted `label` inherits its
            // `div.imptdt` selector, finds nothing, and the row never matches.
            //
            // Null is also the shape that works here. A `label` selector is compared for
            // equality, and these installs disagree on the wording — `Alternative` on one,
            // `Alternative Names` on the others. With no `label` the row's own text is matched
            // as a prefix, which covers both spellings.
            "label": null,
            "match": label,
            // The *second* cell. `td` alone selects the label and stores it as the value; that
            // is the trap `tests/family_presets_fixture.rs` pins for every family with a
            // labelled block.
            "value": "td:nth-of-type(2)"
        })
    }

    json!({
        "tags": "div.seriestugenre a",
        "status": row("Status"),
        "alt": row("Alternative"),
        // Only some of these installs fill an `Author` row; the rest publish credits nowhere,
        // and the row is absent rather than empty. What they all publish is `Posted By` — the
        // WordPress account that uploaded the post, never the creator. Matching it would write
        // a site handle into `series` credits, where the recommender treats a name shared by
        // hundreds of unrelated series as a strong signal.
        "author": row("Author"),
        // Cleared, not repointed. The defaults name `div.imptdt` positions that do not exist
        // here, and the only date rows this table has — `Posted On`, `Updated On` — are
        // WordPress post timestamps, not the year of first publication.
        "artist": null,
        "release": null
    })
}

/// Providers on the Keyoapp platform: hosted, so every install is the stock layout and each
/// site is identity alone.
fn keyoapp_family() -> Vec<BuiltinPreset> {
    // `sirenscans` was the tenth and is retired: its origin answers a bare nginx `404` on every
    // route — `/` included — through a fully solved real browser, while a sibling install answers
    // the same request from the same session normally. That is the origin being gone, not a block
    // and not the flap the whole family is prone to. A preset kept for a dead host spends a crawl
    // budget and fills the console's error feed every cycle; `retire_missing` drops the definition
    // and deliberately leaves any installed provider row alone for the operator to pause.
    vec![
        plain(
            "asmotoon",
            "Asmodeus Scans",
            "https://asmotoon.com",
            KEYOAPP,
        ),
        plain("genztoons", "Genz Toons", "https://genztoons.org", KEYOAPP),
        plain(
            "timelesstoons",
            "Timeless Toons",
            "https://timelesstoons.org",
            KEYOAPP,
        ),
        plain("mistscans", "Mist Scans", "https://mistscans.com", KEYOAPP),
        plain("grimscans", "Grim Scans", "https://grimscans.com", KEYOAPP),
        plain("kewnscans", "Kewn Scans", "https://kewnscans.org", KEYOAPP),
        plain(
            "writerscans",
            "Writer Scans",
            "https://writerscans.com",
            KEYOAPP,
        ),
        plain("nyanukafe", "Nyanu Kafe", "https://nyanukafe.com", KEYOAPP),
    ]
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
fn iken_platform() -> Vec<BuiltinPreset> {
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
        // Iken, added with the 2026-08 expansion; see `iken_platform` below.
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
        // Was a MangaThemesia row at `witchscans.com` until the group rebuilt on its own Next.js
        // platform and moved to `witchtoons.net`. The slug stays: it keys the rate limit, the
        // provider row and every stored source, and the site is the same publisher.
        BuiltinPreset {
            slug: "witchscans",
            name: "WitchToons",
            base_url: "https://witchtoons.net",
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
