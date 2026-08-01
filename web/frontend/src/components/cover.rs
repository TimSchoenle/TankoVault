//! Cover artwork and the cover card used by every grid.

use crate::i18n::use_i18n;
use crate::models::{ContentTypeExt, SeriesStatusExt, SeriesSummary};
use crate::util::initial;
use crate::Route;
use dioxus::prelude::*;

/// A cover image with a typographic fallback when no `cover_url` is stored.
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
    let i18n = use_i18n();
    let series = series.read();
    rsx! {
        Link { to: Route::Series { id: series.id.to_string() }, class: "ik-card",
            Cover { url: series.cover_url.clone(), title: series.title.clone() }
            div { class: "ik-card-body",
                div { class: "ik-card-title", "{series.title}" }
                div { class: "ik-card-meta",
                    span { {i18n.t(series.content_type.label_key())} }
                    span { "·" }
                    span { {i18n.t(series.status.label_key())} }
                    span { class: "ik-rail-spacer" }
                    span { class: "ik-mono",
                        {i18n.args("series.sourceCount", &[("count", &series.source_count.to_string())])}
                    }
                }
            }
        }
    }
}
