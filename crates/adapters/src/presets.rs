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

/// Shorthands for the families that name most of the presets below.
const MADARA: AdapterKind = AdapterKind::Madara;
const THEMESIA: AdapterKind = AdapterKind::MangaThemesia;
const KEYOAPP: AdapterKind = AdapterKind::Keyoapp;
const GENERIC: AdapterKind = AdapterKind::GenericConfig;

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
#[expect(
    clippy::too_many_lines,
    reason = "one entry per site, and the family's list is the point: splitting it by \
              expansion batch would put half the family somewhere nobody adding a site looks"
)]
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
        // The 2026-08-26 expansion. Every entry here was checked for a **server-rendered**
        // chapter list before it was written down, because the AJAX limitation above is the
        // family's real filter: of ~70 live English installs surveyed, three in four render no
        // `wp-manga-chapter` at all and would have ingested a perfect catalogue and zero
        // chapters. The ones that survived are below.
        BuiltinPreset {
            slug: "brainrotcomics",
            name: "Brainrot Comics",
            base_url: "https://brainrotcomics.com",
            adapter: MADARA,
            // Must name a marker: this install answers the page after its last with a hard
            // **404**, not the 200 error-shell the family default's yielded-items fallback
            // relies on, so the request that should end the walk fails instead and the scan
            // task is retried forever after ingesting the catalogue correctly.
            //
            // `div.nav-previous` and not `link[rel=next]`, which this theme never emits.
            // WordPress names its paginator by chronology, so "previous" is the *older* page —
            // the one further into the archive. Present on pages 1 and 2, absent on the last.
            config: json!({ "catalog": { "next": "div.nav-previous" } }),
            politeness: Politeness::default(),
        },
        plain("bunmanga", "Bun Manga", "https://bunmanga.com", MADARA),
        plain("dragontea", "DragonTea", "https://dragontea.ink", MADARA),
        madara_in(
            "gourmetscans",
            "Gourmet Supremacy",
            "https://gourmetsupremacy.com",
            "project",
        ),
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
            slug: "setsuscans",
            name: "Setsu Scans",
            base_url: "https://setsuscans.com",
            adapter: MADARA,
            config: json!({
                // Same hard-404 past the last page as `brainrotcomics`, so the walk needs a
                // marker rather than the family default's yielded-items fallback. This install
                // does emit the WordPress `<head>` marker, present on every page but the last.
                "catalog": { "next": "link[rel=\"next\"]" },
                // The theme's `div.post-title` wrapper is renamed here, and the `<h1>` also
                // carries a NEW/HOT badge span — so the id, not the bare tag, not the default.
                "series": { "title": "#manga-title h1" }
            }),
            politeness: Politeness::default(),
        },
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

/// Providers on the `MangaThemesia` theme.
#[expect(
    clippy::too_many_lines,
    reason = "one entry per site; same reason as `madara_family`"
)]
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
        // `readcomiconline.xyz` was probed and rejected. It parses — 766 chapter rows on a
        // long-running title — but two things make it worse than no source. Its feed ignores
        // `?order=update` and returns a slice of the alphabetical listing, so a fast scan reads
        // thirty arbitrary stubs and ingests nothing; and its one-shots are labelled `<Title>
        // #Full`, so `parse_chapter_number`'s bare-number fallback stores `.357!` as chapter
        // 357. A live fast scan confirmed it: 30 series seen, zero chapters.
        // The 2026-08-26 expansion.
        // Runs the coin plugin, and deliberately carries **no** `locked` selector: this install
        // renders a paid row as a modal trigger whose anchor has no `href` at all, so the row has
        // no chapter URL and is dropped before access is ever read — the shape `violetscans` and
        // `thunderscans` already have. A `span.text-gold` rule here could never fire, and a rule
        // that cannot fire reads as a live one. `tests/expansion_presets_fixture.rs` pins the
        // reason, so a future install that *does* link its paid rows fails a test rather than
        // silently ingesting them as free.
        themesia(
            "athreascans",
            "Athrea Scans",
            "https://athreascans.com",
            json!({ "series": infotable_series() }),
        ),
        plain(
            "culturedworks",
            "CulturedWorks",
            "https://culturedworks.com",
            THEMESIA,
        ),
        themesia(
            "evascans",
            "Eva Scans",
            "https://evascans.org",
            card_v_series(),
        ),
        themesia(
            "galaxymanga",
            "Galaxy Manga",
            "https://galaxymanga.io",
            // Scoped to the list's own id. This install ships a Handlebars *template* row for
            // its reading-history widget — `<li data-num="{{number}}">`, outside the chapter
            // list — and the family's bare `li[data-num]` matched it on every series page.
            json!({ "chapters": { "container": "#chapterlist li[data-num]" } }),
        ),
        themesia(
            "lagoonscans",
            "Lagoon Scans",
            "https://lagoonscans.com",
            json!({ "series": infotable_series() }),
        ),
        madarascans(),
        themesia(
            "mangatx",
            "MangaTX",
            "https://mangatx.cc",
            // This install's paginator is clamped server-side: `/manga/page/3/`, `?page=3` and
            // `?page=9999` all return the byte-identical page-1 document, 40 cards, and the
            // theme still renders a next control. Without `pages: 1` the yielded-items fallback
            // never goes false and the walk re-ingests page 1 until the planner's cap — 20 000
            // requests for 40 series, with every one of them "succeeding".
            json!({ "catalog": { "pages": 1 } }),
        ),
        themesia(
            "rackusreads",
            "Rackus Reads",
            "https://rackusreads.com",
            json!({ "series": infotable_series() }),
        ),
        themesia_in(
            "ravenscans",
            "Raven Scans",
            "https://ravenscans.net",
            "series",
        ),
        plain(
            "scythescans",
            "Scythe Scans",
            "https://scythescans.com",
            THEMESIA,
        ),
        themesia(
            "silentquill",
            "Silent Quill",
            "https://www.silentquill.net",
            // Only the heading is renamed; the info block, chapter rows and listing are stock.
            json!({ "series": { "title": "h1.kdt8-left-title" } }),
        ),
    ]
}

