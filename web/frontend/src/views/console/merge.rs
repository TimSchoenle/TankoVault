//! The canonicalisation review queue, with merge and dismiss actions, the standing duplicate
//! sweep, and the compact read-only series card the compare view is built from.
//!
//! # What this surface has to answer
//!
//! On a real catalogue this queue is thousands of rows long, and a row is only actionable if it
//! answers three questions without opening both series: *is this actually one work*, *which of
//! the two should survive*, and *why does the matcher think so*. It used to answer none of them
//! — every row carried the same "ambiguous title match" reason, the list was ordered by
//! insertion time so the certain duplicates were buried among the coincidences, and the merge
//! button always kept whichever series the scan happened to create *second*, deleting the older,
//! richer one.
//!
//! So: the band filter narrows to a confidence range, the server orders by score, the signal
//! badges say which rule fired, the counts say which side carries more of the catalogue, and the
//! direction defaults to the server's `suggested_keep` with an explicit swap.

use crate::api;
use crate::components::{async_block, async_block_list, Cover};
use crate::hooks::{use_reload, Reload};
use crate::i18n::use_i18n;
use crate::models::*;
use crate::state::use_session;
use dioxus::prelude::*;
use progenitor_client::ResponseValue;

/// The confidence bands an operator triages in.
///
/// Working a large queue means working it in bands: above 90% is nearly all genuine duplicates
/// and can be actioned quickly, while 60–75% needs real attention per row. Filtering server-side
/// keeps the page small as well as the list relevant.
const BANDS: &[(f32, &str)] = &[
    (0.0, "console.merge.bandAll"),
    (0.6, "console.merge.bandLow"),
    (0.75, "console.merge.bandMed"),
    (0.9, "console.merge.bandHigh"),
];

