//! The reader's global source order — which provider a series should open on by default.
//!
//! Ranking is explicit and ordered rather than a single favourite, because the realistic case is
//! "this site, else that one, else whatever has it": a series simply is not on every provider.
//! Providers left unranked are not last, they are unopinionated — those series keep resolving by
//! the objective richest-source order the API publishes.
//!
//! Reordering is buttons, not drag-and-drop: a pointer-only affordance would put the whole
//! preference out of reach of a keyboard, and the list is short enough that two clicks is not
//! the slower path anyway.

use crate::api;
use crate::components::{OutcomeLine, PanelCard, Section, SkeletonBlock};
use crate::hooks::use_outcome;
use crate::i18n::use_i18n;
use crate::icons::{Ic, Icon};
use crate::models::{PreferredProvider, ProviderId, PublicProvider, SourcePreferencesUpdate};
use crate::state::source_order::use_source_order;
use dioxus::prelude::*;
use inkstone_ui::Button;
#[component]
pub(crate) fn SourcesPanel() -> Element {
    let i18n = use_i18n();
    let api = api::use_api();
    let mut outcome = use_outcome();
    // The same cache the Series screen ranks against, so a save here reaches that screen
    // without a reload — and without this panel becoming a second source of truth.
    let order_cache = use_source_order();
    // `None` until the first load settles, so the panel skeletons instead of rendering an
    // empty order that a reader could mistake for "nothing is ranked".
    let mut ranked = use_signal(|| Option::<Vec<PreferredProvider>>::None);
    // Which providers the reader has opted into paid early access for. Separate from the
    // order because it answers a different question: the order is where a series opens, this
    // is whether chapters they have paid for count as unread.
    let mut early = use_signal(Vec::<PreferredProvider>::new);
    let mut available = use_signal(Vec::<PublicProvider>::new);

    use_effect(move || {
        let client = api.client();
        spawn(async move {
            match client.source_preferences().send().await {
                Ok(response) => {
                    let prefs = response.into_inner();
                    early.set(prefs.early_access_providers.clone());
                    ranked.set(Some(prefs.providers));
                }
                Err(e) => outcome.set(Some(Err(api::friendly_error(i18n, e)))),
            }
        });
    });
    use_effect(move || {
        let client = api.client();
        spawn(async move {
            if let Ok(response) = client.providers().send().await {
                available.set(response.into_inner());
            }
        });
    });

    // Every mutation is a whole-list write, matching the endpoint: the order *is* the
    // preference, so there is no partial edit to reconcile and no chance of two half-applied
    // writes interleaving into an order the reader never asked for.
    let mut save = move |next: Vec<ProviderId>| {
        outcome.set(None);
        let client = api.client();
        spawn(async move {
            // `early_access_provider_ids: None` leaves the opt-ins untouched — the order and
            // the opt-ins are edited by different controls and must not overwrite each other.
            let body = SourcePreferencesUpdate {
                provider_ids: next,
                early_access_provider_ids: None,
            };
            match client.put_source_preferences().body(body).send().await {
                Ok(response) => {
                    let prefs = response.into_inner();
                    let saved = prefs.providers;
                    order_cache.set(saved.iter().map(|p| p.slug.clone()).collect());
                    early.set(prefs.early_access_providers);
                    ranked.set(Some(saved));
                    outcome.set(Some(Ok(i18n.t("account.sources.saved"))));
                }
                Err(e) => outcome.set(Some(Err(api::friendly_error(i18n, e)))),
            }
        });
    };

    let mut save_early = move |next: Vec<ProviderId>| {
        outcome.set(None);
        let client = api.client();
        spawn(async move {
            // The order half is sent unchanged rather than omitted: the endpoint replaces it
            // wholesale, so leaving it out would clear the reader's ranking as a side effect of
            // toggling a paywall switch.
            let current = ranked.read().clone().unwrap_or_default();
            let body = SourcePreferencesUpdate {
                provider_ids: current.iter().map(|p| p.id).collect(),
                early_access_provider_ids: Some(next),
            };
            match client.put_source_preferences().body(body).send().await {
                Ok(response) => {
                    let prefs = response.into_inner();
                    early.set(prefs.early_access_providers);
                    ranked.set(Some(prefs.providers));
                    outcome.set(Some(Ok(i18n.t("account.sources.saved"))));
                }
                Err(e) => outcome.set(Some(Err(api::friendly_error(i18n, e)))),
            }
        });
    };

    let Some(order) = ranked.read().clone() else {
        return rsx! {
            PanelCard { icon: Icon::Layers, title: i18n.t("account.sources.title"),
                SkeletonBlock { height: 160 }
            }
        };
    };

    let ids = |list: &[PreferredProvider]| list.iter().map(|p| p.id).collect::<Vec<_>>();
    let unranked: Vec<PublicProvider> = available
        .read()
        .iter()
        .filter(|p| !order.iter().any(|r| r.id.0 == p.id))
        .cloned()
        .collect();

    let last = order.len().saturating_sub(1);

    rsx! {
        PanelCard { icon: Icon::Layers, title: i18n.t("account.sources.title"),
            p { class: "ik-muted", style: "font-size:12.5px;margin:0 0 14px;",
                {i18n.t("account.sources.intro")}
            }
            Section { label: i18n.t("account.sources.section.ranked"),
                if order.is_empty() {
                    div { class: "ik-muted", style: "font-size:12.5px;", {i18n.t("account.sources.empty")} }
                }
                for (index , provider) in order.iter().cloned().enumerate() {
                    div { class: "ik-row", key: "{provider.id}",
                        span {
                            class: "ik-mono",
                            style: "min-width:22px;color:var(--faint);font-size:11.5px;",
                            "{index + 1}"
                        }
                        div { class: "grow", "{provider.name}" }
                        Button {
                            disabled: index == 0,
                            title: i18n.t("account.sources.moveUp"),
                            aria_label: i18n.args("account.sources.moveUpOf", &[("source", &provider.name)]),
                            on_click: {
                                let order = order.clone();
                                move |_| {
                                    let mut next = ids(&order);
                                    next.swap(index, index - 1);
                                    save(next);
                                }
                            },
                            Ic { icon: Icon::ArrowUp, size: 14 }
                        }
                        Button {
                            disabled: index == last,
                            title: i18n.t("account.sources.moveDown"),
                            aria_label: i18n.args("account.sources.moveDownOf", &[("source", &provider.name)]),
                            on_click: {
                                let order = order.clone();
                                move |_| {
                                    let mut next = ids(&order);
                                    next.swap(index, index + 1);
                                    save(next);
                                }
                            },
                            Ic { icon: Icon::ArrowDown, size: 14 }
                        }
                        Button {
                            title: i18n.t("account.sources.unrank"),
                            aria_label: i18n.args("account.sources.unrankOf", &[("source", &provider.name)]),
                            on_click: {
                                let order = order.clone();
                                move |_| {
                                    let mut next = ids(&order);
                                    next.remove(index);
                                    save(next);
                                }
                            },
                            Ic { icon: Icon::Close, size: 14 }
                        }
                    }
                }
            }
            if !unranked.is_empty() {
                Section { label: i18n.t("account.sources.section.unranked"),
                    for provider in unranked.iter().cloned() {
                        div { class: "ik-row", key: "{provider.id}",
                            div { class: "grow",
                                div { "{provider.name}" }
                                div { class: "ik-muted", style: "font-size:12px;",
                                    {i18n.args("account.sources.seriesCount", &[("count", &provider.series_count.to_string())])}
                                }
                            }
                            Button {
                                aria_label: i18n.args("account.sources.rankOf", &[("source", &provider.name)]),
                                on_click: {
                                    let order = order.clone();
                                    move |_| {
                                        let mut next = ids(&order);
                                        next.push(ProviderId::from(provider.id));
                                        save(next);
                                    }
                                },
                                Ic { icon: Icon::Add, size: 14 }
                                {i18n.t("account.sources.rank")}
                            }
                        }
                    }
                }
            }
            Section { label: i18n.t("account.sources.section.earlyAccess"),
                p { class: "ik-muted", style: "font-size:12.5px;margin:0 0 10px;",
                    {i18n.t("account.sources.earlyAccessIntro")}
                }
                for provider in available.read().iter().cloned() {
                    {
                        let enabled = early.read().iter().any(|e| e.id.0 == provider.id);
                        let early_now = early.read().clone();
                        rsx! {
                            div { class: "ik-row", key: "ea-{provider.id}",
                                div { class: "grow",
                                    div { "{provider.name}" }
                                    div { class: "ik-muted", style: "font-size:12px;",
                                        if enabled {
                                            {i18n.t("account.sources.earlyAccessOn")}
                                        } else {
                                            {i18n.t("account.sources.earlyAccessOff")}
                                        }
                                    }
                                }
                                Button {
                                    aria_label: i18n.args(
                                        "account.sources.earlyAccessToggleOf",
                                        &[("source", &provider.name)],
                                    ),
                                    on_click: move |_| {
                                        let mut next: Vec<ProviderId> =
                                            early_now.iter().map(|e| e.id).collect();
                                        let id = ProviderId::from(provider.id);
                                        if enabled {
                                            next.retain(|e| *e != id);
                                        } else {
                                            next.push(id);
                                        }
                                        save_early(next);
                                    },
                                    Ic { icon: if enabled { Icon::Check } else { Icon::Add }, size: 14 }
                                }
                            }
                        }
                    }
                }
            }
            OutcomeLine { outcome: outcome.read().clone() }
        }
    }
}