/// The `catalog`/`latest`/`series` fragment for the two installs that replaced the theme's
/// `div.bsx` grid with a `manga-card-v` one and renamed the archive to `/series/`.
///
/// The chapter list is deliberately *not* overridden: both still render the theme's own
/// `li[data-num]` rows with `span.chapternum`/`span.chapterdate`, which is what makes them worth
/// a config row rather than an adapter.
fn card_v_listing() -> Value {
    json!({
        "catalog": {
            "path": "/series/?page={page}",
            "item": "div.manga-card-v",
            "link": "a",
            "title": "h3.card-v-title a"
        },
        "latest": {
            "path": "/series/?page=1&order=update",
            "item": "div.manga-card-v",
            "link": "a",
            "title": "h3.card-v-title a",
            "chapter": "div.card-v-chapters a"
        }
    })
}

/// `card_v_listing`, the `series` heading `evascans` renames, and its paywall marker.
///
/// The coin plugin here is a *different skin* from the one `akazascans`/`rokaricomics` run:
/// it renders a price-and-padlock block rather than the `span.text-gold` those installs use, so
/// [`coin_gated_chapters`] would never fire. Found by sampling twenty series — thirteen carried
/// locked rows and the site's first page carried none, which is why a single-series probe missed
/// it and every paid chapter had been ingesting as free.
///
/// No `unlock`: the badge states a coin price and never a date, so a locked row keeps an unknown
/// unlock time and stays locked. Same policy as the rest of the family.
fn card_v_series() -> Value {
    let mut cfg = card_v_listing();
    cfg["series"] = json!({ "title": "h1.series-title-main" });
    cfg["chapters"] = json!({ "locked": "div.locked-badge" });
    cfg
}

/// `madarascans.org`: the `card_v_listing` shell, its own heading, and — the reason it is spelled
/// out rather than folded into a helper — a chapter list that is neither the theme's nor another
/// install's.
///
/// Rows are keyed by a `data-ch` attribute with the link one level in, and a paid row is marked
/// by a lock glyph inside the number cell. That glyph is rendered on locked rows only, which is
/// what `chapters.locked` requires: a selector matching a container that is always present would
/// mark the whole catalogue early-access. No `unlock`, because the row states no date — so a
/// locked chapter stays locked until a scan sees the glyph gone.
fn madarascans() -> BuiltinPreset {
    let mut config = card_v_listing();
    config["series"] = json!({ "title": "h1.lh-title" });
    config["chapters"] = json!({
        "container": "[data-ch]",
        "link": "a.ch-main-anchor",
        "number_from": "text",
        "locked": "i.fa-lock"
    });
    themesia(
        "madarascans",
        "Madara Scans",
        "https://madarascans.org",
        config,
    )
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
        // The 2026-08-26 expansion added exactly one. Nine further Keyoapp installs were
        // surveyed and eight are unreachable: three answer `/` with an origin nginx 404 through
        // a fully solved browser over both address families (the soft-ban shape this platform
        // uses), and five have moved off it. The platform is worth watching, not mining.
        plain("erisscans", "Eris Scans", "https://erisscans.com", KEYOAPP),
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
        // Two more of the same application found in the 2026-08-26 survey. Sized like their
        // siblings above, and kept separate for the same reason.
        manganato("mangabats", "Mangabat", "https://www.mangabats.com"),
        manganato("manganatogg", "Manganato", "https://manganato.gg"),
        manganato(
            "mangakakalove",
            "MangaKakaLove",
            "https://www.mangakakalove.com",
        ),
    ]
}

