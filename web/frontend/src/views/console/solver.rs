//! Challenge & solver, plus the standalone adapter-test tab.

use crate::api;
use crate::components::{SkeletonBlock, EmptyBox, ErrorBox};
use crate::hooks::{use_reload, Reload};
use crate::i18n::use_i18n;
use crate::icons::{Ic, Icon};
use crate::models::*;
use crate::state::use_session;
use crate::views::console::providers::provider_state_token;
use crate::views::console::providers::AdapterTestPanel;
use crate::views::console::providers::HealthPill;
use crate::views::console::RefreshTick;
use dioxus::prelude::*;
use progenitor_client::ResponseValue;

/// Challenge & solver (`DESIGN_SPEC` §7.8.4). The challenge back-end (`FlareSolverr`) is shown as
/// an informational card; per-provider solve-success metrics need a dedicated endpoint
/// (TODO(api) §9.5), so this lists provider health with a **Re-solve** (fast re-scan) action
/// and a **Re-enable** toggle for blocked/disabled providers.
#[component]
pub(super) fn SolverPanel(tick: RefreshTick) -> Element {
    let api = api::use_api();
    let i18n = use_i18n();
    let session = use_session();
    let reload = use_reload();
    let res = use_resource(move || {
        tick.track();
        reload.track();
        let client = api.client();
        async move {
            if session.is_authenticated() {
                client
                    .list_providers()
                    .send()
                    .await
                    .map(ResponseValue::into_inner)
                    .map_err(|e| api::friendly_error(i18n, e))
            } else {
                Ok(Vec::new())
            }
        }
    });

    let body = match &*res.read_unchecked() {
        None => rsx! { SkeletonBlock { height: 100 } },
        Some(Err(e)) => {
            let msg = e.clone();
            rsx! {
                ErrorBox { message: msg, on_retry: move |()| reload.bump() }
            }
        }
        Some(Ok(list)) if list.is_empty() => rsx! {
            EmptyBox { message: i18n.t("console.solver.noProviders") }
        },
        Some(Ok(list)) => {
            let rows = list.clone();
            rsx! {
                for p in rows {
                    SolverRow { key: "{p.id}", provider: p, reload }
                }
            }
        }
    };

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
                                {i18n.args("console.solver.backend", &[("backend", "FlareSolverr")])}
                            }
                        }
                    }
                    span { class: "ik-pill jade", {i18n.t("console.solver.active")} }
                }
                p { class: "ik-muted", style: "font-size:13px;margin:10px 0 0;",
                    {i18n.t("console.solver.note")}
                }
            }
            h3 { {i18n.t("console.solver.providerStates")} }
            {body}
        }
    }
}

#[component]
pub(super) fn SolverRow(provider: Provider, reload: Reload) -> Element {
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
            spawn(async move {
                let client = api.client();
                if session.token_value().is_some()
                    && client.resolve_provider().id(id).send().await.is_ok()
                {
                    reload.bump();
                }
            });
        }
    };
    let reenable = {
        move |_| {
            spawn(async move {
                let client = api.client();
                if session.token_value().is_some()
                    && client
                        .set_provider_state()
                        .id(id)
                        .body(SetProviderStateBody {
                            state: ProviderState::Active,
                        })
                        .send()
                        .await
                        .is_ok()
                {
                    reload.bump();
                }
            });
        }
    };

    rsx! {
        div { class: "ik-row",
            div { class: "grow",
                div { style: "font-weight:600;", "{provider.name}" }
                div { class: "ik-mono ik-muted", style: "font-size:12px;", "{provider.base_url}" }
            }
            HealthPill { state: provider_state_token(provider.state).to_owned() }
            if blocked {
                button { class: "ik-btn", onclick: reenable, {i18n.t("console.solver.reenable")} }
            }
            button { class: "ik-btn primary", onclick: resolve,
                Ic { icon: Icon::Refresh, size: 15 }
                {i18n.t("console.solver.resolve")}
            }
        }
    }
}

/// Standalone Adapter-test tab (`DESIGN_SPEC` §7.8.5): pick a provider, then dry-run its
/// adapter against the live site and inspect the parsed sample (reuses `AdapterTestPanel`).
#[component]
pub(super) fn AdapterTestTab() -> Element {
    let api = api::use_api();
    let i18n = use_i18n();
    let session = use_session();
    let res = use_resource(move || {
        let client = api.client();
        async move {
            if session.is_authenticated() {
                client
                    .list_providers()
                    .send()
                    .await
                    .map(ResponseValue::into_inner)
                    .map_err(|e| api::friendly_error(i18n, e))
            } else {
                Ok(Vec::new())
            }
        }
    });
    let mut chosen = use_signal(|| Option::<String>::None);

    let body = match &*res.read_unchecked() {
        None => rsx! { SkeletonBlock { height: 60 } },
        Some(Err(e)) => rsx! {
            p { class: "ik-muted", style: "font-size:13px;",
                {i18n.args("console.providers.unavailable", &[("message", e)])}
            }
        },
        Some(Ok(list)) if list.is_empty() => rsx! {
            EmptyBox { message: i18n.t("console.adapterTest.noProviders") }
        },
        Some(Ok(list)) => {
            let opts = list.clone();
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
        }
    };

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
