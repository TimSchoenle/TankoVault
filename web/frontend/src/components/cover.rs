//! Cover artwork and the cover card used by every grid.

use crate::models::{ContentTypeExt, SeriesStatusExt, SeriesSummary};
use crate::util::initial;
use crate::Route;
use dioxus::prelude::*;

/// A cover image with a typographic fallback when no `cover_url` is stored.
///
/// Images are lazily decoded and given explicit `decoding="async"` so a grid of covers never
/// blocks the main thread while the reader is already scrolling past them.
#[component]
pub(crate) fn Cover(url: Option<String>, title: String) -> Element {
    match url {
        Some(src) if !src.is_empty() => rsx! {
            img {
                class: "ik-cover",
                src: "{src}",
                alt: "{title}",
                loading: "lazy",
                decoding: "async",
            }
        },
        _ => rsx! {
            div { class: "ik-cover-fallback", "{initial(&title)}" }
        },
    }
}

/// A single cover card in the Discover/Search/recommendation grids.
#[component]
pub(crate) fn CoverCard(series: ReadSignal<SeriesSummary>) -> Element {
    let series = series.read();
    rsx! {
        Link { to: Route::Series { id: series.id.to_string() }, class: "ik-card",
            Cover { url: series.cover_url.clone(), title: series.title.clone() }
            div { class: "ik-card-body",
                div { class: "ik-card-title", "{series.title}" }
                div { class: "ik-card-meta",
                    span { "{series.content_type.label()}" }
                    span { "·" }
                    span { "{series.status.label()}" }
                    span { class: "ik-rail-spacer" }
                    span { class: "ik-mono", "{series.source_count} src" }
                }
            }
        }
    }
}
