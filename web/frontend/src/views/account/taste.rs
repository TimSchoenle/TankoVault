//! The reader's own taste profile (`GET /v1/me/taste`) — what the recommender believes about
//! them, in their own words.
//!
//! Exists so the profile is inspectable by the person it describes, and so a bad shelf can be
//! diagnosed without anyone reading a watchlist. That is why it lives in Account rather than
//! beside the shelf on Home: it is a record about the reader, not a browsing surface.

use crate::api;
use crate::components::{async_block, PanelCard};
use crate::hooks::use_reload;
use crate::i18n::use_i18n;
use crate::icons::Icon;
use crate::models::{TasteFeature, TasteView};
use crate::util::iso_date;
use dioxus::prelude::*;
use progenitor_client::ResponseValue;

/// Features shown per column. The profile keeps 64 positive and 32 negative; the tail of that
/// is noise at weights the reader cannot act on, and a wall of chips reads as data rather than
/// as an answer.
const SHOWN: usize = 12;

#[component]
pub(crate) fn TastePanel() -> Element {
    let api = api::use_api();
    let i18n = use_i18n();
    let reload = use_reload();

    let taste = use_resource(move || {
        reload.track();
        let client = api.client();
        async move {
            client
                .taste()
                .send()
                .await
                .map(ResponseValue::into_inner)
                .map_err(|e| api::friendly_error(i18n, e))
        }
    });

    rsx! {
        PanelCard { icon: Icon::AutoAwesome, title: i18n.t("account.taste.title"),
            p { class: "ik-muted", style: "font-size:12.5px;margin:0 0 14px;line-height:1.6;",
                {i18n.t("account.taste.intro")}
            }
            {async_block(&taste, reload, 200, |profile| rsx! { Profile { profile: profile.clone() } })}
        }
    }
}

/// The two feature lists and the line saying when they were computed.
#[component]
fn Profile(profile: TasteView) -> Element {
    let i18n = use_i18n();
    // A profile with nothing in it is the normal state for a new account, not a failure: the
    // shelf falls back to the catalogue's popularity prior until there is something to learn from.
    if profile.likes.is_empty() && profile.avoids.is_empty() {
        return rsx! {
            div { class: "ik-muted", style: "font-size:13px;", {i18n.t("account.taste.empty")} }
        };
    }
    let seeds = i64::try_from(profile.seeds.len()).unwrap_or(i64::MAX);
    rsx! {
        FeatureList { label: i18n.t("account.taste.likes"), features: profile.likes.clone() }
        if !profile.avoids.is_empty() {
            FeatureList { label: i18n.t("account.taste.avoids"), features: profile.avoids.clone() }
        }
        div { class: "ik-mono ik-muted", style: "font-size:11px;margin-top:14px;",
            {i18n.plural("account.taste.seeds", seeds, &[])}
            " · "
            {
                let built = iso_date(Some(profile.built_at.as_str())).to_owned();
                i18n.args("account.taste.built", &[("date", &built)])
            }
        }
    }
}

/// One weighted list, strongest first, as a bar per feature.
///
/// Bars are relative to the strongest feature in the list, not absolute: the weights are
/// L2-normalised, so their absolute size says how many features share the profile rather than
/// how strongly the reader feels about any one of them.
#[component]
fn FeatureList(label: String, features: Vec<TasteFeature>) -> Element {
    if features.is_empty() {
        return rsx! {};
    }
    let peak = features
        .iter()
        .map(|feature| feature.weight)
        .fold(0.0_f32, f32::max);

    rsx! {
        div { style: "margin-bottom:16px;",
            div { class: "ik-sec-lbl", style: "margin-bottom:8px;", "{label}" }
            for feature in features.iter().take(SHOWN) {
                div { key: "{feature.kind}-{feature.value}", class: "ik-flex", style: "gap:10px;padding:3px 0;",
                    span { style: "font-size:12.5px;min-width:0;flex:1;", "{feature.value}" }
                    span {
                        style: "flex:none;width:72px;height:6px;border-radius:3px;background:var(--border-row);overflow:hidden;",
                        span {
                            style: {
                                let share = if peak > 0.0 { feature.weight / peak } else { 0.0 };
                                let percent = (share * 100.0).clamp(0.0, 100.0);
                                format!("display:block;height:100%;width:{percent:.0}%;background:var(--acc);")
                            },
                        }
                    }
                }
            }
        }
    }
}