/// One domain of the Manganato application, at the catalogue-scale crawl budget the family
/// shares. Per worker *process*, so at the shipped two replicas this is the policy ceiling.
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

/// The crawl budget for a catalogue in the tens of thousands of series.
///
/// Double the default, half the policy ceiling: enforced per worker process, so at the shipped
/// two replicas this is exactly `MAX_RPS`/`MAX_CONCURRENCY` aggregate. Raising the replica count
/// without lowering these silently exceeds the ceiling.
fn bulk_budget() -> Politeness {
    Politeness {
        rps: 4.0,
        concurrency: 8,
        ..Politeness::default()
    }
}

/// Bespoke layouts that still reduce to selectors, so they need no Rust of their own.
fn selector_only() -> Vec<BuiltinPreset> {
    let mut all = vec![tcbscans()];
    all.push(weebcentral());
    all.push(mangapill());
    all.extend(aggregators());
    all.extend(single_series_readers());
    all
}

/// The `MangaCatalog` theme: eight sites, one popular series each, identical markup.
///
/// Small catalogues, but each carries the full run of a series people actually track — one of
/// them lists 1 207 chapters — so they are worth a row apiece. Every one is `single_series_site`
/// with nothing but identity, which is what a hosted theme should cost.
///
/// Two siblings are deliberately absent. `readberserk` and `readopm` moved to a `WordPress`
/// theme whose chapter rows put the number in a `<td>` and give every anchor the text `Read`;
/// `chapters.number_from` reads link text, so no selector set can number those rows — a schema
/// limit, not a missing selector. `readblackclover`'s domain has expired.
fn single_series_readers() -> Vec<BuiltinPreset> {
    vec![
        single_series_site("readsnk", "Read Attack on Titan", "https://readsnk.com"),
        single_series_site(
            "tokyoghoulre",
            "Read Tokyo Ghoul",
            "https://tokyoghoulre.com",
        ),
        single_series_site(
            "readjujutsu",
            "Read Jujutsu Kaisen",
            "https://readjujutsukaisen.com",
        ),
        single_series_site(
            "read7ds",
            "Read Seven Deadly Sins",
            "https://read7deadlysins.com",
        ),
        single_series_site(
            "readsololeveling",
            "Read Solo Leveling",
            "https://readsololeveling.org",
        ),
        single_series_site(
            "readfairytail",
            "Read Fairy Tail",
            "https://readfairytail.com",
        ),
        single_series_site("readkingdom", "Read Kingdom", "https://readkingdom.com"),
        single_series_site("readonepiece", "Read One Piece", "https://readonepiece.com"),
    ]
}

/// A site on the `MangaCatalog` theme.
///
/// `base_url` is the **bare** domain on purpose. Each of these serves from a rotating `wwN.`
/// host and redirects the bare domain to whichever prefix is current, so naming the prefix would
/// pin the preset to a hostname the operator rotates without notice.
fn single_series_site(
    slug: &'static str,
    name: &'static str,
    base_url: &'static str,
) -> BuiltinPreset {
    BuiltinPreset {
        slug,
        name,
        base_url,
        adapter: GENERIC,
        config: json!({
            "catalog": {
                // The sitemap, not the home page. The home page's section headers list one to
                // six of the seven-to-twenty-five series each site actually hosts, so an HTML
                // walk would silently enumerate a fraction of the catalogue. `pages: 1` is what
                // ends the walk: there is one shard, and without it the yielded-items fallback
                // re-fetches it forever.
                "mode": "sitemap",
                "path": "/sitemap.xml",
                "item": "/manga/",
                "link": "",
                "title": "",
                "pages": 1,
                "next": null
            },
            "latest": {
                "path": "/",
                "item": "div.gap-3.my-3",
                "link": "a.bg-bg-action",
                "title": "h3",
                "chapter": null
            },
            "series": {
                // Not a bare `h1`: the page banner ("Read X Manga Online") is an `h1` too and
                // comes first in the DOM, so the site name would be stored as the series title
                // — and the title is what the matching key is built from.
                "title": "h1.my-3",
                "desc": "div.py-2 > div.text-text-muted",
                "cover": "img.rounded-full@src"
            },
            "chapters": {
                "container": "div.bg-bg-secondary.p-3.rounded.mb-3.shadow",
                "link": "a[href*=\"/chapter/\"]",
                "number_from": "text",
                // No `date`: the `div.text-xs` beside each link is the chapter's subtitle, and
                // the theme publishes no per-chapter timestamp at all.
                "title": "div.text-xs.text-text-muted"
            }
        }),
        politeness: Politeness::default(),
    }
}

