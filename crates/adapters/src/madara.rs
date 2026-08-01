//! Madara `WordPress` theme defaults: most target sites share this markup, so onboarding one is
//! a single config row overriding only what differs.

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
            // `match` disambiguates this row from Author(s)/Artist(s)/Status rows sharing the
            // same container; matching the label selector alone silently mislabels those as
            // alternative titles, polluting `series_titles` (trigram match, catalogue search).
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