/// Canonicalisation review queue with merge / dismiss actions and the duplicate sweep.
#[component]
pub(super) fn MergeQueue() -> Element {
    let api = api::use_api();
    let i18n = use_i18n();
    let session = use_session();
    let reload = use_reload();
    let mut min_score = use_signal(|| 0.0_f32);
    let mut notice = use_signal(String::new);
    let mut busy = use_signal(|| false);

    let resource = use_resource(move || {
        reload.track();
        let threshold = *min_score.read();
        let client = api.client();
        async move {
            client
                .list_merge_candidates()
                .min_score(threshold)
                .send()
                .await
                .map(ResponseValue::into_inner)
                .map_err(|e| api::friendly_error(i18n, e))
        }
    });

    let empty = i18n.t("console.merge.empty");
    let body = async_block_list(&resource, reload, 60, &empty, |list| {
        let list = list.to_vec();
        rsx! {
            for c in list {
                MergeRow { key: "{c.id}", candidate: Signal::new(c), reload }
            }
        }
    });

    // The sweep is the only control here that acts on rows the operator cannot see — it finds
    // duplicates the queue never recorded — so its outcome is reported as a line of text rather
    // than left to be inferred from the list changing length.
    let run_sweep = move |_| {
        if *busy.peek() {
            return;
        }
        busy.set(true);
        notice.set(String::new());
        spawn(async move {
            let client = api.client();
            if session.token_value().is_some() {
                match client.sweep_merge_candidates().send().await {
                    Ok(r) => {
                        let r = r.into_inner();
                        notice.set(i18n.args(
                            "console.merge.sweepDone",
                            &[
                                ("examined", &r.pairs_examined.to_string()),
                                ("merged", &r.auto_merged.to_string()),
                                ("queued", &r.queued.to_string()),
                                ("withdrawn", &r.withdrawn.to_string()),
                            ],
                        ));
                        reload.bump();
                    }
                    Err(e) => notice.set(i18n.args(
                        "console.merge.actionFailed",
                        &[("message", &api::friendly_error(i18n, e))],
                    )),
                }
            }
            busy.set(false);
        });
    };

    let rebuild_keys = move |_| {
        if *busy.peek() {
            return;
        }
        busy.set(true);
        notice.set(String::new());
        spawn(async move {
            let client = api.client();
            if session.token_value().is_some() {
                match client.rebuild_matching_keys().send().await {
                    Ok(r) => {
                        let r = r.into_inner();
                        notice.set(i18n.args(
                            "console.merge.rebuildDone",
                            &[
                                ("series", &r.series_updated.to_string()),
                                ("titles", &r.titles_updated.to_string()),
                            ],
                        ));
                        reload.bump();
                    }
                    Err(e) => notice.set(i18n.args(
                        "console.merge.actionFailed",
                        &[("message", &api::friendly_error(i18n, e))],
                    )),
                }
            }
            busy.set(false);
        });
    };

    let active = *min_score.read();
    rsx! {
        section {
            h3 { {i18n.t("console.tab.merge")} }
            div { class: "ik-row", style: "gap:8px;flex-wrap:wrap;margin-bottom:12px;",
                div { class: "ik-flex", style: "gap:4px;flex-wrap:wrap;",
                    for (threshold , label) in BANDS.iter().copied() {
                        button {
                            key: "{label}",
                            class: if (active - threshold).abs() < f32::EPSILON { "ik-btn primary" } else { "ik-btn" },
                            onclick: move |_| min_score.set(threshold),
                            {i18n.t(label)}
                        }
                    }
                }
                div { class: "grow" }
                button { class: "ik-btn", disabled: *busy.read(), onclick: run_sweep,
                    {i18n.t("console.merge.sweep")}
                }
                button { class: "ik-btn", disabled: *busy.read(), onclick: rebuild_keys,
                    {i18n.t("console.merge.rebuildKeys")}
                }
            }
            if !notice.read().is_empty() {
                div { class: "ik-card", style: "margin-bottom:12px;padding:10px;",
                    "{notice}"
                }
            }
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
    #[expect(
        clippy::cast_possible_truncation,
        reason = "the score is a 0..=1 ratio and the clamp makes the percentage total rather \
                  than trusting the input to be well-formed"
    )]
    let pct = (can.score * 100.0).round().clamp(0.0, 100.0) as i32;
    let id = can.id;
    let mut open = use_signal(|| false);
    let mut busy = use_signal(|| false);
    // Which side survives. Seeded from the server's suggestion — the series with more sources,
    // then more chapters — because the absorbed id stops existing and everything already
    // pointing at it breaks. Swappable, because the operator can see something the counts cannot.
    let mut keep_first = use_signal(|| can.suggested_keep == can.series_id);

    // Higher-confidence matches get a warmer pill so operators can triage at a glance.
    let score_class = if pct >= 90 {
        "ik-mono acc"
    } else if pct >= 75 {
        "ik-mono jade"
    } else {
        "ik-mono ik-muted"
    };

    let first = SideSummary {
        id: can.series_id,
        title: can.series_title.clone(),
        sources: can.series_sources,
        chapters: can.series_chapters,
    };
    let second = SideSummary {
        id: can.candidate_id,
        title: can.candidate_title.clone(),
        sources: can.candidate_sources,
        chapters: can.candidate_chapters,
    };
    let signals = can.signals.clone();
    let reason = can.reason.clone();
    drop(can);

    let (keep, drop_side) = if *keep_first.read() {
        (first.clone(), second.clone())
    } else {
        (second.clone(), first.clone())
    };
    let keep_id = keep.id;
    let drop_id = drop_side.id;

    let merge = move |_| {
        if *busy.peek() {
            return;
        }
        busy.set(true);
        spawn(async move {
            let client = api.client();
            if session.token_value().is_some()
                && client
                    .merge_series()
                    .body(MergeRequest {
                        keep: keep_id,
                        merge: drop_id,
                    })
                    .send()
                    .await
                    .is_ok()
            {
                reload.bump();
            }
            busy.set(false);
        });
    };

    let dismiss = move |_| {
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
    };

    let keep_title = keep.title.clone();
    rsx! {
        div { class: "ik-card", style: "margin-bottom:10px;",
            div { class: "ik-row",
                div { class: "grow",
                    div { class: "ik-flex", style: "justify-content:space-between;align-items:center;",
                        span { style: "font-weight:600;", "{keep.title}" }
                        span { class: "{score_class}",
                            {i18n.args("console.merge.score", &[("percent", &pct.to_string())])}
                        }
                    }
                    div { class: "ik-muted", style: "font-size:13px;", "↔ {drop_side.title}" }
                    div { class: "ik-flex", style: "gap:4px;margin-top:6px;flex-wrap:wrap;",
                        for s in signals.iter() {
                            span { key: "{s}", class: "ik-pill", style: "font-size:11px;",
                                {i18n.t(&format!("console.merge.signal.{s}"))}
                            }
                        }
                    }
                    div { class: "ik-muted", style: "font-size:12px;margin-top:4px;",
                        {
                            i18n.args(
                                "console.merge.sides",
                                &[
                                    ("keep", &keep.summary(i18n)),
                                    ("drop", &drop_side.summary(i18n)),
                                ],
                            )
                        }
                    }
                    if let Some(r) = &reason {
                        div { class: "ik-muted", style: "font-size:12px;",
                            {i18n.args("console.merge.reason", &[("reason", r)])}
                        }
                    }
                }
                button {
                    class: "ik-btn",
                    onclick: move |_| { let v = *keep_first.peek(); keep_first.set(!v); },
                    {i18n.t("console.merge.swap")}
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
                button {
                    class: "ik-btn primary",
                    disabled: *busy.read(),
                    title: "{keep_title}",
                    onclick: merge,
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

/// One side of a candidate pair, as the row header summarises it.
#[derive(Clone, PartialEq)]
struct SideSummary {
    id: SeriesId,
    title: String,
    sources: i64,
    chapters: i64,
}

impl SideSummary {
    /// "3 sources · 412 chapters" — the two numbers that decide which side should survive.
    fn summary(&self, i18n: crate::i18n::Translator) -> String {
        let sources = i18n.plural("series.sources", self.sources, &[]);
        let chapters = i18n.args("series.chapterCount", &[("count", &self.chapters.to_string())]);
        format!("{sources} · {chapters}")
    }
}

/// Compact read-only "manga info" card for a single series, used by the merge compare view
/// and the Sync inspector. Fetches the public series detail so operators can eyeball cover,
/// type/status, sources and tags before acting.
#[component]
pub(super) fn SeriesMiniCard(series_id: SeriesId) -> Element {
    let api = api::use_api();
    let i18n = use_i18n();
    let reload = use_reload();
    let res = use_resource(move || {
        reload.track();
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

    async_block(&res, reload, 120, |d| {
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
    })
}