/// The 2026-08-26 expansion's selector-only rows: general-purpose readers, each with a layout of
/// its own and none of them on a theme this repository already parses.
///
/// They are grouped because they share a property the scanlator sites do not — catalogues in the
/// tens of thousands of series — and so they all take [`bulk_budget`]. Everything else about them
/// is per-site, which is why each is spelled out rather than folded into a family.
fn aggregators() -> Vec<BuiltinPreset> {
    vec![
        // Two domains of one application, selector for selector. Kept as two rows for the reason
        // the Manganato clones are: each has its own rate limit, health state and reader links.
        fmreader("fanfox", "MangaFox", "https://fanfox.net"),
        fmreader("mangahere", "MangaHere", "https://www.mangahere.cc"),
        hadesscans(),
        kaliscan(),
        mangafreak(),
        mangago(),
        mangakatana(),
        manganow(),
        mangatown(),
        manhuaplus_mirror(),
        mgeko(),
        projectsuki(),
        readcomicsonline(),
        reimanga(),
        saymanhwa(),
        xoxocomics(),
    ]
}

/// `fanfox.net` and `mangahere.cc`: the same application on two domains.
fn fmreader(slug: &'static str, name: &'static str, base_url: &'static str) -> BuiltinPreset {
    BuiltinPreset {
        slug,
        name,
        base_url,
        adapter: GENERIC,
        config: json!({
            "catalog": {
                "path": "/directory/{page}.htm",
                "item": "ul.manga-list-1-list li",
                "link": "a",
                // The visible title is clipped; the anchor's `title` carries it in full.
                "title": "a@title",
                "next": null
            },
            // The same directory with one query flag, which is what re-sorts it by update time.
            "latest": {
                "path": "/directory/1.htm?latest",
                "item": "ul.manga-list-1-list li",
                "link": "a",
                "title": "a@title",
                "chapter": null
            },
            "series": {
                "title": "span.detail-info-right-title-font",
                "desc": "p.fullcontent",
                "cover": "img.detail-info-cover-img@src",
                "tags": "p.detail-info-right-tag-list a",
                "author": "p.detail-info-right-say a",
                "status": "span.detail-info-right-title-tip"
            },
            "chapters": {
                "container": "ul.detail-main-list li",
                "link": "a",
                "number_from": "text",
                "title": "p.title3",
                "date": "p.title2"
            }
        }),
        politeness: bulk_budget(),
    }
}

/// A `WordPress` scanlator site on a bespoke theme — Madara's URL shape, none of its markup.
fn hadesscans() -> BuiltinPreset {
    BuiltinPreset {
        slug: "hadesscans",
        name: "Hades Scans",
        base_url: "https://hadesscans.com",
        adapter: GENERIC,
        config: json!({
            "catalog": {
                // The path, not `?page=`: this theme keeps Madara's URL shape and *ignores* the
                // query parameter, so `?page=2` re-served page 1 and the walk re-ingested the
                // same thirty series until the planner's cap without failing once.
                "path": "/manga/page/{page}/",
                "item": "article.cx-poster-card",
                "link": "a.cx-poster-card__cover-link",
                "title": "h3.cx-poster-card__title",
                // No marker: `link[rel=next]` is rendered on every page here, including past
                // the end. The page after the last answers 200 with zero cards, which is
                // exactly what the yielded-items fallback needs.
                "next": null
            },
            "latest": {
                "path": "/manga/?page=1&m_orderby=latest",
                "item": "article.cx-poster-card",
                "link": "a.cx-poster-card__cover-link",
                "title": "h3.cx-poster-card__title",
                "chapter": "span.cx-poster-card__chapter"
            },
            "series": {
                "title": "h1.cx-single-hero__title",
                "desc": "div.cx-single-hero__synopsis",
                "cover": "div.cx-single-hero__cover img@src",
                "tags": "a.cx-single-hero__genre"
            },
            "chapters": {
                // The row *is* the anchor here, so `self` — a nested `link` selector finds
                // nothing and the whole list parses to zero rows without failing.
                "container": "a.cx-chapter-item",
                "link": "self",
                "number_from": "text",
                "title": "span.cx-chapter-item__title",
                "date": "span.cx-chapter-item__date"
            }
        }),
        politeness: Politeness::default(),
    }
}

