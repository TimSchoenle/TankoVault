//! General-purpose readers with layouts of their own, all on the bulk crawl budget.

use serde_json::json;

use super::{BuiltinPreset, GENERIC, bulk_budget};

/// The 2026-08-26 expansion's selector-only rows: general-purpose readers, each with a layout of
/// its own and none of them on a theme this repository already parses.
///
/// They are grouped because they share a property the scanlator sites do not — catalogues in the
/// tens of thousands of series — and so they all take [`bulk_budget`]. Everything else about them
/// is per-site, which is why each is spelled out rather than folded into a family.
pub(super) fn aggregators() -> Vec<BuiltinPreset> {
    vec![
        // Two domains of one application, selector for selector. Kept as two rows for the reason
        // the Manganato clones are: each has its own rate limit, health state and reader links.
        fmreader("fanfox", "MangaFox", "https://fanfox.net"),
        fmreader("mangahere", "MangaHere", "https://www.mangahere.cc"),
        kaliscan(),
        mangafreak(),
        mangakatana(),
        mangatown(),
        manhuaplus_mirror(),
        mgeko(),
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
