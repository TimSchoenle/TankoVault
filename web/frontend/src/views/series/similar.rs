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
#[component]
pub(super) fn SimilarRail(series_id: SeriesId) -> Element {
    let api = api::use_api();
    let i18n = use_i18n();
    let caps = use_capabilities();
    let reload = use_reload();

    let offered = caps.has_feature(Feature::CatalogueRecommendations);
    let similar = use_resource(move || {
        reload.track();
        let client = api.client();
        async move {
            if !offered {
                return Ok(Vec::new());
            }
            client
                .similar()
                .id(series_id)
                .limit(RAIL_SIZE)
                .send()
                .await
                .map(ResponseValue::into_inner)
                .map_err(|e| api::friendly_error(i18n, e))
        }
    });

    if !offered {
        return rsx! {};
    }

    rsx! {
        div { class: "ik-sidebar-card",
            div { class: "ik-sec-lbl", style: "margin-bottom:10px;", {i18n.t("series.similar.title")} }
            {
                async_view(
                    &similar,
                    reload,
                    || rsx! { SkeletonBlock { height: 120 } },
                    |items| {
                        if items.is_empty() {
                            return rsx! {
                                div { class: "ik-muted", style: "font-size:12.5px;",
                                    {i18n.t("series.similar.empty")}
                                }
                            };
                        }
                        rsx! {
                            for item in items.iter().cloned() {
                                SimilarRow { key: "{item.id}", item }
                            }
                        }
                    },
                )
            }
        }
    }
}

/// One neighbour: a thumbnail, the title, and the features it shares with the seed.
#[component]
fn SimilarRow(item: SimilarSeries) -> Element {
    rsx! {
        Link {
            to: Route::Series { id: item.id.to_string() },
            class: "ik-flex",
            style: "gap:10px;align-items:flex-start;padding:8px 0;color:inherit;",
            div { style: "width:38px;flex:none;border-radius:6px;overflow:hidden;",
                Cover { url: item.cover_url.clone(), title: item.title.clone() }
            }
            div { style: "min-width:0;",
                div { style: "font-size:13px;font-weight:600;line-height:1.3;", "{item.title}" }
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