fn kaliscan() -> BuiltinPreset {
    BuiltinPreset {
        slug: "kaliscan",
        name: "KaliScan",
        base_url: "https://kaliscan.io",
        adapter: GENERIC,
        config: json!({
            "catalog": {
                "path": "/az-list?page={page}",
                "item": "div.book-detailed-item",
                "link": "div.thumb a",
                "title": "div.thumb a@title",
                "next": null
            },
            "latest": {
                "path": "/latest?page=1",
                "item": "div.book-detailed-item",
                "link": "div.thumb a",
                "title": "div.thumb a@title",
                "chapter": "span.latest-chapter"
            },
            "series": {
                "title": "div.name h1",
                "desc": "div.section-body p",
                // Covers are lazy-loaded; `src` is a shared placeholder GIF.
                "cover": "div.img-cover img@data-src",
                "alt": "div.name h2",
                "tags": "div.meta a[href*=\"/genre\"]"
            },
            "chapters": {
                // The id, not the visible "LATEST CHAPTERS" strip above it, which is a preview
                // of the newest few and would have truncated every series to three rows.
                "container": "#chapter-list li",
                "link": "a",
                "number_from": "text",
                // The row nests its update time inside the anchor, so the anchor's own text
                // reads `Chapter 17 days ago` for chapter 1 and the number parses as 17. This
                // scrambled 99% of the numbers on the first ingest — plausible values, wrong
                // reading order, and nothing failed. Read the label element instead.
                "number": "strong.chapter-title",
                "date": "time.chapter-update"
            }
        }),
        politeness: bulk_budget(),
    }
}

fn mangafreak() -> BuiltinPreset {
    BuiltinPreset {
        slug: "mangafreak",
        name: "Mangafreak",
        base_url: "https://ww3.mangafreak.me",
        adapter: GENERIC,
        config: json!({
            "catalog": {
                "path": "/Genre/All/{page}",
                "item": "div.ranking_item",
                "link": "a",
                "title": "a@title",
                "next": "a.next_p"
            },
            "latest": {
                "path": "/Latest_Releases",
                "item": "div.latest_releases_item",
                "link": "a",
                "title": "a@title",
                "chapter": null
            },
            "series": {
                "title": "div.manga_series_data h1",
                "desc": "div.manga_series_description p",
                "cover": "div.manga_series_image img@src",
                "tags": "div.series_sub_genre_list a"
                // No credits or status: the info block is a run of unlabelled `div`s
                // distinguished only by position, which no `TextSource` shape can address.
            },
            "chapters": {
                "container": "div.manga_series_list tr",
                "link": "a",
                "number_from": "text",
                "date": "td:nth-of-type(2)"
            }
        }),
        politeness: bulk_budget(),
    }
}

fn mangago() -> BuiltinPreset {
    BuiltinPreset {
        slug: "mangago",
        name: "Mangago",
        base_url: "https://www.mangago.me",
        adapter: GENERIC,
        config: json!({
            "catalog": {
                "path": "/list/directory/all/{page}/",
                "item": "div.updatesli",
                "link": "a.thm-effect",
                "title": "a.thm-effect@title",
                // 455 pages, and past the end the site answers 200 with a full 44-card page of
                // series it has not served before — deterministic per page number, disjoint from
                // the real ones. Neither item count nor novelty can stop the walk; the only
                // thing that changes is that the paginator stops marking a current page.
                "next": "div.pagination li.current + li > a"
            },
            "latest": {
                "path": "/list/directory/all/1/",
                "item": "div.updatesli",
                "link": "a.thm-effect",
                "title": "a.thm-effect@title",
                "chapter": null
            },
            "series": {
                "title": "div.w-title h1",
                "desc": "div.manga_summary",
                "cover": "div.left.cover img@src",
                "tags": "td a[href*=\"/genre/\"]"
            },
            "chapters": {
                "container": "table#chapter_table tr",
                "link": "a.chico",
                "number_from": "text",
                "date": "td:nth-of-type(3)"
            }
        }),
        politeness: bulk_budget(),
    }
}

fn mangakatana() -> BuiltinPreset {
    BuiltinPreset {
        slug: "mangakatana",
        name: "MangaKatana",
        base_url: "https://mangakatana.com",
        adapter: GENERIC,
        config: json!({
            "catalog": {
                "path": "/manga/page/{page}",
                "item": "div#book_list > div.item",
                "link": "div.text h3 a",
                "title": "div.text h3 a",
                "next": "a.next.page-numbers"
            },
            // `/page/{n}` is the site-wide newest-updates listing, rendered by the same template.
            "latest": {
                "path": "/page/1",
                "item": "div#book_list > div.item",
                "link": "div.text h3 a",
                "title": "div.text h3 a",
                "chapter": null
            },
            "series": {
                "title": "h1.heading",
                "desc": "div.summary p",
                "cover": "div.media div.cover img@src",
                "tags": "div.genres a",
                "status": "div.value.status",
                "alt": "div.alt_name",
                "author": "a.author"
            },
            "chapters": {
                "container": "div.chapters table tr",
                "link": "div.chapter a",
                "number_from": "text",
                "date": "div.update_time"
            }
        }),
        politeness: bulk_budget(),
    }
}

