//! Madara `WordPress` theme defaults.
//!
//! Most target sites (kunmanga, manhuaus, …) run the Madara theme with near-identical
//! markup, differing only in a few selectors. Onboarding one is therefore a single row
//! whose `config` overrides only what differs from these defaults (design §7).

use serde_json::{Value, json};

/// The default selector set for a Madara-themed provider.
#[must_use]
pub fn madara_default_config() -> Value {
    json!({
        "catalog": {
            "path": "/manga/?page={page}",
            "item": "div.page-item-detail",
            "link": "a",
            "title": "h3 a",
            "next": "a.nextpostslink"
        },
        "latest": {
            "path": "/",
            "item": "div.page-item-detail",
            "link": "a",
            "title": "h3 a",
            "chapter": "span.chapter a"
        },
        "series": {
            "title": "div.post-title h1",
            "desc": "div.description-summary",
            "cover": "div.summary_image img@src",
            "tags": "div.genres-content a",
            "status": "div.post-status .summary-content",
            // A labelled row, not a selector: the theme's summary block renders Alternative,
            // Author(s), Artist(s) and Genre(s) as identical `div.post-content_item` rows
            // that differ only in their heading text. This field used to be
            // `div.summary-heading`, which matched every row's *label* — so every series on
            // every Madara provider was ingested with the alternative titles "Alternative",
            // "Author(s)", "Genre(s)" and "Status", and those went into `series_titles`,
            // which the trigram matcher and the catalogue search both read. See `TextSource`.
            "alt": {
                "row": "div.post-content_item",
                "label": "div.summary-heading h5",
                "match": "Alternative",
                "value": "div.summary-content"
            },
            "author": "div.author-content a",
            "artist": "div.artist-content a"
        },
        "chapters": {
            "container": "li.wp-manga-chapter",
            "link": "a",
            "number_from": "text",
            "date": "span.chapter-release-date"
        }
    })
}
