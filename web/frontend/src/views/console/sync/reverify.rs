//! Re-verifying the stored `AniList` mappings: start a run, follow it, review what it refused.
//!
//! A real run removes and blocks every mapping it refuses, so the panel offers it only once a
//! dry run has finished and its list is on screen to review.

use super::use_sync_gate;
use crate::api;
use crate::components::{async_view, ErrorLine, Kpi, SkeletonBlock};
use crate::hooks::use_reload;
use crate::i18n::{use_i18n, Translator};
use crate::state::capabilities::use_capabilities;
use crate::util::{rel_time, thousands};
use crate::wire::types::{MappingReverifyReason, MappingReverifyView, Permission};
use dioxus::prelude::*;
use inkstone_ui::{Button, Tone};
use progenitor_client::ResponseValue;

/// How often the panel re-reads itself while a run holds the claim. The run publishes its
/// counters once per page of rate-limited lookups, so a faster tick re-reads unchanged numbers.
const REVERIFY_POLL_MS: u32 = 5_000;

/// Failures listed on screen. The count above the list is exact.
const LISTED: usize = 100;

#[component]
pub(super) fn MappingReverifyPanel() -> Element {
    let api = api::use_api();
    let i18n = use_i18n();
    let reload = use_reload();

    let state = use_resource(move || {
        reload.track();
        let client = api.client();
        async move {
            client
                .mapping_reverify_status()
                .send()
                .await
                .map(ResponseValue::into_inner)
                .map_err(|e| api::friendly_error(i18n, e))
        }
    });

    // Read inside the loop: this future runs once, and a flag captured at mount would stay
    // `false` for a run started from this panel.
    use_future(move || async move {
        loop {
            crate::platform::sleep_ms(REVERIFY_POLL_MS).await;
            if matches!(&*state.read_unchecked(), Some(Ok(view)) if view.running) {
                reload.bump();
            }
        }
    });

    rsx! {
        {
            async_view(
                &state,
                reload,
                || rsx! { SkeletonBlock { height: 160 } },
                |view| rsx! { ReverifyBody { view: view.clone() } },
            )
        }
    }
}

