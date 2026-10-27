//! Bespoke layouts that still reduce to selectors, so they need no Rust of their own.

use serde_json::json;
use tankovault_domain::{AdapterKind, Politeness};

use super::{BuiltinPreset, aggregators::aggregators};

/// Bespoke layouts that still reduce to selectors, so they need no Rust of their own.
pub(super) fn selector_only() -> Vec<BuiltinPreset> {
    let mut all = vec![weebcentral(), mangapill()];
    all.extend(aggregators());
    all
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