/// The `MangaReader` theme: ~2 860 series over 179 A-Z pages, whole chapter list server-rendered.
fn manganow() -> BuiltinPreset {
    BuiltinPreset {
        slug: "manganow",
        name: "MangaNow",
        base_url: "https://manganow.to",
        adapter: GENERIC,
        config: json!({
            "catalog": {
                "path": "/az-list?page={page}",
                "item": "div.mls-wrap div.item",
                "link": "a.manga-poster",
                "title": "h3.manga-name",
                "next": "ul.pagination a[rel=\"next\"]"
            },
            "latest": {
                "path": "/filter?sort=latest-updated",
                "item": "div.mls-wrap div.item",
                "link": "a.manga-poster",
                "title": "h3.manga-name",
                "chapter": "div.fd-list div.chapter a"
            },
            "series": {
                "title": "h2.manga-name",
                "alt": "div.manga-name-or",
                "desc": "div.description",
                "cover": "div.manga-poster img.manga-poster-img@src",
                "tags": "div.sort-desc div.genres a",
                "status": { "row": "div.anisc-info div.item-title", "label": "span.item-head",
                            "match": "Status", "value": "span.name" },
                "author": { "row": "div.anisc-info div.item-title", "label": "span.item-head",
                            "match": "Authors", "value": "a" },
                "artist": { "row": "div.anisc-info div.item-title", "label": "span.item-head",
                            "match": "Artists", "value": "a" },
                "release": { "row": "div.anisc-info div.item-title", "label": "span.item-head",
                             "match": "Published", "value": "span.name" }
            },
            "chapters": {
                "container": "ul.reading-list li.chapter-item",
                "link": "a.item-link",
                "number_from": "text",
                "title": "span.name"
                // No `date`: this theme publishes no per-chapter timestamp at all, and an
                // invented one reorders the release feed.
            }
        }),
        politeness: bulk_budget(),
    }
}

fn mangatown() -> BuiltinPreset {
    BuiltinPreset {
        slug: "mangatown",
        name: "MangaTown",
        base_url: "https://www.mangatown.com",
        adapter: GENERIC,
        config: json!({
            "catalog": {
                // The zeroed segments are the directory's own "no filter" form.
                "path": "/directory/0-0-0-0-0-0/{page}.htm",
                "item": "ul.manga_pic_list li",
                "link": "a.manga_cover",
                "title": "p.title a",
                "next": null
            },
            "latest": {
                "path": "/latest/1.htm",
                "item": "ul.manga_pic_list li",
                "link": "a.manga_cover",
                "title": "p.title a",
                "chapter": null
            },
            "series": {
                "title": "h1.title-top",
                "desc": "span#show",
                "cover": "div.detail_info img@src",
                "tags": "li a[href*=\"/directory/\"]"
            },
            "chapters": {
                "container": "ul.chapter_list li",
                "link": "a",
                // Rows read "<Series Title> 526" with no chapter marker at all, so the number
                // comes from `parse_chapter_number`'s bare-number fallback — the *first* digit
                // run in the label. That is wrong whenever the series title carries digits of
                // its own, and one title here does: "The Dark Magician Transmigrates After
                // 66666 Years" stores 66666 for every chapter. No selector fixes it, because
                // the theme wraps the title and number in one text node with nothing to select
                // between them; `chapters.number` needs an element and there is none.
                //
                // Kept at one known-bad series in ~3 500 chapters, and the implausible-number
                // guard bounds the damage rather than removing it. If that ratio moves, this
                // site needs an adapter that reads the number from the chapter path.
                "number_from": "text",
                "date": "span.time"
            }
        }),
        politeness: bulk_budget(),
    }
}

