//! The canonicalisation review queue, with merge and dismiss actions, and the compact
//! read-only series card the compare view is built from.

use crate::api;
use crate::components::{SkeletonBlock, EmptyBox, Cover, ErrorBox};
use crate::hooks::{use_reload, Reload};
use crate::i18n::use_i18n;
use crate::models::*;
use crate::state::use_session;
use dioxus::prelude::*;
use progenitor_client::ResponseValue;

/// Canonicalisation review queue with merge / dismiss actions.
#[component]
pub(super) fn MergeQueue() -> Element {
    let api = api::use_api();
    let i18n = use_i18n();
    let reload = use_reload();
    let resource = use_resource(move || {
        reload.track();
        let client = api.client();
        async move {
            client
                .list_merge_candidates()
                .send()
                .await
                .map(ResponseValue::into_inner)
                .map_err(|e| api::friendly_error(i18n, e))
        }
    });

    let body = match &*resource.read_unchecked() {
        None => rsx! { SkeletonBlock { height: 60 } },
        Some(Err(e)) => {
            let msg = e.clone();
            rsx! {
                ErrorBox { message: msg, on_retry: move |()| reload.bump() }
            }
        }
        Some(Ok(list)) if list.is_empty() => rsx! {
            EmptyBox { message: i18n.t("console.merge.empty") }
        },
        Some(Ok(list)) => {
            let list = list.clone();
            rsx! {
                for c in list {
                    MergeRow { key: "{c.id}", candidate: Signal::new(c), reload }
                }
            }
        }
    };

    rsx! {
        section {
            h3 { {i18n.t("console.tab.merge")} }
            {body}
        }
    }
}

#[component]
pub(super) fn MergeRow(candidate: Signal<MergeCandidate>, reload: Reload) -> Element {
    let api = api::use_api();
    let i18n = use_i18n();
    let session = use_session();
    let can = candidate.read();
    // The score is a 0..=1 ratio, so the rounded percentage is always in range; clamping
    // makes that total rather than relying on the input being well-formed.
    #[allow(clippy::cast_possible_truncation)]
    let pct = (can.score * 100.0).round().clamp(0.0, 100.0) as i32;
    let id = can.id;
    let a = can.series_id;
    let b = can.candidate_id;
    let mut open = use_signal(|| false);
    let mut busy = use_signal(|| false);

    // Higher-confidence matches get a warmer pill so operators can triage at a glance.
    let score_class = if pct >= 90 {
        "ik-mono acc"
    } else if pct >= 75 {
        "ik-mono jade"
    } else {
        "ik-mono ik-muted"
    };

    let series_title = can.series_title.clone();
    let candidate_title = can.candidate_title.clone();
    let reason = can.reason.clone();

    let merge = {
        move |_| {
            if *busy.peek() {
                return;
            }
            busy.set(true);
            spawn(async move {
                let client = api.client();
                if session.token_value().is_some()
                    && client
                        .merge_series()
                        .body(MergeRequest { keep: a, merge: b })
                        .send()
                        .await
                        .is_ok()
                {
                    reload.bump();
                }
                busy.set(false);
            });
        }
    };

    let dismiss = {
        move |_| {
            if *busy.peek() {
                return;
            }
            busy.set(true);
            spawn(async move {
                let client = api.client();
                if session.token_value().is_some()
                    && client
                        .dismiss_merge_candidate()
                        .body(DismissRequest { id })
                        .send()
                        .await
                        .is_ok()
                {
                    reload.bump();
                }
                busy.set(false);
            });
        }
    };

    let keep_id = a;
    let drop_id = b;

    rsx! {
        div { class: "ik-card", style: "margin-bottom:10px;",
            div { class: "ik-row",
                div { class: "grow",
                    div { class: "ik-flex", style: "justify-content:space-between;align-items:center;",
                        span { style: "font-weight:600;", "{series_title}" }
                        span { class: "{score_class}",
                            {i18n.args("console.merge.score", &[("percent", &pct.to_string())])}
                        }
                    }
                    div { class: "ik-muted", style: "font-size:13px;", "↔ {candidate_title}" }
                    if let Some(r) = &reason {
                        div { class: "ik-muted", style: "font-size:12px;",
                            {i18n.args("console.merge.reason", &[("reason", r)])}
                        }
                    }
                }
                button {
                    class: "ik-btn",
                    onclick: move |_| { let v = *open.peek(); open.set(!v); },
                    if *open.read() {
                        {i18n.t("console.merge.hide")}
                    } else {
                        {i18n.t("console.merge.compare")}
                    }
                }
                button { class: "ik-btn primary", disabled: *busy.read(), onclick: merge,
                    {i18n.t("console.merge.merge")}
                }
                button { class: "ik-btn", disabled: *busy.read(), onclick: dismiss,
                    {i18n.t("console.merge.distinct")}
                }
            }
            if *open.read() {
                div { class: "ik-flex", style: "gap:14px;margin-top:12px;align-items:stretch;flex-wrap:wrap;",
                    div { style: "flex:1;min-width:240px;",
                        div { class: "ik-pill jade", style: "margin-bottom:6px;",
                            {i18n.t("console.merge.keep")}
                        }
                        SeriesMiniCard { series_id: keep_id }
                    }
                    div { style: "flex:1;min-width:240px;",
                        div { class: "ik-pill", style: "margin-bottom:6px;",
                            {i18n.t("console.merge.drop")}
                        }
                        SeriesMiniCard { series_id: drop_id }
                    }
                }
            }
        }
    }
}

