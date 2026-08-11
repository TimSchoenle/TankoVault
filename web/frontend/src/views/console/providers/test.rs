//! The standalone adapter dry-run panel, shared by the inspector's Config tab and the
//! Adapter-test entity.

use super::config::DryRunResult;
use crate::api;
use crate::hooks::use_busy;
use crate::i18n::use_i18n;
use crate::models::{ProviderId, TestAdapterRequest};
use dioxus::prelude::*;
use inkstone_ui::{Button, Tone};
use progenitor_client::ResponseValue;
/// Live adapter dry-run against the provider's site, as the standalone Adapter test surface
/// uses it. Runs on demand only; shows the raw parsed sample so operators can validate
/// selectors without a deploy.
#[component]
pub(in crate::views::console) fn AdapterTestPanel(provider_id: ProviderId) -> Element {
    let api = api::use_api();
    let i18n = use_i18n();
    let mut path = use_signal(String::new);
    let busy = use_busy();
    let mut result: Signal<Option<Result<serde_json::Value, String>>> = use_signal(|| None);

    let run = move |_| {
        if !busy.claim() {
            return;
        }
        let p = path.peek().trim().to_owned();
        let client = api.client();
        spawn(async move {
            let body = TestAdapterRequest {
                path: (!p.is_empty()).then_some(p),
            };
            let out = client
                .test_adapter()
                .id(provider_id)
                .body(body)
                .send()
                .await
                .map(ResponseValue::into_inner)
                .map_err(|e| api::friendly_error(i18n, e));
            result.set(Some(out));
            busy.release();
        });
    };

    rsx! {
        div { style: "margin-top:14px;border-top:1px solid var(--border);padding-top:14px;",
            div { class: "ik-flex",
                input {
                    class: "ik-input ik-mono",
                    style: "flex:1;",
                    placeholder: i18n.t("console.adapterTest.pathPlaceholder"),
                    value: "{path}",
                    oninput: move |e| path.set(e.value()),
                }
                Button {
                    tone: Tone::Primary,
                    disabled: busy.is_busy(),
                    on_click: run,
                    if busy.is_busy() {
                    {i18n.t("console.adapterTest.running")}
                    } else {
                    {i18n.t("console.adapterTest.run")}
                    }
                }
            }
            DryRunResult { result: result.read().clone() }
        }
    }
}
