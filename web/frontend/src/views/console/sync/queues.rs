//! The two matching backlogs: local series with no mapping, and remote entries that
//! matched nothing locally.

use crate::api;
use crate::components::{SkeletonBlock, EmptyBox, ErrorBox};
use crate::hooks::Reload;
use crate::i18n::{use_i18n, Translator};
use crate::models::*;
use crate::state::use_session;
use crate::views::console::merge::SeriesMiniCard;
use dioxus::prelude::*;
use progenitor_client::ResponseValue;

/// The assign queue: pick a provider, optionally filter by title, and hand-assign an
/// external id to any series the automatic matcher left unmapped (or open it in the
/// inspector).
#[component]
pub(super) fn AssignQueue(selected: Signal<Option<String>>, reload: Reload) -> Element {
    let api = api::use_api();
    let i18n = use_i18n();
    let mut provider = use_signal(|| "anilist".to_string());
    let mut query = use_signal(String::new);

    let providers = use_resource(move || {
        let client = api.client();
        async move {
            client
                .sync_providers()
                .send()
                .await
                .map(ResponseValue::into_inner)
                .map_err(|e| api::friendly_error(i18n, e))
        }
    });

    let list = {
        use_resource(move || {
            let p = provider.read().clone();
            let q = query.read().clone();
            reload.track();
            let client = api.client();
            async move {
                let mut builder = client.list_unmapped_series().provider(p);
                if !q.trim().is_empty() {
                    builder = builder.query(q);
                }
                builder
                    .send()
                    .await
                    .map(ResponseValue::into_inner)
                    .map_err(|e| api::friendly_error(i18n, e))
            }
        })
    };

    let prov_list: Vec<ProviderInfo> = match &*providers.read_unchecked() {
        Some(Ok(l)) => l.clone(),
        _ => Vec::new(),
    };

    let body = match &*list.read_unchecked() {
        None => rsx! { SkeletonBlock { height: 60 } },
        Some(Err(e)) => {
            let msg = e.clone();
            rsx! { ErrorBox { message: msg, on_retry: move |()| reload.bump() } }
        }
        Some(Ok(l)) if l.is_empty() => rsx! {
            EmptyBox { message: i18n.t("console.sync.assignEmpty") }
        },
        Some(Ok(l)) => {
            let l = l.clone();
            let prov = provider.read().clone();
            rsx! {
                for s in l {
                    AssignRow { key: "{s.series_id}", series: Signal::new(s), provider: prov.clone(), selected, reload }
                }
            }
        }
    };

    rsx! {
        div { class: "ik-flex", style: "gap:8px;margin-bottom:10px;flex-wrap:wrap;",
            select {
                class: "ik-input",
                value: "{provider}",
                onchange: move |e| provider.set(e.value()),
                if prov_list.is_empty() {
                    option { value: "anilist", "anilist" }
                }
                for p in prov_list.clone() {
                    option { value: "{p.slug}", "{p.name}" }
                }
            }
            input {
                class: "ik-input",
                style: "flex:1;",
                r#type: "text",
                placeholder: i18n.t("console.sync.filterUnmapped"),
                value: "{query}",
                oninput: move |e| query.set(e.value()),
            }
        }
        {body}
    }
}

