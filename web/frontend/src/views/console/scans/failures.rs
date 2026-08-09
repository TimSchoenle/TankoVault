//! The failure triage feed, grouped or flat, and the clear that empties it.
//!
//! # Clearing is acknowledgement, never deletion
//!
//! A cleared failure keeps its `failed` state, its error text and its contribution to the run
//! counters; only the feed stops showing it. That is why the "show cleared" filter can always
//! reopen the full window, and why the totals in the health strip still reconcile against the
//! history after an operator has emptied the feed.

use super::ScanFilter;
use crate::api;
use crate::components::{async_block, use_step_up_gate, InlineConfirm, StepUpPrompt};
use crate::i18n::use_i18n;
use crate::models::{ClearFailuresBody, FailedTask, FailureGroup};
use crate::util::thousands;
use crate::views::console::RefreshTick;
use dioxus::prelude::*;

/// The failure feed with its view toggle and clear controls.
#[component]
pub(super) fn FailuresSection(
    filter: ScanFilter,
    failures: Resource<Result<Vec<FailedTask>, String>>,
    groups: Resource<Result<Vec<FailureGroup>, String>>,
    tick: RefreshTick,
) -> Element {
    let api = api::use_api();
    let i18n = use_i18n();
    let gate = use_step_up_gate();

    // Grouping is a *view* of the same failures, not a different set, so it stays a local
    // toggle: it changes nothing an operator would want to send someone.
    let mut grouped = use_signal(|| true);
    let mut busy = use_signal(|| false);
    let mut outcome = use_signal(|| Option::<Result<String, String>>::None);
    // What a confirmation is currently asking about. `None` is "nothing pending"; the inner
    // `Option<String>` is the error group, where `None` means the group that recorded none —
    // which is why this is not flattened into one level.
    let mut pending = use_signal(|| Option::<Option<Option<String>>>::None);

    let window = filter.since;
    let provider = filter.provider.clone();
    let mut clear = move |error: Option<Option<String>>| {
        let client = gate.client(api);
        let body = ClearFailuresBody {
            provider: provider.clone(),
            since: window.since_iso(),
            run_id: None,
            match_null_error: Some(matches!(error, Some(None))),
            error: error.flatten(),
        };
        busy.set(true);
        spawn(async move {
            let result = client.clear_scan_failures().body(body).send().await;
            busy.set(false);
            pending.set(None);
            match result {
                Ok(response) => {
                    let cleared = response.into_inner().cleared;
                    outcome.set(Some(Ok(i18n.args(
                        "console.scan.failures.cleared",
                        &[("count", &thousands(cleared))],
                    ))));
                    tick.bump();
                }
                Err(e) => {
                    if !gate.refused(api::Refusal::of(&e)) {
                        outcome.set(Some(Err(api::guarded_error(i18n, e))));
                    }
                }
            }
        });
    };

    rsx! {
        div { class: "ik-flex", style: "justify-content:space-between;align-items:center;gap:10px;flex-wrap:wrap;margin-top:16px;",
            div { class: "ik-subhead", style: "margin:0;", {i18n.t("console.scans.failures")} }
            div { class: "ik-flex", style: "gap:10px;flex-wrap:wrap;",
                label { class: "ik-flex", style: "gap:5px;font-size:12px;align-items:center;",
                    input {
                        r#type: "checkbox",
                        checked: *grouped.read(),
                        onchange: move |event: FormEvent| grouped.set(event.checked()),
                    }
                    {i18n.t("console.scan.failures.grouped")}
                }
                button {
                    class: "ik-btn xs",
                    disabled: *busy.read(),
                    onclick: move |_| pending.set(Some(None)),
                    {i18n.t("console.scan.failures.clearAll")}
                }
            }
        }

        if let Some(target) = pending.read().clone() {
            InlineConfirm {
                title: i18n.t("console.scan.failures.confirmTitle"),
                // Names the exact selection, because the clear follows the *filter*, not the
                // rows on screen: an operator who has scrolled past a page of failures must not
                // be told they are clearing twenty when the window holds four hundred.
                body: confirm_body(i18n, &filter, target.as_ref()),
                cta: i18n.t("console.scan.failures.clearCta"),
                busy: *busy.read(),
                on_cancel: move |()| pending.set(None),
                on_confirm: move |()| clear(target.clone()),
            }
        }
        if gate.is_open() {
            StepUpPrompt {
                enrolled: true,
                intro: Some(i18n.t("console.stepUp.intro")),
                on_done: move |()| {
                    gate.close();
                    outcome.set(Some(Ok(i18n.t("stepUp.confirmedRetry"))));
                },
            }
        }
        crate::components::OutcomeLine { outcome: outcome.read().clone() }

        if *grouped.read() {
            {
                async_block(
                    &groups,
                    tick.reload(),
                    60,
                    |rows| {
                        rsx! {
                            GroupedFailures {
                                groups: rows.clone(),
                                on_clear: move |error| pending.set(Some(Some(error))),
                            }
                        }
                    },
                )
            }
        } else {
            {
                async_block(
                    &failures,
                    tick.reload(),
                    60,
                    |rows| {
                        rsx! {
                            FailuresPanel { failures: rows.clone() }
                        }
                    },
                )
            }
        }
    }
}