#[component]
fn ReverifyBody(view: MappingReverifyView) -> Element {
    let api = api::use_api();
    let i18n = use_i18n();
    let caps = use_capabilities();
    let gate = use_sync_gate();
    let reload = use_reload();
    let mut busy = use_signal(|| false);
    let mut notice = use_signal(String::new);

    let start = use_callback(move |dry_run: bool| {
        if *busy.peek() {
            return;
        }
        busy.set(true);
        notice.set(String::new());
        spawn(async move {
            let client = gate.client(api);
            match client
                .start_mapping_reverify()
                .body_map(|body| body.dry_run(dry_run))
                .send()
                .await
            {
                Ok(_) => {
                    notice.set(i18n.t(if dry_run {
                        "console.sync.reverify.dryRunStarted"
                    } else {
                        "console.sync.reverify.runStarted"
                    }));
                    reload.bump();
                }
                Err(e) => {
                    if !gate.refused(api::Refusal::of(&e)) {
                        notice.set(i18n.args(
                            "console.sync.reverify.failed",
                            &[("message", &api::guarded_error(i18n, e))],
                        ));
                    }
                }
            }
            busy.set(false);
        });
    });

    let can_write = caps.can(Permission::SyncAdminWrite);
    let finished = view.finished_at.is_some() && !view.running;
    // The destructive run is offered only beside the dry run it would act on.
    let can_apply = can_write && finished && view.dry_run && view.error.is_none();
    let when = if view.running {
        i18n.args(
            "console.sync.reverify.startedAt",
            &[("when", &rel_time(i18n, view.started_at.as_deref()))],
        )
    } else if view.finished_at.is_some() {
        i18n.args(
            "console.sync.reverify.finishedAt",
            &[("when", &rel_time(i18n, view.finished_at.as_deref()))],
        )
    } else {
        i18n.t("console.sync.reverify.never")
    };
    let mode = i18n.t(if view.dry_run {
        "console.sync.reverify.modeDry"
    } else {
        "console.sync.reverify.modeReal"
    });
    let failed_accent = if view.failed > 0 { "warn" } else { "" }.to_owned();
    let listed = view.failures.len().min(LISTED);

    rsx! {
        div { class: "ik-flex", style: "gap:8px;align-items:center;margin-bottom:12px;flex-wrap:wrap;",
            span {
                class: if view.running { "ik-pill run" } else { "ik-pill" },
                if view.running {
                    {i18n.t("console.sync.reverify.running")}
                } else {
                    {i18n.t("console.sync.reverify.idle")}
                }
            }
            if view.started_at.is_some() {
                span { class: "ik-pill", "{mode}" }
            }
            span { class: "ik-mono ik-muted", style: "font-size:11.5px;", "{when}" }
        }

        if let Some(error) = view.error.clone() {
            ErrorLine { message: i18n.args("console.sync.reverify.lastError", &[("message", &error)]) }
        }

        div { class: "ik-kpis",
            Kpi {
                label: i18n.t("console.sync.reverify.kpi.checked"),
                value: thousands(i64::from(view.checked)),
                sub: i18n.t("console.sync.reverify.kpi.checkedSub"),
            }
            Kpi {
                label: i18n.t("console.sync.reverify.kpi.failed"),
                value: thousands(i64::from(view.failed)),
                sub: i18n.t("console.sync.reverify.kpi.failedSub"),
                accent: failed_accent,
            }
            Kpi {
                label: i18n.t("console.sync.reverify.kpi.removed"),
                value: thousands(i64::from(view.removed)),
                sub: i18n.t("console.sync.reverify.kpi.removedSub"),
            }
            Kpi {
                label: i18n.t("console.sync.reverify.kpi.unreachable"),
                value: thousands(i64::from(view.unreachable)),
                sub: i18n.t("console.sync.reverify.kpi.unreachableSub"),
            }
        }

        if can_write {
            div { class: "ik-flex", style: "gap:8px;margin-top:12px;flex-wrap:wrap;align-items:center;",
                Button {
                    tone: Tone::Primary,
                    disabled: view.running || *busy.read(),
                    on_click: move |_| gate.attempt(move || start.call(true)),
                    {i18n.t("console.sync.reverify.dryRun")}
                }
                Button {
                    tone: Tone::Danger,
                    disabled: !can_apply || *busy.read(),
                    on_click: move |_| gate.attempt(move || start.call(false)),
                    {
                        i18n.args(
                            "console.sync.reverify.apply",
                            &[("count", &thousands(i64::from(view.failed)))],
                        )
                    }
                }
                span { class: "ik-muted", style: "font-size:12px;max-width:60ch;",
                    {i18n.t("console.sync.reverify.applyNote")}
                }
            }
        }
        if !notice.read().is_empty() {
            p { style: "font-size:12px;margin:8px 0 0;", "{notice}" }
        }

        if finished && !view.failures.is_empty() {
            div { style: "margin-top:12px;",
                p { class: "ik-muted", style: "font-size:12px;margin:0 0 6px;",
                    {
                        i18n.args(
                            "console.sync.reverify.listed",
                            &[
                                ("shown", &listed.to_string()),
                                ("total", &thousands(i64::from(view.failed))),
                            ],
                        )
                    }
                }
                div { class: "ik-tablewrap scroll",
                table { class: "ik-table ik-table-compact",
                    thead {
                        tr {
                            th { {i18n.t("console.sync.reverify.col.series")} }
                            th { {i18n.t("console.sync.reverify.col.external")} }
                            th { {i18n.t("console.sync.reverify.col.reason")} }
                        }
                    }
                    tbody {
                        for failure in view.failures.iter().take(LISTED) {
                            tr { key: "{failure.series_id}-{failure.provider}",
                                td { class: "ik-mono", "{failure.series_id}" }
                                td { class: "ik-mono", "{failure.provider}:{failure.external_id}" }
                                td { {reason_label(i18n, failure.reason)} }
                            }
                        }
                    }
                }
                }
            }
        }
    }
}

fn reason_label(i18n: Translator, reason: MappingReverifyReason) -> String {
    i18n.t(match reason {
        MappingReverifyReason::NotALibraryRecord => {
            "console.sync.reverify.reason.notALibraryRecord"
        }
        MappingReverifyReason::TitlesDiffer => "console.sync.reverify.reason.titlesDiffer",
    })
}