/// Compact read-only "manga info" card for a single series, used by the merge compare view
/// and the Sync inspector. Fetches the public series detail so operators can eyeball cover,
/// type/status, sources and tags before acting.
#[component]
pub(super) fn SeriesMiniCard(series_id: SeriesId) -> Element {
    let api = api::use_api();
    let i18n = use_i18n();
    let res = use_resource(move || {
        let client = api.client();
        async move {
            client
                .detail()
                .id(series_id)
                .send()
                .await
                .map(ResponseValue::into_inner)
                .map_err(|e| api::friendly_error(i18n, e))
        }
    });

    match &*res.read_unchecked() {
        None => rsx! { SkeletonBlock { height: 120 } },
        Some(Err(e)) => rsx! {
            div { class: "ik-empty", style: "font-size:12px;",
                {i18n.args("console.merge.seriesUnavailable", &[("message", e)])}
            }
        },
        Some(Ok(d)) => {
            let d = d.clone();
            let year = d.release_year.map(|y| y.to_string()).unwrap_or_default();
            let tags: Vec<String> = d.tags.iter().take(6).map(|t| t.name.clone()).collect();
            rsx! {
                div { class: "ik-card", style: "padding:10px;",
                    div { class: "ik-flex", style: "gap:10px;align-items:flex-start;",
                        div { style: "width:56px;flex:0 0 auto;",
                            Cover { url: d.cover_url.clone(), title: d.title.clone() }
                        }
                        div { class: "grow",
                            div { style: "font-weight:600;", "{d.title}" }
                            div { class: "ik-flex", style: "gap:6px;margin-top:4px;flex-wrap:wrap;",
                                span { class: "ik-pill", {i18n.t(d.content_type.label_key())} }
                                span { class: "ik-pill", {i18n.t(d.status.label_key())} }
                                if !year.is_empty() {
                                    span { class: "ik-pill", "{year}" }
                                }
                                span { class: "ik-pill",
                                    {
                                        i18n.plural(
                                            "series.sources",
                                            i64::try_from(d.sources.len()).unwrap_or(0),
                                            &[],
                                        )
                                    }
                                }
                            }
                            div { class: "ik-mono ik-muted", style: "font-size:11px;margin-top:4px;word-break:break-all;",
                                "{d.id}"
                            }
                        }
                    }
                    if !d.sources.is_empty() {
                        div { style: "margin-top:8px;",
                            for s in d.sources.iter().take(5) {
                                div { class: "ik-muted", style: "font-size:12px;",
                                    {
                                        let count = i18n.args(
                                            "series.chapterCount",
                                            &[("count", &s.chapter_count.to_string())],
                                        );
                                        format!("· {} — {count}", s.provider_name)
                                    }
                                }
                            }
                        }
                    }
                    if !tags.is_empty() {
                        div { class: "ik-flex", style: "gap:4px;margin-top:8px;flex-wrap:wrap;",
                            for t in tags {
                                span { class: "ik-pill", style: "font-size:11px;", "{t}" }
                            }
                        }
                    }
                }
            }
        }
    }
}
