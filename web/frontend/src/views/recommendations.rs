//! "Because you read" — the recommendation shelf as a screen of its own.
//!
//! Split off the home dashboard rather than grown there. On Home it was the last section under a
//! stat row, a continue rail and a day-grouped feed, so it got what was left: a short shelf a
//! reader had to scroll past three other sections to reach, with each suggestion's reason
//! compressed into a grey line. The reason is the point of this surface — a recommendation
//! nobody understands is one nobody acts on — so it gets the room here, at full size and with
//! what the two titles share spelled out.

use crate::api;
use crate::components::{async_view, AuthRequired, EmptyBox, RecCard, SkeletonGrid};
use crate::hooks::use_reload;
use crate::i18n::use_i18n;
use crate::icons::{Ic, Icon};
use crate::state::capabilities::use_capabilities;
use crate::state::use_session;
use crate::wire::types::Feature;
use dioxus::prelude::*;
use progenitor_client::ResponseValue;

#[component]
pub(crate) fn Recommendations() -> Element {
    let session = use_session();
    let i18n = use_i18n();
    let api = api::use_api();
    let caps = use_capabilities();
    // Dismissing a suggestion has to refetch this screen and nothing else, so the shelf owns its
    // own handle rather than sharing one with whatever else a route happens to load.
    let reload = use_reload();

    let offered = caps.has_feature(Feature::CatalogueRecommendations);
    let shelf = use_resource(move || {
        reload.track();
        let client = api.client();
        // Gated here as well as server-side: with the feature off the endpoint answers 404, and
        // an error box under this heading reads as a fault rather than a deployment that does
        // not offer recommendations.
        let fetch = session.is_authenticated() && offered;
        async move {
            if !fetch {
                return Ok(Vec::new());
            }
            // No `limit`: the server's shelf size is an operator tuning value, and asking for a
            // number of our own would silently walk past a deployment that shortened it.
            client
                .recommendations()
                .send()
                .await
                .map(ResponseValue::into_inner)
                .map_err(|e| api::friendly_error(i18n, e))
        }
    });

    if !session.is_authenticated() {
        return rsx! { AuthRequired { title: i18n.t("nav.recommendations") } };
    }

    rsx! {
        div { class: "ik-page-head",
            div {
                h1 { class: "ik-page-title", style: "margin-bottom:2px;",
                    {i18n.t("nav.recommendations")}
                }
                p { class: "ik-muted", style: "font-size:13px;margin:0;max-width:70ch;",
                    {i18n.t("recommendations.intro")}
                }
            }
        }

        if offered {
            {
                async_view(
                    &shelf,
                    reload,
                    || rsx! { SkeletonGrid { count: 12 } },
                    |items| {
                        if items.is_empty() {
                            // A reader with nothing tracked has given the model nothing to work
                            // from, which is a different situation from a broken shelf and gets
                            // its own sentence and a way out of it.
                            return rsx! {
                                div { class: "ik-empty",
                                    Ic { icon: Icon::AutoAwesome, size: 28 }
                                    p { style: "margin:10px 0 4px;font-weight:600;",
                                        {i18n.t("recommendations.empty.title")}
                                    }
                                    p { class: "ik-muted", style: "font-size:13px;",
                                        {i18n.t("recommendations.empty.hint")}
                                    }
                                    Link { to: crate::Route::Discover {}, class: "ik-btn", style: "margin-top:10px;",
                                        {i18n.t("nav.discover")}
                                    }
                                }
                            };
                        }
                        rsx! {
                            div { class: "ik-count-line",
                                {i18n.plural("recommendations.countLine", i64::try_from(items.len()).unwrap_or(0), &[])}
                            }
                            div { class: "ik-grid",
                                for item in items.iter().cloned() {
                                    RecCard { key: "{item.id}", item, reload, detailed: true }
                                }
                            }
                        }
                    },
                )
            }
        } else {
            EmptyBox { message: i18n.t("recommendations.disabled") }
        }
    }
}