/// `manhuaplus.org` — an unrelated site to the shipped `manhuaplus` (`manhuaplus.com`), sharing
/// only a name. Distinct slug, distinct row, and the console shows both domains.
fn manhuaplus_mirror() -> BuiltinPreset {
    BuiltinPreset {
        slug: "manhuaplusorg",
        name: "Manhuaplus Mirror",
        base_url: "https://manhuaplus.org",
        adapter: GENERIC,
        config: json!({
            "catalog": {
                // `/manga-list` is a static legacy page that ignores `?page=` entirely and
                // re-serves the same twenty cards forever; this is the site's own pager route,
                // and it uses a different card than `/home` does. The sort is pinned rather
                // than left to a server default so consecutive pages stay consistent.
                "path": "/all-manga/{page}/?sort=last_update&status=0",
                "item": "div.b-img.i-mage",
                "link": "a.block.pt-140p",
                "title": "a.block.pt-140p@title",
                // The pager marks the current page `span.pagecurrent` and every link
                // `span.displaypageNum` — backward links included, so a bare class test never
                // goes false. The adjacent sibling does: on the last page `span.pagecurrent`
                // is the pager's final child. Walks all 28 pages and enumerates 659 series,
                // which is exactly what `manga_sitemap.xml` lists.
                "next": "span.pagecurrent + span.displaypageNum"
            },
            "latest": {
                "path": "/home",
                "item": "figure.sac",
                "link": "a.block",
                "title": "figcaption a",
                "chapter": null
            },
            "series": {
                "title": "h1",
                "desc": "div.summary",
                // The cover is a lazy-loaded `data-src` on a separate host; the Open Graph tag
                // is the one attribute-shaped copy that is already resolved.
                "cover": "meta[property=\"og:image\"]@content"
            },
            "chapters": {
                "container": "li.chapter",
                "link": "a",
                "number_from": "text",
                "date": "time"
            }
        }),
        politeness: bulk_budget(),
    }
}

fn mgeko() -> BuiltinPreset {
    BuiltinPreset {
        slug: "mgeko",
        name: "MangaGeko",
        base_url: "https://www.mgeko.cc",
        adapter: GENERIC,
        config: json!({
            "catalog": {
                // `results` is this listing's page parameter, oddly named but 1-based.
                "path": "/jumbo/manga/?results={page}&filter=All",
                // Scoped to the grid: the page also renders a 24-card `ul.swiper-wrapper`
                // carousel of `li.novel-item`, so the unscoped selector mixed a rotating
                // promo strip into the catalogue and made consecutive pages overlap.
                "item": "ul.novel-list.grid > li.novel-item",
                "link": "a.list-body",
                "title": "a.list-body@title",
                // Past the last page the server clamps to page 5 and keeps serving it, so the
                // yielded-items fallback never terminates. The next chevron survives there but
                // degrades to `href="javascript:void(0)"` — testing the attribute, not the
                // class, is what makes it disappear.
                "next": "div.mg-pagination-table + a.mg-pagination-chev[href*=\"results=\"]"
            },
            "latest": {
                "path": "/jumbo/manga/?results=1&filter=All",
                "item": "ul.novel-list.grid > li.novel-item",
                "link": "a.list-body",
                "title": "a.list-body@title",
                "chapter": "h5.chapter-title"
            },
            "series": {
                "title": "h1.novel-title",
                "desc": "div.description",
                "cover": "figure.cover img@data-src",
                "tags": "div.categories a",
                "alt": "h2.alternative-title"
            },
            "chapters": {
                // The series page renders the newest rows behind a "Load All Chapters" control
                // and this is where that control points. Reading the series page instead
                // truncates every series to its first screen, silently.
                "path": "{path}all-chapters/",
                "container": "ul.chapter-list li",
                "link": "a",
                "number_from": "text"
            }
        }),
        politeness: bulk_budget(),
    }
}

fn projectsuki() -> BuiltinPreset {
    BuiltinPreset {
        slug: "projectsuki",
        name: "Project Suki",
        base_url: "https://projectsuki.com",
        adapter: GENERIC,
        config: json!({
            "catalog": {
                "path": "/browse?page={page}",
                "item": "div.browse",
                "link": "h5 a",
                "title": "h5 a",
                "next": null
            },
            "latest": {
                "path": "/browse?page=1",
                "item": "div.browse",
                "link": "h5 a",
                "title": "h5 a",
                "chapter": null
            },
            "series": {
                // This template renders no heading for the title at all — the only copy on the
                // page outside the JSON-LD block is the Open Graph tag.
                "title": "meta[property=\"og:title\"]@content",
                "desc": "meta[name=\"description\"]@content",
                "cover": "img.book@src"
            },
            "chapters": {
                "container": "tbody tr",
                "link": "a[href*=\"/read/\"]",
                "number_from": "text"
            }
        }),
        politeness: Politeness::default(),
    }
}

