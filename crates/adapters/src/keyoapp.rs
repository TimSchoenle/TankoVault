//! Keyoapp theme defaults: the hosted platform a cluster of scanlator sites runs on.
//!
//! Server-rendered Tailwind, so the markup carries almost no semantic classes — what it *does*
//! carry is stable ids (`#searched_series_page`, `#chapters`, `#expand_content`) and data on the
//! elements themselves, which is what the selectors below key on. Like [`madara`](crate::madara),
//! a site on this platform is a config row carrying only its deviations.

use serde_json::{Value, json};

/// The default selector set for a Keyoapp-hosted provider.
///
/// Derived from live markup (`asmotoon.com`) and pinned by `tests/family_presets_fixture.rs`.
#[must_use]
pub fn keyoapp_default_config() -> Value {
    json!({
        "catalog": {
            // `/series/` renders the **whole** catalogue server-side and filters it in the
            // browser, so there is one page and never a next one. `pages: 1` is what ends the
            // walk: the platform serves that same document for any query string, so both the
            // next-marker and the yielded-items fallback would say "more pages" forever.
            "path": "/series/",
            "pages": 1,
            "item": "#searched_series_page > button",
            "link": "a",
            // The button's own `title` concatenates the title with every alternative title;
            // the anchor's carries the canonical one alone, which is what the matching key is
            // built from.
            "title": "a@title",
            "next": null
        },
        "latest": {
            "path": "/latest/",
            "item": "div.latest-poster",
            "link": "a[href*=\"/series/\"]",
            "title": "h3",
            "chapter": "a[href*=\"/chapter/\"]@title"
        },
        "series": {
            "title": "h1",
            "desc": "#expand_content p",
            // The cover is a CSS `background-image`, which no selector can read; the Open Graph
            // tag carries the same asset and is the only attribute-shaped copy on the page.
            "cover": "meta[property=\"og:image\"]@content",
            "tags": "a[href*=\"genre=\"]@title",
            // Status, Type, Author and Artist render as identical two-cell grids whose only
            // distinguishing feature is the label text, so each is a labelled row rather than a
            // positional selector — the trap `madara`'s `alt` documents.
            "status": {
                "row": "div.grid.gap-2",
                "label": "div.font-medium",
                "match": "Status",
                "value": "div.min-h-8"
            },
            "author": {
                "row": "div.grid.gap-2",
                "label": "div.font-medium",
                "match": "Author",
                "value": "div.min-h-8"
            },
            "artist": {
                "row": "div.grid.gap-2",
                "label": "div.font-medium",
                "match": "Artist",
                "value": "div.min-h-8"
            },
            "alt": {
                "row": "div.grid.gap-2",
                "label": "div.font-medium",
                "match": "Alternative titles",
                "value": "span.text-md"
            }
        },
        "chapters": {
            "container": "#chapters > a",
            "link": "self",
            "number_from": "text",
            // The row's own `d` attribute, because the rendered copy of the date sits behind
            // two other `.text-xs` elements (the "New" badge and the coin price) that a
            // first-match selector would pick up instead.
            "date": "self@d",
            // Locked rows, and only locked rows, render the coin price badge. The lock overlay
            // marks the same chapters but is drawn with an inline `<img>` whose src is a
            // third-party icon URL, so the alt text is the more stable marker.
            "locked": "img[alt=\"Coin\"]"
            // No `unlock`: the platform states a price, never a date. A locked chapter with no
            // announced unlock time stays locked, which is the deliberate policy.
        }
    })
}