/// What a pending clear will actually hide, in words.
fn confirm_body(
    i18n: crate::i18n::Translator,
    filter: &ScanFilter,
    error: Option<&Option<String>>,
) -> String {
    let scope = filter
        .provider
        .clone()
        .unwrap_or_else(|| i18n.t("console.scans.scopeAll"));
    let window = i18n.t(filter.since.label_key());
    match error {
        None => i18n.args(
            "console.scan.failures.confirmAll",
            &[("provider", &scope), ("window", &window)],
        ),
        Some(text) => {
            let named = text
                .clone()
                .unwrap_or_else(|| i18n.t("console.scan.failures.noError"));
            i18n.args(
                "console.scan.failures.confirmGroup",
                &[("error", &named), ("provider", &scope), ("window", &window)],
            )
        }
    }
}

/// Failures collapsed by their error text, worst first.
///
/// One broken selector that hit twelve series is one problem. The flat feed presents it as
/// twelve rows of the same sentence, and on a bad day that is the entire feed.
#[component]
fn GroupedFailures(groups: Vec<FailureGroup>, on_clear: EventHandler<Option<String>>) -> Element {
    let i18n = use_i18n();
    if groups.is_empty() {
        return rsx! {
            p { class: "ik-muted", style: "font-size:13px;margin:6px 0 0;",
                {i18n.t("console.scans.noFailures")}
            }
        };
    }
    rsx! {
        div { style: "margin-top:8px;display:grid;gap:8px;",
            for group in groups {
                div { key: "{group.error:?}", class: "ik-fail",
                    div { class: "ik-flex", style: "justify-content:space-between;gap:10px;flex-wrap:wrap;",
                        div { class: "ik-flex", style: "gap:8px;flex-wrap:wrap;",
                            span { class: "ik-pill vermilion", "{group.count}×" }
                            for slug in group.providers.clone() {
                                span { key: "{slug}", class: "ik-pill", "{slug}" }
                            }
                            for kind in group.kinds.clone() {
                                span { key: "{kind}", class: "ik-pill", "{kind}" }
                            }
                            if group.cleared > 0 {
                                span { class: "ik-pill", style: "opacity:.7;",
                                    {
                                        i18n.args(
                                            "console.scan.failures.clearedCount",
                                            &[("count", &thousands(group.cleared))],
                                        )
                                    }
                                }
                            }
                        }
                        div { class: "ik-flex", style: "gap:8px;",
                            span { class: "ik-muted ik-mono", style: "font-size:12px;",
                                "{crate::util::rel_time(i18n, group.latest_at.as_deref())}"
                            }
                            button {
                                class: "ik-btn xs",
                                onclick: {
                                    let error = group.error.clone();
                                    move |_| on_clear.call(error.clone())
                                },
                                {i18n.t("console.scan.failures.clear")}
                            }
                        }
                    }
                    p {
                        class: "ik-mono",
                        style: "margin:6px 0 0;font-size:12px;color:var(--vermilion);word-break:break-word;",
                        {
                            group
                                .error
                                .clone()
                                .unwrap_or_else(|| i18n.t("console.scan.failures.noError"))
                        }
                    }
                }
            }
        }
    }
}

/// Recent task failures with their errors — the operator's triage feed.
#[component]
fn FailuresPanel(failures: Vec<FailedTask>) -> Element {
    let i18n = use_i18n();
    if failures.is_empty() {
        return rsx! {
            p { class: "ik-muted", style: "font-size:13px;margin:6px 0 0;",
                {i18n.t("console.scans.noFailures")}
            }
        };
    }
    rsx! {
        div { style: "margin-top:8px;display:grid;gap:8px;",
            for failure in failures {
                div {
                    key: "{failure.id}",
                    class: "ik-fail",
                    // A cleared row is only ever on screen because the operator asked to see
                    // cleared ones, so it is dimmed rather than badged as an alert.
                    style: if failure.acknowledged_at.is_some() { "opacity:.6;" } else { "" },
                    div { class: "ik-flex", style: "justify-content:space-between;gap:10px;flex-wrap:wrap;",
                        div { class: "ik-flex", style: "gap:8px;flex-wrap:wrap;",
                            span { class: "ik-pill vermilion", "{failure.kind}" }
                            span { class: "ik-mono ik-muted", style: "font-size:12px;",
                                {
                                    let slug = failure
                                        .provider_slug
                                        .clone()
                                        .unwrap_or_else(|| i18n.t("time.unknown"));
                                    i18n.args(
                                        "console.scans.failureMeta",
                                        &[
                                            ("provider", &slug),
                                            ("mode", &failure.mode),
                                            ("attempts", &failure.attempts.to_string()),
                                        ],
                                    )
                                }
                            }
                            if failure.acknowledged_at.is_some() {
                                span { class: "ik-pill", {i18n.t("console.scan.failures.clearedBadge")} }
                            }
                        }
                        span { class: "ik-muted ik-mono", style: "font-size:12px;",
                            "{crate::util::rel_time(i18n, failure.finished_at.as_deref())}"
                        }
                    }
                    p { class: "ik-mono", style: "margin:6px 0 0;font-size:12px;color:var(--vermilion);word-break:break-word;",
                        {failure.error.clone().unwrap_or_else(|| i18n.t("console.scans.noErrorMessage"))}
                    }
                }
            }
        }
    }
}
