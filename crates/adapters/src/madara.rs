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
            "alt": "div.summary-heading"
        },
        "chapters": {
            "container": "li.wp-manga-chapter",
            "link": "a",
            "number_from": "text",
            "date": "span.chapter-release-date"
        }
    })
}
