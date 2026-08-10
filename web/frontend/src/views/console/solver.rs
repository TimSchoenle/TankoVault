//! Challenge & solver, plus the standalone adapter-test tab.

use crate::api;
use crate::components::{async_block_list, use_step_up_gate, HealthPill, StepUpGate, StepUpGuard};
use crate::hooks::{use_reload, Reload};
use crate::i18n::use_i18n;
use crate::icons::{Ic, Icon};
use crate::models::*;
use crate::state::use_session;
use crate::views::console::providers::AdapterTestPanel;
use crate::views::console::RefreshTick;
use dioxus::prelude::*;
use inkstone_ui::{Button, Pill, Tone};
use progenitor_client::ResponseValue;
/// Challenge & solver panel: provider health with re-solve and re-enable actions.
#[component]
pub(super) fn SolverPanel(tick: RefreshTick) -> Element {
    let api = api::use_api();
    let i18n = use_i18n();
    let reload = tick.reload();
    // One gate for the panel: every row's actions are the same capability, so an operator
    // confirms once and the prompt appears above the list rather than inside a row.
    let gate = use_step_up_gate();
    let res = use_resource(move || {
        tick.track();
        let client = api.client();
        async move {
            client
                .list_providers()
                .send()
                .await
                .map(ResponseValue::into_inner)
                .map_err(|e| api::friendly_error(i18n, e))
        }
    });

    let empty = i18n.t("console.solver.noProviders");
    let body = async_block_list(&res, reload, 100, &empty, |rows| {
        let rows = rows.to_vec();
        rsx! {
            for p in rows {
                SolverRow { key: "{p.id}", provider: p, reload, gate }
            }
        }
    });

    rsx! {
        section { style: "margin-bottom:18px;",
            div { class: "ik-tile", style: "margin-bottom:14px;",
                div { class: "ik-flex", style: "justify-content:space-between;flex-wrap:wrap;",
                    div { class: "ik-flex", style: "gap:9px;",
                        Ic { icon: Icon::ShieldLock, size: 20 }
                        div {
                            div { style: "font-weight:600;", {i18n.t("console.solver.title")} }
                            // The back-end's product name, not a message.
                            div { class: "ik-mono ik-muted", style: "font-size:12px;",
                                {i18n.args("console.solver.backend", &[("backend", "TRAWL")])}
                            }
                        }
                    }
                    Pill {
                        tone: Tone::Positive,
                        {i18n.t("console.solver.active")}
                    }
                }
                p { class: "ik-muted", style: "font-size:13px;margin:10px 0 0;",
                    {i18n.t("console.solver.note")}
                }
            }
            h3 { {i18n.t("console.solver.providerStates")} }
            StepUpGuard { gate, intro: Some(i18n.t("console.stepUp.intro")) }
            {body}
        }
    }
}

#[component]
pub(super) fn SolverRow(
    provider: Provider,
    reload: Reload,
    /// The panel's gate, so a refusal opens the one prompt above the list.
    gate: StepUpGate,
) -> Element {
    let api = api::use_api();
    let i18n = use_i18n();
    let session = use_session();
    let id = provider.id;
    let blocked = matches!(
        provider.state,
        ProviderState::Blocked | ProviderState::Disabled | ProviderState::Challenged
    );

    let resolve = {
        move |_| {
            gate.attempt(move || {
                spawn(async move {
                    let client = gate.client(api);
                    if session.token_value().is_some() {
                        match client.resolve_provider().id(id).send().await {
                            Ok(_) => reload.bump(),
                            // The row has no error line, so every other failure stays silent as it
                            // always did — but an elevation demand has to reach the panel's prompt,
                            // or the button does nothing for the rest of the session.
                            Err(e) => {
                                let _refused = gate.refused(api::Refusal::of(&e));
                            }
                        }
                    }
                });
            });
        }
    };
    let reenable = {
        move |_| {
            gate.attempt(move || {
                spawn(async move {
                    let client = gate.client(api);
                    if session.token_value().is_some() {
                        match client
                            .set_provider_state()
                            .id(id)
                            .body(SetProviderStateBody {
                                state: ProviderState::Active,
                            })
                            .send()
                            .await
                        {
                            Ok(_) => reload.bump(),
                            // See `resolve` above.
                            Err(e) => {
                                let _refused = gate.refused(api::Refusal::of(&e));
                            }
                        }
                    }
                });
            });
        }
    };

    rsx! {
        div { class: "ik-row",
            div { class: "grow",
                div { style: "font-weight:600;", "{provider.name}" }
                div { class: "ik-mono ik-muted", style: "font-size:12px;", "{provider.base_url}" }
            }
            HealthPill { state: Some(provider.state) }
            if blocked {
                Button {
                    on_click: reenable,
                    {i18n.t("console.solver.reenable")}
                }
            }
            Button {
                tone: Tone::Primary,
                on_click: resolve,
                Ic { icon: Icon::Refresh, size: 15 }
                {i18n.t("console.solver.resolve")}
            }
        }
    }
}

/// Standalone adapter-test tab: pick a provider, then dry-run its adapter.
#[component]
pub(super) fn AdapterTestTab() -> Element {
    let api = api::use_api();
    let i18n = use_i18n();
    let reload = use_reload();
    let res = use_resource(move || {
        reload.track();
        let client = api.client();
        async move {
            client
                .list_providers()
                .send()
                .await
                .map(ResponseValue::into_inner)
                .map_err(|e| api::friendly_error(i18n, e))
        }
    });
    let mut chosen = use_signal(|| Option::<String>::None);

    let empty = i18n.t("console.adapterTest.noProviders");
    let body = async_block_list(&res, reload, 60, &empty, |rows| {
        let opts = rows.to_vec();
        let sel = chosen.read().clone();
        rsx! {
                div { class: "ik-flex", style: "margin-bottom:4px;",
                    label { class: "ik-muted", style: "font-size:13px;", {i18n.t("discover.provider")} }
                    select {
                        class: "ik-input",
                        style: "width:auto;",
                        onchange: move |e| {
                            let v = e.value();
                            chosen.set(if v.is_empty() { None } else { Some(v) });
                        },
                        option { value: "", selected: sel.is_none(), {i18n.t("console.adapterTest.choose")} }
                        for p in opts {
                            option { value: "{p.id}", selected: sel.as_deref() == Some(p.id.to_string().as_str()), "{p.name}" }
                        }
                    }
                }
            if let Some(pid) = chosen.read().clone().and_then(|v| v.parse::<ProviderId>().ok()) {
                AdapterTestPanel { provider_id: pid }
            }
        }
    });

    rsx! {
        section { style: "margin-bottom:18px;",
            h3 { {i18n.t("console.tab.adapterTest")} }
            p { class: "ik-muted", style: "font-size:13px;margin-top:0;",
                {i18n.t("console.adapterTest.intro")}
            }
            {body}
        }
    }
}
