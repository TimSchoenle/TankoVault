//! The "More like this" sidebar rail, from `GET /v1/series/{id}/similar`.
//!
//! Content similarity, not a relation: the endpoint ranks by the recommendation model's
//! embedding space and says which features each match shares with the seed. A sequel is *not*
//! what this finds — `relations` is unbuilt — so the wording must not promise one.

use crate::api;
use crate::components::{async_view, Cover, SkeletonBlock};
use crate::hooks::use_reload;
use crate::i18n::use_i18n;
use crate::models::{SeriesId, SimilarSeries};
use crate::state::capabilities::use_capabilities;
use crate::wire::types::Feature;
use crate::Route;
use dioxus::prelude::*;
use progenitor_client::ResponseValue;

/// How many neighbours the sidebar asks for. Short on purpose: this is a rail beside the
/// chapter list, not a second discovery screen.
const RAIL_SIZE: i64 = 6;

/// Series close to this one, with what each has in common.
///
/// Renders nothing when the deployment has recommendations switched off — the endpoint is gated
/// on the same feature and would answer 404.
///
/// This is the furthest below the fold of the eight requests one series page issues, so `ready`
/// holds it back until the screen above it has landed. It is a *prop*, which a `use_resource`
/// does not react to on its own — hence `use_reactive!`.
///
/// The held-back state is `Ok(None)`, not `Ok(vec![])`: a resource keeps its last value while it
/// restarts, so an empty list would render as "nothing similar" for the length of the real fetch,
/// exactly at the moment the rail was finally allowed to run.
#[component]
pub(super) fn SimilarRail(series_id: SeriesId, ready: bool) -> Element {
    let api = api::use_api();
    let i18n = use_i18n();
    let caps = use_capabilities();
    let reload = use_reload();

    let offered = caps.has_feature(Feature::CatalogueRecommendations);
    let similar = use_resource(use_reactive!(|ready| {
        reload.track();
        let client = api.client();
        async move {
            if !offered || !ready {
                return Ok(None);
            }
            client
                .similar()
                .id(series_id)
                .limit(RAIL_SIZE)
                .send()
                .await
                .map(|items| Some(ResponseValue::into_inner(items)))
                .map_err(|e| api::friendly_error(i18n, e))
        }
    }));

    if !offered {
        return rsx! {};
    }

    rsx! {
        div { class: "ik-panel",
            div { class: "ik-sec-lbl", style: "margin-bottom:10px;", {i18n.t("series.similar.title")} }
            {
                async_view(
                    &similar,
                    reload,
                    || rsx! { RailSkeleton {} },
                    |items| match items {
                        None => rsx! {
                            RailSkeleton {}
                        },
                        Some(items) if items.is_empty() => rsx! {
                            div { class: "ik-muted", style: "font-size:12.5px;",
                                {i18n.t("series.similar.empty")}
                            }
                        },
                        Some(items) => rsx! {
                            for item in items.iter().cloned() {
                                SimilarRow { key: "{item.id}", item }
                            }
                        },
                    },
                )
            }
        }
    }
}

/// The rail's loading state, one placeholder per row it is about to ask for.
///
/// A single flat block was 120px against a loaded rail of roughly six times that, so the sidebar
/// grew under the reader every time the neighbours landed. Reserving row-shaped boxes reserves
/// the height as well.
#[component]
fn RailSkeleton() -> Element {
    rsx! {
        div { role: "status", "aria-busy": "true",
            for index in 0..RAIL_SIZE {
                div { key: "{index}", class: "ik-similar-row",
                    div { class: "ik-similar-art",
                        div { class: "ik-skeleton", style: "width:100%;height:100%;" }
                    }
                    div { style: "min-width:0;flex:1;display:flex;flex-direction:column;gap:7px;",
                        SkeletonBlock { height: 14, width: "75%" }
                        SkeletonBlock { height: 11, width: "40%" }
                        SkeletonBlock { height: 11, width: "60%" }
                    }
                }
            }
        }
    }
}

/// One neighbour: a thumbnail, the title, its length, and the features it shares with the seed.
///
/// The thumbnail is sized by a class rather than an inline `width`, because the box has to be
/// reserved in *both* dimensions. It was a 38px-wide `div` with no height, leaving the height to
/// the `<img>`'s aspect ratio — which a cover that fails to load does not have, so one broken
/// image collapsed its row and pulled the whole rail's spacing out of alignment. `Cover` now
/// swaps a failed image for the fallback element as well, so the two fixes are belt and braces:
/// the box exists whatever renders inside it.
#[component]
fn SimilarRow(item: SimilarSeries) -> Element {
    let i18n = use_i18n();
    rsx! {
        Link {
            to: Route::Series { id: item.id.to_string() },
            class: "ik-similar-row",
            div { class: "ik-similar-art",
                Cover { url: item.cover_url.clone(), title: item.title.clone() }
            }
            div { style: "min-width:0;",
                div { style: "font-size:13.5px;font-weight:600;line-height:1.3;", "{item.title}" }
                div { class: "ik-mono ik-muted", style: "font-size:11.5px;margin-top:3px;",
                    {i18n.plural("series.chapterTally", item.chapter_count, &[])}
                }
                if !item.shared.is_empty() {
                    div {
                        class: "ik-muted",
                        style: "font-size:11.5px;margin-top:3px;line-height:1.4;",
                        {item.shared.join(" · ")}
                    }
                }
            }
        }
    }
}
