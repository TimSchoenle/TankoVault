//! Owns the scan queue's controls: trigger a run, drain the queue, and narrow the panel.

use crate::api;
use crate::components::{use_step_up_gate, StepUpGuard};
use crate::i18n::use_i18n;
use crate::models::*;
use crate::views::console::RefreshTick;
use dioxus::prelude::*;
use inkstone_ui::{Button, Tone};

use super::{filters, ScanFilter};

/// The trigger and drain buttons, the filter bar, and the outcome of the last action.
#[component]
pub(super) fn QueueControls(filter: ScanFilter, tick: RefreshTick) -> Element {
    let api = api::use_api();
    let i18n = use_i18n();
    let gate = use_step_up_gate();

    let mut mode = use_signal(|| ScanMode::Fast);
    let mut message = use_signal(|| Option::<String>::None);

    let trigger = move |_| {
        gate.attempt(move || {
            let chosen = *mode.read();
            // Elevated: triggering a run is a mutating operator capability, which the API answers
            // `403 step_up_required` to until it has a second factor.
            let client = gate.client(api);
            spawn(async move {
                let body = TriggerScan {
                    mode: chosen,
                    provider_id: None,
                };
                match client.trigger_scan().body(body).send().await {
                    // Body carries the planner's run_ids, which this view doesn't render.
                    Ok(_) => {
                        message.set(Some(i18n.t("console.scans.queued")));
                        tick.bump();
                    }
                    Err(e) => {
                        if !gate.refused(api::Refusal::of(&e)) {
                            message.set(Some(api::guarded_error(i18n, e)));
                        }
                    }
                }
            });
        });
    };

    // Narrowed by the panel's own provider filter, so "stop the queue" means what the operator is
    // looking at: with a provider selected it drains that provider, and with none it drains
    // everything. A button that always stopped everything would be unusable on the one screen
    // that exists to narrow to a single misbehaving provider.
    let drain_provider = filter.provider.clone();
    let drain = move |_| {
        let provider = drain_provider.clone();
        gate.attempt(move || {
            let provider = provider.clone();
            let client = gate.client(api);
            spawn(async move {
                let body = CancelScansBody {
                    provider,
                    mode: None,
                };
                match client.cancel_scans().body(body).send().await {
                    Ok(stopped) => {
                        let stopped = stopped.into_inner();
                        message.set(Some(i18n.args(
                            "console.scans.cancelled",
                            &[
                                ("runs", &stopped.runs.to_string()),
                                ("tasks", &stopped.tasks.to_string()),
                            ],
                        )));
                        tick.bump();
                    }
                    Err(e) => {
                        if !gate.refused(api::Refusal::of(&e)) {
                            message.set(Some(api::guarded_error(i18n, e)));
                        }
                    }
                }
            });
        });
    };

    rsx! {
        div { class: "ik-flex", style: "justify-content:space-between;flex-wrap:wrap;",
            h3 { style: "margin:0;", {i18n.t("console.scans.title")} }
            div { class: "ik-flex",
                select {
                    class: "ik-input",
                    style: "width:auto;",
                    "aria-label": i18n.t("console.scans.triggerMode"),
                    value: ScanModeExt::token(*mode.read()),
                    onchange: move |event: FormEvent| {
                        if let Some(chosen) = ScanModeExt::parse(&event.value()) {
                            mode.set(chosen);
                        }
                    },
                    for option in <ScanMode as ScanModeExt>::all().iter().copied() {
                        option {
                            key: "{ScanModeExt::token(option)}",
                            value: ScanModeExt::token(option),
                            {i18n.t(ScanModeExt::label_key(option))}
                        }
                    }
                }
                Button {
                    tone: Tone::Primary,
                    on_click: trigger,
                    {i18n.t("console.scans.trigger")}
                }
                Button {
                    tone: Tone::Danger,
                    on_click: drain,
                    {i18n.t("console.scans.cancelAll")}
                }
            }
        }

        filters::FilterBar { filter: filter.clone() }

        StepUpGuard { gate, intro: Some(i18n.t("console.stepUp.intro")) }
        if let Some(m) = message.read().clone() {
            p { class: "ik-muted", style: "margin:8px 0 0;", "{m}" }
        }
    }
}
