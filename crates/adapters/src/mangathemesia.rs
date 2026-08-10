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
            "path": "/series/?page={page}",
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
            "path": "/",
            "item": "div.utao div.uta",
            "link": "div.imgu a",
            "title": "div.imgu a@title",
            "chapter": "div.luf ul li a"
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
            "link": "div.eph-num a",
            "number_from": "text",
            "title": "span.chapternum",
            "date": "span.chapterdate"
        }
    })
}
