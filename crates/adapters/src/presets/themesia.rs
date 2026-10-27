//! Presets on the `MangaThemesia` `WordPress` theme.

use serde_json::{Value, json};
use tankovault_domain::Politeness;

use super::{BuiltinPreset, THEMESIA, plain};

/// Providers on the `MangaThemesia` theme.
pub(super) fn mangathemesia_family() -> Vec<BuiltinPreset> {
    vec![
        // The 2026-08 expansion: the ex-Asura scanlator sites, all on the theme itself. Three
        // things vary across them and nothing else does: a `catalog`/`latest` pair where the
        // install renamed the theme's listing directory (`themesia_in`), the series template's
        // info block (`infotable_series`), and the coin plugin's lock selector where the site
        // sells early access (`coin_gated_chapters`). They are independent, so a site carrying
        // two of them says so at the call site rather than in a helper named after the pair.
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
        themesia(
            "evascans",
            "Eva Scans",
            "https://evascans.org",
            card_v_series(),
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
/// The coin plugin here is a *different skin* from the one `rokaricomics` runs:
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