fn readcomicsonline() -> BuiltinPreset {
    BuiltinPreset {
        slug: "readcomicsonline",
        name: "Read Comics Online",
        base_url: "https://readcomicsonline.ru",
        adapter: GENERIC,
        config: json!({
            "catalog": {
                "path": "/comic-list?page={page}",
                "item": "div.group",
                "link": "a.line-clamp-2",
                "title": "a.line-clamp-2",
                "next": null
            },
            // `/latest-release` exists and renders none of these cards; `/` is the updates grid.
            "latest": {
                "path": "/",
                "item": "div.group",
                "link": "a.line-clamp-2",
                "title": "a.line-clamp-2",
                "chapter": null
            },
            "series": {
                "title": "h1",
                "desc": "div.prose",
                "cover": "meta[property=\"og:image\"]@content"
            },
            "chapters": {
                // Scoped to the list section: the "Read First" button above it is the same
                // anchor shape and would be stored as a duplicate of chapter 1.
                "container": "section div.divide-y a",
                "link": "self",
                // Rows read "<Series Title (2026-)>#205"; `#` is a chapter marker, so the number
                // after it is what parses — not the year in the title.
                "number_from": "text"
            }
        }),
        politeness: bulk_budget(),
    }
}

fn reimanga() -> BuiltinPreset {
    BuiltinPreset {
        slug: "reimanga",
        name: "ReiManga",
        base_url: "https://reimanga.net",
        adapter: GENERIC,
        config: json!({
            "catalog": {
                "path": "/advanced-search?page={page}",
                // The card *is* the anchor on this template, so `self` — a nested link selector
                // finds nothing and the listing parses to zero items.
                "item": "a.group",
                "link": "self",
                "title": "h3",
                "next": null
            },
            "latest": {
                "path": "/latest-update",
                "item": "a.group",
                "link": "self",
                "title": "h3",
                "chapter": "div.text-blue-400"
            },
            "series": {
                "title": "h1",
                "desc": "p.line-clamp-4",
                "cover": "img.shadow-lg@src",
                "tags": "a.bg-gray-700[href*='genre=']",
                "alt": "p.leading-relaxed.line-clamp-2",
                "status": "span.inline-flex.font-medium"
            },
            "chapters": {
                // Keyed on the row id prefix rather than a utility class: this template's
                // classes are Tailwind, and the id is the only stable thing on the row.
                "container": "a[id^='chapter-']",
                "link": "self",
                "number_from": "text",
                // Rendered as a coarse relative label ("7mo ago") that `parse_date_label` does
                // not recognise, so dates come back absent rather than wrong. Kept pointed at
                // the element so a switch to an absolute date starts working on its own.
                "date": "span.text-gray-500"
            }
        }),
        politeness: bulk_budget(),
    }
}

fn saymanhwa() -> BuiltinPreset {
    BuiltinPreset {
        slug: "saymanhwa",
        name: "SayManhwa",
        base_url: "https://saymanhwa.com",
        adapter: GENERIC,
        config: json!({
            "catalog": {
                // The language prefix is part of every path this site serves.
                "path": "/en/series?page={page}",
                "item": "article.series-card",
                "link": "a.series-card-cover",
                "title": "div.series-card-body h2 a",
                // A page past the end keeps serving twenty-four cards, so the yielded-items
                // fallback never terminates; this marker is absent there and present before it.
                "next": "link[rel=\"next\"]"
            },
            "latest": {
                "path": "/en/series?page=1",
                "item": "article.series-card",
                "link": "a.series-card-cover",
                "title": "div.series-card-body h2 a",
                "chapter": null
            },
            "series": {
                "title": "h1",
                "desc": "div.series-v72-synopsis",
                "cover": "meta[property=\"og:image\"]@content",
                "alt": "div.series-v72-alt"
            },
            "chapters": {
                // The card class, not `a[href*="/chapter-"]`: the series page also renders
                // "first chapter" and "latest chapter" action buttons pointing at real chapter
                // URLs, and the href test swept those in as two extra rows per series.
                "container": "a.series-chapter-card",
                "link": "self",
                "number_from": "text"
            }
        }),
        politeness: bulk_budget(),
    }
}

fn xoxocomics() -> BuiltinPreset {
    BuiltinPreset {
        slug: "xoxocomics",
        name: "XOXO Comics",
        base_url: "https://xoxocomic.com",
        adapter: GENERIC,
        config: json!({
            "catalog": {
                "path": "/comic-list?page={page}",
                "item": "div.box_li",
                "link": "div.box_img a",
                "title": "div.title",
                "next": "a.next-page, a[rel=\"next\"]"
            },
            "latest": {
                "path": "/comic-update?page=1",
                "item": "div.box_li",
                "link": "div.box_img a",
                "title": "div.title",
                "chapter": null
            },
            "series": {
                "title": "h1.title-detail",
                "desc": "div.detail-content p",
                "cover": "div.col-image img@src",
                "tags": "li.kind a"
            },
            "chapters": {
                "container": "div.list-chapter li.row",
                "link": "a",
                "number_from": "text",
                "date": "div.col-xs-4"
            }
        }),
        politeness: bulk_budget(),
    }
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
#[expect(
    clippy::too_many_lines,
    reason = "one entry per site; same reason as `madara_family`"
)]
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
