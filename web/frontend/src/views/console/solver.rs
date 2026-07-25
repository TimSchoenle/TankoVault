//! Challenge & solver, plus the standalone adapter-test tab.

use crate::api;
use crate::components::ErrorBox;
use crate::hooks::{use_reload, Reload};
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
                    .map_err(api::friendly_error)
            } else {
                Ok(Vec::new())
            }
        }
    });

    let body = match &*res.read_unchecked() {
        None => rsx! { div { class: "ik-skeleton", style: "height:100px;" } },
        Some(Err(e)) => {
            let msg = e.clone();
            rsx! {
                ErrorBox { message: msg, on_retry: move |()| reload.bump() }
            }
        }
        Some(Ok(list)) if list.is_empty() => rsx! {
            div { class: "ik-empty", "No providers configured." }
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
                            div { style: "font-weight:600;", "Challenge solver" }
                            div { class: "ik-mono ik-muted", style: "font-size:12px;", "Backend: FlareSolverr" }
                        }
                    }
                    span { class: "ik-pill jade", "active" }
                }
                p { class: "ik-muted", style: "font-size:13px;margin:10px 0 0;",
                    "Per-provider solve-success rates need the solver-metrics endpoint (TODO(api) §9.5). Until then, re-solve queues a fast re-scan that re-attempts any challenged sources."
                }
            }
            h3 { "Provider states" }
            {body}
        }
    }
}

#[component]
pub(super) fn SolverRow(provider: Provider, reload: Reload) -> Element {
    let api = api::use_api();
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
                button { class: "ik-btn", onclick: reenable, "Re-enable" }
            }
            button { class: "ik-btn primary", onclick: resolve,
                Ic { icon: Icon::Refresh, size: 15 }
                "Re-solve"
            }
        }
    }
}

/// Standalone Adapter-test tab (`DESIGN_SPEC` §7.8.5): pick a provider, then dry-run its
/// adapter against the live site and inspect the parsed sample (reuses `AdapterTestPanel`).
#[component]
pub(super) fn AdapterTestTab() -> Element {
    let api = api::use_api();
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
                    .map_err(api::friendly_error)
            } else {
                Ok(Vec::new())
            }
        }
    });
    let mut chosen = use_signal(|| Option::<String>::None);

    let body = match &*res.read_unchecked() {
        None => rsx! { div { class: "ik-skeleton", style: "height:60px;" } },
        Some(Err(e)) => rsx! {
            p { class: "ik-muted", style: "font-size:13px;", "Could not load providers: {e}" }
        },
        Some(Ok(list)) if list.is_empty() => rsx! {
            div { class: "ik-empty", "No providers to test yet." }
        },
        Some(Ok(list)) => {
            let opts = list.clone();
            let sel = chosen.read().clone();
            rsx! {
                div { class: "ik-flex", style: "margin-bottom:4px;",
                    label { class: "ik-muted", style: "font-size:13px;", "Provider" }
                    select {
                        class: "ik-input",
                        style: "width:auto;",
                        onchange: move |e| {
                            let v = e.value();
                            chosen.set(if v.is_empty() { None } else { Some(v) });
                        },
                        option { value: "", selected: sel.is_none(), "— choose a provider —" }
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
            h3 { "Adapter test" }
            p { class: "ik-muted", style: "font-size:13px;margin-top:0;",
                "Dry-run a provider's adapter against the live site without deploying — validate selectors and pagination."
            }
            {body}
        }
    }
}
