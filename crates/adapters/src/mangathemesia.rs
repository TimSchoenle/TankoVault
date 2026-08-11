//! `MangaThemesia` (`WP Manga Stream`) theme defaults.
//!
//! The second-most common layout among the sites this project scrapes, and the one most of the
//! ex-Asura scanlator sites run. Like [`madara`](crate::madara), onboarding a site on this theme
//! is a config row carrying only its deviations.

use serde_json::{Value, json};

/// The default selector set for a MangaThemesia-themed provider.
///
/// Derived from live markup (`rizzfables.com`) and pinned by `tests/mangathemesia_fixture.rs`.
#[must_use]
pub fn mangathemesia_default_config() -> Value {
    json!({
        "catalog": {
            // The theme's `mangaUrlDirectory`, which installs rename freely (`/series/`,
            // `/comics/`); a site that has is a one-line path override on both this and
            // `latest`, and its listing is otherwise identical.
            "path": "/manga/?page={page}",
            "item": "div.bsx",
            "link": "a",
            // The visible title is clipped by CSS; the anchor's `title` carries it in full.
            "title": "a@title",
            // The theme's own paginator (`div.hpage a.r`) is commented out of the markup on
            // several of these sites, so there is no reliable next-page marker. Cleared, which
            // falls back to "another page exists while this one yielded items" — exact here,
            // because a page past the end renders zero `div.bsx`.
            "next": null
        },
        "latest": {
            // The same listing re-sorted, not the home page. The home page's `div.utao` slider
            // is a *widget*: installs that drop it — most of them — served a feed that parsed
            // to zero items, which is the failure mode with no alarm attached, since an empty
            // feed is a valid answer. This listing is rendered by the same template as the
            // catalogue, so a site whose catalogue works has a working feed by construction.
            "path": "/manga/?page=1&order=update",
            "item": "div.bsx",
            "link": "a",
            "title": "a@title",
            "chapter": "div.epxs"
        },
        "series": {
            "title": "h1.entry-title",
            "desc": "div.entry-content[itemprop=description]",
            "cover": "div.thumb img@src",
            "tags": "span.mgen a",
            // `div.imptdt` renders Status, Type, Released, Author and Artist as identical rows
            // distinguished only by their leading label text, so each one is a labelled row and
            // not a selector — the same trap `madara`'s `alt` documents.
            "status": "div.imptdt i.bs-status",
            "alt": "span.alternative",
            "author": {
                "row": "div.imptdt",
                "label": "div.imptdt",
                "match": "Author",
                "value": "i"
            },
            "artist": "div.imptdt:nth-of-type(4) i",
            "release": "div.imptdt:nth-of-type(3) i"
        },
        "chapters": {
            "container": "li[data-num]",
            // The row's first anchor, not `div.eph-num a`: forks of this theme disagree about
            // whether the link sits inside that div or wraps it, and a selector that picks the
            // inner shape finds nothing on the outer one — so the site ingested a full
            // catalogue and zero chapters, with nothing failing. Both shapes put the chapter
            // link first.
            "link": "a",
            "number_from": "text",
            "title": "span.chapternum",
            "date": "span.chapterdate"
        }
    })
}
