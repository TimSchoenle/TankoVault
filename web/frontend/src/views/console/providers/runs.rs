//! The Runs tab: this provider's recent scan runs, filtered out of the global run list.

use crate::api;
use crate::components::{async_view, EmptyBox, SkeletonBlock};
use crate::hooks::use_reload;
use crate::i18n::use_i18n;
use crate::models::{ProviderId, RunStateExt as _, ScanRun};
use crate::util::rel_time;
use crate::views::console::run_state_pill;
use dioxus::prelude::*;
use progenitor_client::ResponseValue;

/// This provider's recent scan runs, filtered out of the run list.
#[component]
pub(super) fn RunsTab(provider_id: ProviderId) -> Element {
    let api = api::use_api();
    let i18n = use_i18n();
    let reload = use_reload();

    let runs = use_resource(move || {
        reload.track();
        let client = api.client();
        async move {
            client
                .list_scans()
                .send()
                .await
                .map(ResponseValue::into_inner)
                .map_err(|e| api::friendly_error(i18n, e))
        }
    });

    async_view(
        &runs,
        reload,
        || rsx! { SkeletonBlock { height: 160 } },
        |all| {
            let mine: Vec<ScanRun> = all
                .items
                .iter()
                .filter(|run| run.provider_id == Some(provider_id))
                .cloned()
                .collect();
            if mine.is_empty() {
                return rsx! {
                    EmptyBox { message: i18n.t("console.providers.noRuns") }
                };
            }
            rsx! {
                div { class: "ik-listbox",
                    for run in mine.into_iter().take(12) {
                        div { key: "{run.id}", class: "ik-listrow",
                            span { class: run_state_pill(run.state), style: "font-size:9.5px;",
                                {i18n.t(run.state.label_key())}
                            }
                            span { class: "ik-mono", style: "font-size:11.5px;color:var(--muted);",
                                "{run.done_tasks}/{run.total_tasks}"
                            }
                            if run.failed_tasks > 0 {
                                span { class: "ik-mono", style: "font-size:11.5px;color:var(--acc3);",
                                    {
                                        i18n.args(
                                            "console.providers.runFailed",
                                            &[("count", &run.failed_tasks.to_string())],
                                        )
                                    }
                                }
                            }
                            span { class: "ik-mono", style: "margin-left:auto;font-size:11px;color:var(--faint);",
                                {rel_time(i18n, run.started_at.as_deref())}
                            }
                        }
                    }
                }
            }
        },
    )
}
