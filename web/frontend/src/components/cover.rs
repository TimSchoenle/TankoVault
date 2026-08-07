//! Cover artwork and the cover card used by every grid.

use crate::i18n::use_i18n;
use crate::models::{ContentTypeExt, SeriesStatusExt, SeriesSummary};
use crate::util::{chapter_number, initial};
use crate::Route;
use dioxus::prelude::*;

/// A cover image with a typographic fallback when there is no artwork to show.
///
/// The fallback covers two cases, not one: a series with no stored `cover_url`, and a stored URL
/// that does not load. The second is why this is a component with state rather than a `match` on
/// the option — a broken `<img>` is replaced by the browser with an inline alt-text box that
/// ignores `aspect-ratio`, so it collapses to the height of one line of text and drags every
/// neighbour in the row up with it. That is what made a single unreachable cover host visibly
/// break the "More like this" rail's spacing. Swapping the element out keeps the box the layout
/// reserved.
#[component]
pub(crate) fn Cover(url: Option<String>, title: String) -> Element {
    let mut broken = use_signal(|| false);
    // Cleared when the prop changes: this component's instance is reused as a list re-renders,
    // so a failure recorded for one series would otherwise suppress the next one's artwork.
    use_effect(use_reactive!(|url| {
        let _ = &url;
        broken.set(false);
    }));

    match url {
        Some(src) if !src.is_empty() && !*broken.read() => rsx! {
            img {
                class: "ik-cover",
                src: "{src}",
                alt: "{title}",
                loading: "lazy",
                decoding: "async",
                onerror: move |_| broken.set(true),
            }
        },
        _ => rsx! {
            div { class: "ik-cover-fallback", "{initial(&title)}" }
        },
    }
}

/// A single cover card in the Discover/Search/recommendation grids.
///
/// Says four things about a series, not two. Type and status alone answered almost nothing a
/// reader chooses on: whether a title has enough of a backlog to be worth starting, when it is
/// from, and what it is about are the questions a grid of covers has to answer before one gets
/// clicked. Chapter count is deliberately the *de-duplicated* one — the number of sources a
/// series happens to be carried by is an operational fact about this deployment's crawlers, and
/// showing it where a reader expects a length taught them to read it as one.
#[component]
pub(crate) fn CoverCard(series: ReadSignal<SeriesSummary>) -> Element {
    let i18n = use_i18n();
    let series = series.read();
    rsx! {
        Link { to: Route::Series { id: series.id.to_string() }, class: "ik-card",
            div { class: "ik-cover-wrap",
                Cover { url: series.cover_url.clone(), title: series.title.clone() }
                // Only ever rendered for a reader who opted in, since a card carrying this flag
                // has already passed the gate. It marks what they chose to see; it does not hide.
                if series.is_adult {
                    span { class: "ik-adult-badge", {i18n.t("account.content.badge")} }
                }
            }
            div { class: "ik-card-body",
                div { class: "ik-card-title", "{series.title}" }
                CardMeta {
                    content_type: series.content_type,
                    status: series.status,
                    chapter_count: series.chapter_count,
                    latest_chapter: series.latest_chapter,
                    release_year: series.release_year,
                    tags: series.tags.clone(),
                }
            }
        }
    }
}

/// The two meta rows every catalogue card shares, so a recommendation and a search hit describe
/// a series identically.
#[component]
pub(crate) fn CardMeta(
    content_type: crate::models::ContentType,
    status: crate::models::SeriesStatus,
    chapter_count: i64,
    latest_chapter: Option<f64>,
    release_year: Option<i32>,
    tags: Vec<String>,
) -> Element {
    let i18n = use_i18n();
    // The newest number, not the count — they differ whenever a series skips numbers or starts
    // above one, and "up to 412" is the more useful of the two when they disagree.
    let latest = latest_chapter.map(chapter_number);
    rsx! {
        div { class: "ik-card-meta",
            span {
                style: "color:{content_type.color()};",
                {i18n.t(content_type.label_key())}
            }
            span { class: "ik-card-dot", "·" }
            span { class: "ik-flex", style: "gap:5px;align-items:center;color:{status.color()};",
                span { class: "ik-status-dot", style: "width:6px;height:6px;background:{status.color()};" }
                {i18n.t(status.label_key())}
            }
            if let Some(year) = release_year {
                span { class: "ik-card-dot", "·" }
                span { class: "ik-mono", "{year}" }
            }
        }
        div { class: "ik-card-meta",
            span { class: "ik-mono", style: "color:var(--text-2);",
                {i18n.plural("series.chapterTally", chapter_count, &[])}
            }
            if let Some(latest) = latest {
                span { class: "ik-card-dot", "·" }
                span { class: "ik-mono", style: "color:var(--faint);",
                    {i18n.args("series.upTo", &[("number", &latest)])}
                }
            }
        }
        if !tags.is_empty() {
            div { class: "ik-card-tags",
                for tag in tags {
                    span { key: "{tag}", class: "ik-minitag", "{tag}" }
                }
            }
        }
    }
}