/// One assign-queue row: title + source count, an external-id input to Assign, and Inspect
/// to open the full editor.
#[component]
pub(super) fn AssignRow(
    series: Signal<UnmappedSeries>,
    provider: String,
    selected: Signal<Option<String>>,
    reload: Reload,
) -> Element {
    let api = api::use_api();
    let i18n = use_i18n();
    let session = use_session();
    let mut value = use_signal(String::new);
    let mut busy = use_signal(|| false);
    let s = series.read();

    let assign = {
        let series_id = SeriesId(s.series_id);
        let provider = provider.clone();
        let _client = api.client();
        move |_| {
            if *busy.peek() {
                return;
            }
            let ext = value.peek().trim().to_string();
            if ext.is_empty() {
                return;
            }
            busy.set(true);
            let provider = provider.clone();
            spawn(async move {
                let client = api.client();
                if session.token_value().is_some()
                    && client
                        .upsert_sync_mapping()
                        .body(UpsertMapping {
                            series_id,
                            provider: provider.clone(),
                            external_id: ext.clone(),
                        })
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

    let sid = s.series_id;
    rsx! {
        div { class: "ik-row",
            div { class: "grow",
                div { style: "font-weight:600;", "{s.series_title}" }
                div { class: "ik-muted", style: "font-size:12px;",
                    {
                        let sources = i18n.plural("series.sources", s.source_count, &[]);
                        format!("{sources} · {provider}")
                    }
                }
            }
            input {
                class: "ik-input ik-mono",
                style: "width:200px;",
                r#type: "text",
                placeholder: i18n.t("console.sync.externalId"),
                value: "{value}",
                oninput: move |e| value.set(e.value()),
            }
            button { class: "ik-btn primary", disabled: *busy.read(), onclick: assign,
                {i18n.t("console.sync.assign")}
            }
            button { class: "ik-btn", onclick: move |_| selected.set(Some(sid.to_string())),
                {i18n.t("console.sync.inspect")}
            }
        }
    }
}

/// The reverse assign queue: pick a provider, optionally filter, and match every fetched
/// remote entry the auto-matcher could not confidently link to a local series.
#[component]
pub(super) fn UnmatchedRemoteQueue(reload: Reload) -> Element {
    let api = api::use_api();
    let i18n = use_i18n();
    let mut provider = use_signal(|| "anilist".to_string());
    let mut query = use_signal(String::new);

    let providers = use_resource(move || {
        let client = api.client();
        async move {
            client
                .sync_providers()
                .send()
                .await
                .map(ResponseValue::into_inner)
                .map_err(|e| api::friendly_error(i18n, e))
        }
    });

    let list = {
        use_resource(move || {
            let p = provider.read().clone();
            let q = query.read().clone();
            reload.track();
            let client = api.client();
            async move {
                let mut builder = client.list_unmatched_remote().provider(p);
                if !q.trim().is_empty() {
                    builder = builder.query(q);
                }
                builder
                    .send()
                    .await
                    .map(ResponseValue::into_inner)
                    .map_err(|e| api::friendly_error(i18n, e))
            }
        })
    };

    let prov_list: Vec<ProviderInfo> = match &*providers.read_unchecked() {
        Some(Ok(l)) => l.clone(),
        _ => Vec::new(),
    };

    let body = match &*list.read_unchecked() {
        None => rsx! { SkeletonBlock { height: 60 } },
        Some(Err(e)) => {
            let msg = e.clone();
            rsx! { ErrorBox { message: msg, on_retry: move |()| reload.bump() } }
        }
        Some(Ok(l)) if l.is_empty() => rsx! {
            EmptyBox { message: i18n.t("console.sync.remoteEmpty") }
        },
        Some(Ok(l)) => {
            let l = l.clone();
            rsx! {
                for e in l {
                    UnmatchedRemoteRow { key: "{e.user_id}-{e.external_id}", entry: Signal::new(e), reload }
                }
            }
        }
    };

    rsx! {
        div { class: "ik-flex", style: "gap:8px;margin-bottom:10px;flex-wrap:wrap;",
            select {
                class: "ik-input",
                value: "{provider}",
                onchange: move |e| provider.set(e.value()),
                if prov_list.is_empty() {
                    option { value: "anilist", "anilist" }
                }
                for p in prov_list.clone() {
                    option { value: "{p.slug}", "{p.name}" }
                }
            }
            input {
                class: "ik-input",
                style: "flex:1;",
                r#type: "text",
                placeholder: i18n.t("console.sync.filterUnmatched"),
                value: "{query}",
                oninput: move |e| query.set(e.value()),
            }
        }
        {body}
    }
}

/// The canonical web URL for a fetched remote entry, so an operator can open the original
/// listing on the provider's site to compare it against local candidates. Only providers with
/// a known URL scheme return `Some`; `external_id` is the provider's media id.
pub(super) fn provider_entry_url(provider: &str, external_id: &str) -> Option<String> {
    match provider {
        // AniList media ids resolve under /manga/ for every reading medium (manga/manhwa/
        // manhua/novel all live there), so a single scheme covers the tracker's content.
        "anilist" => Some(format!("https://anilist.co/manga/{external_id}")),
        _ => None,
    }
}

/// One reverse-queue row: shows the remote entry (with a link to open it on the provider),
/// automatic ranked match suggestions, and a manual search fallback. Every candidate can be
/// inspected in place (a full "manga info" card) before matching.
#[component]
pub(super) fn UnmatchedRemoteRow(entry: Signal<UnmatchedRemoteEntry>, reload: Reload) -> Element {
    let api = api::use_api();
    let i18n = use_i18n();
    let mut search = use_signal(String::new);
    let en = entry.read();

    // Automatic suggestions from the server-side matcher, loaded once for this entry.
    let entry_title = en.title.clone();
    let entry_ct = en.content_type.clone();
    let entry_year = en.start_year;
    let suggestions = {
        use_resource(move || {
            let title = entry_title.clone();
            let ct = entry_ct.clone();
            let year = entry_year;
            let client = api.client();
            async move {
                let mut builder = client.list_suggestions().title(title);
                if !ct.is_empty() {
                    builder = builder.content_type(ct);
                }
                if let Some(y) = year {
                    builder = builder.start_year(y);
                }
                builder
                    .send()
                    .await
                    .map(ResponseValue::into_inner)
                    .map_err(|e| api::friendly_error(i18n, e))
            }
        })
    };

    // Manual search fallback for the cases the matcher misses entirely.
    let results = {
        use_resource(move || {
            let q = search.read().clone();
            let client = api.client();
            async move {
                let q = q.trim().to_string();
                if q.len() < 3 {
                    return Ok(Vec::new());
                }
                client
                    .list()
                    .query(q)
                    .limit(8)
                    .send()
                    .await
                    .map(ResponseValue::into_inner)
                    .map_err(|e| api::friendly_error(i18n, e))
            }
        })
    };

    let suggested: Vec<SuggestedMatch> = match &*suggestions.read_unchecked() {
        Some(Ok(l)) => l.clone(),
        _ => Vec::new(),
    };
    let manual: Vec<SeriesSummary> = match &*results.read_unchecked() {
        Some(Ok(l)) => l.clone(),
        _ => Vec::new(),
    };

    let type_line = {
        let mut parts = vec![en.status.clone()];
        if !en.content_type.is_empty() && en.content_type != "unknown" {
            parts.push(en.content_type.clone());
        }
        if let Some(y) = en.start_year {
            parts.push(y.to_string());
        }
        parts.push(format!("#{}", en.external_id));
        parts.join(" · ")
    };

    let entry_url = provider_entry_url(&en.provider, &en.external_id);
    let suggestions_pending = (*suggestions.read_unchecked()).is_none();

    rsx! {
        div { class: "ik-row", style: "flex-direction:column;align-items:stretch;gap:8px;",
            div { class: "ik-flex", style: "justify-content:space-between;gap:8px;align-items:flex-start;",
                div { style: "min-width:0;",
                    div { style: "font-weight:600;", "{en.title}" }
                    div { class: "ik-muted", style: "font-size:12px;", "{en.username} · {type_line}" }
                }
                if let Some(url) = entry_url {
                    a {
                        class: "ik-btn",
                        style: "flex:0 0 auto;",
                        href: "{url}",
                        target: "_blank",
                        rel: "noopener noreferrer",
                        {i18n.args("console.sync.openOn", &[("provider", &en.provider)])}
                    }
                }
            }

            div { class: "ik-muted", style: "font-size:11px;text-transform:uppercase;letter-spacing:.04em;",
                {i18n.t("console.sync.suggested")}
            }
            if suggestions_pending {
                SkeletonBlock { height: 40 }
            } else if suggested.is_empty() {
                div { class: "ik-muted", style: "font-size:12px;",
                    {i18n.t("console.sync.noSuggestions")}
                }
            } else {
                div { class: "ik-flex", style: "flex-direction:column;gap:6px;",
                    for s in suggested {
                        CandidateMatchRow {
                            key: "sug-{s.series_id}",
                            series_id: SeriesId(s.series_id),
                            title: s.title.clone(),
                            meta: suggestion_meta(i18n, &s),
                            score: Some(s.score),
                            user_id: UserId(en.user_id),
                            provider: en.provider.clone(),
                            external_id: en.external_id.clone(),
                            reload,
                        }
                    }
                }
            }

            input {
                class: "ik-input",
                r#type: "text",
                placeholder: i18n.t("console.sync.manualSearch"),
                value: "{search}",
                oninput: move |e| search.set(e.value()),
            }
            if !manual.is_empty() {
                div { class: "ik-flex", style: "flex-direction:column;gap:6px;",
                    for c in manual {
                        CandidateMatchRow {
                            key: "man-{c.id}",
                            series_id: c.id,
                            title: c.title.clone(),
                            meta: {
                                let kind = i18n.t(c.content_type.label_key());
                                let sources = i18n.args(
                                    "series.sourceCount",
                                    &[("count", &c.source_count.to_string())],
                                );
                                format!("{kind} · {sources}")
                            },
                            score: None,
                            user_id: UserId(en.user_id),
                            provider: en.provider.clone(),
                            external_id: en.external_id.clone(),
                            reload,
                        }
                    }
                }
            }
        }
    }
}

/// A short one-line descriptor for a suggested series (type · year · sources).
///
/// The content type arrives as the matcher's raw token rather than a typed enum, so it is
/// passed through as-is; the source count is worded from the catalogue.
pub(super) fn suggestion_meta(i18n: Translator, s: &SuggestedMatch) -> String {
    let mut parts = Vec::new();
    if !s.content_type.is_empty() && s.content_type != "unknown" {
        parts.push(s.content_type.clone());
    }
    if let Some(y) = s.release_year {
        parts.push(y.to_string());
    }
    parts.push(i18n.args(
        "series.sourceCount",
        &[("count", &s.source_count.to_string())],
    ));
    parts.join(" · ")
}

/// One matchable candidate (from suggestions or manual search): shows the series, an optional
/// confidence score, an "Inspect" toggle that expands the full series info card so the entries
/// behind the suggested id can actually be reviewed, and a "Match" button that assigns it.
#[component]
pub(super) fn CandidateMatchRow(
    series_id: SeriesId,
    title: String,
    meta: String,
    score: Option<f32>,
    user_id: UserId,
    provider: String,
    external_id: String,
    reload: Reload,
) -> Element {
    let api = api::use_api();
    let i18n = use_i18n();
    let mut busy = use_signal(|| false);
    let mut show = use_signal(|| false);

    let match_it = {
        let provider = provider.clone();
        let external_id = external_id.clone();
        move |_| {
            if *busy.peek() {
                return;
            }
            busy.set(true);
            let provider = provider.clone();
            let external_id = external_id.clone();
            let _client = api.client();
            spawn(async move {
                let client = api.client();
                let body = AssignRemoteEntry {
                    user_id,
                    provider: provider.clone(),
                    series_id,
                    external_id: external_id.clone(),
                };
                if client.assign_remote_entry().body(body).send().await.is_ok() {
                    reload.bump();
                }
                busy.set(false);
            });
        }
    };

    let score_badge = score.map(|s| {
        // Clamped to 0..=1 first, so the rounded percentage is always in `i32` range.
        #[allow(clippy::cast_possible_truncation)]
        let pct = (s.clamp(0.0, 1.0) * 100.0).round() as i32;
        let cls = if s >= 0.85 {
            "ik-pill jade"
        } else if s >= 0.6 {
            "ik-pill"
        } else {
            "ik-pill vermilion"
        };
        (cls, pct)
    });

    let sid_for_card = series_id;
    let show_now = *show.read();

    rsx! {
        div { class: "ik-card", style: "padding:8px;",
            div { class: "ik-flex", style: "justify-content:space-between;gap:8px;align-items:center;",
                div { style: "min-width:0;",
                    div { style: "font-weight:600;", "{title}" }
                    div { class: "ik-muted", style: "font-size:11px;", "{meta}" }
                }
                div { class: "ik-flex", style: "gap:4px;align-items:center;flex:0 0 auto;",
                    if let Some((cls, pct)) = score_badge {
                        span { class: "{cls}", style: "font-size:11px;", "{pct}%" }
                    }
                    button {
                        class: "ik-btn",
                        onclick: move |_| show.set(!show_now),
                        if show_now {
                            {i18n.t("console.merge.hide")}
                        } else {
                            {i18n.t("console.sync.inspect")}
                        }
                    }
                    button {
                        class: "ik-btn primary",
                        disabled: *busy.read(),
                        onclick: match_it,
                        {i18n.t("console.sync.match")}
                    }
                }
            }
            if show_now {
                div { style: "margin-top:8px;",
                    SeriesMiniCard { series_id: sid_for_card }
                }
            }
        }
    }
}
