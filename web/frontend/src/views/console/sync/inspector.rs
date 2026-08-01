//! The per-series sync inspector: find a series, then add, correct or clear its external
//! mapping for each known provider.

use crate::api;
use crate::components::{async_block_list, EmptyBox, ErrorBox};
use crate::hooks::Reload;
use crate::i18n::use_i18n;
use crate::models::*;
use crate::state::use_session;
use crate::util::rel_time;
use crate::views::console::merge::SeriesMiniCard;
use dioxus::prelude::*;
use progenitor_client::ResponseValue;

/// Either the editable per-series "manga info" view (when a series is selected) or a title
/// search + recently-mapped list to open one.
#[component]
pub(super) fn SeriesSyncInspector(selected: Signal<Option<String>>, reload: Reload) -> Element {
    let api = api::use_api();
    let i18n = use_i18n();
    let mut query = use_signal(String::new);

    // Hooks stay unconditional (Rules of Hooks) ahead of the branch on `selected`.
    let results = {
        use_resource(move || {
            let q = query.read().clone();
            let client = api.client();
            async move {
                if q.trim().len() < 2 {
                    return Ok(Vec::new());
                }
                client
                    .list()
                    .query(q)
                    .limit(12)
                    .send()
                    .await
                    .map(ResponseValue::into_inner)
                    .map_err(|e| api::friendly_error(i18n, e))
            }
        })
    };

    let mappings = {
        use_resource(move || {
            reload.track();
            let client = api.client();
            async move {
                client
                    .list_sync_mappings()
                    .send()
                    .await
                    .map(ResponseValue::into_inner)
                    .map_err(|e| api::friendly_error(i18n, e))
            }
        })
    };

    if let Some(sid) = selected.read().clone() {
        return rsx! {
            SeriesSyncEditor { key: "{sid}", series_id: sid, selected, reload }
        };
    }

    // Cannot use `async_block_list`: below 2 characters typed, results stay `None`, and a
    // skeleton/empty box under an untouched field would read as a broken screen.
    let results_body = match &*results.read() {
        None => rsx! {},
        Some(Err(message)) => {
            let message = message.clone();
            // Retrying means re-running the query, and the resource is keyed on `query` — so
            // writing the same value back is the retry.
            rsx! {
                ErrorBox {
                    message,
                    on_retry: move |()| {
                        let current = query.peek().clone();
                        query.set(current);
                    },
                }
            }
        }
        Some(Ok(list)) if list.is_empty() => rsx! {},
        Some(Ok(list)) => {
            let list = list.clone();
            rsx! {
                div { style: "margin-top:8px;",
                    for s in list {
                        SeriesPickRow { key: "{s.id}", series: s, selected }
                    }
                }
            }
        }
    };

    let no_mappings = i18n.t("console.sync.noMappings");
    let mappings_body = async_block_list(&mappings, reload, 40, &no_mappings, |rows| {
        let rows = rows.to_vec();
        rsx! {
            for m in rows {
                MappingPickRow {
                    key: "{m.series_id}-{m.provider}",
                    mapping: Signal::new(m),
                    selected,
                }
            }
        }
    });

    rsx! {
        input {
            class: "ik-input",
            style: "width:100%;",
            r#type: "text",
            placeholder: i18n.t("console.sync.searchSeries"),
            value: "{query}",
            oninput: move |e| query.set(e.value()),
        }
        {results_body}
        div { class: "ik-muted", style: "font-size:12px;margin:14px 0 6px;",
            {i18n.t("console.sync.recentlyMapped")}
        }
        {mappings_body}
    }
}

/// A search-result row that opens the series in the inspector.
#[component]
pub(super) fn SeriesPickRow(series: SeriesSummary, selected: Signal<Option<String>>) -> Element {
    let i18n = use_i18n();
    let sid = series.id;
    rsx! {
        div { class: "ik-row",
            div { class: "grow",
                span { style: "font-weight:600;", "{series.title}" }
                div { class: "ik-muted", style: "font-size:12px;",
                    {i18n.plural("series.sources", series.source_count, &[])}
                }
            }
            button { class: "ik-btn", onclick: move |_| selected.set(Some(sid.to_string())),
                {i18n.t("common.open")}
            }
        }
    }
}

/// A recently-mapped row that opens the series in the inspector.
#[component]
pub(super) fn MappingPickRow(
    mapping: Signal<AdminSyncMapping>,
    selected: Signal<Option<String>>,
) -> Element {
    let i18n = use_i18n();
    let m = mapping.read();
    let updated = rel_time(i18n, Some(&m.updated_at));
    let sid = m.series_id;
    rsx! {
        div { class: "ik-row",
            div { class: "grow",
                div { class: "ik-flex", style: "justify-content:space-between;",
                    span { style: "font-weight:600;", "{m.series_title}" }
                    span { class: "ik-pill", "{m.provider}" }
                }
                div { class: "ik-mono ik-muted", style: "font-size:12px;",
                    {
                        i18n.args(
                            "console.sync.mappingMeta",
                            &[("id", &m.external_id), ("when", &updated)],
                        )
                    }
                }
            }
            button { class: "ik-btn", onclick: move |_| selected.set(Some(sid.to_string())),
                {i18n.t("common.open")}
            }
        }
    }
}

/// The editable per-series "manga info" view: the series card plus one editor row per known
/// sync provider, prefilled with its current external id.
#[component]
pub(super) fn SeriesSyncEditor(
    series_id: String,
    selected: Signal<Option<String>>,
    reload: Reload,
) -> Element {
    let api = api::use_api();
    let i18n = use_i18n();
    // `series_id` arrives as a plain `String` shared with the search/pick-row flow; parsed
    // once here at the boundary.
    let Ok(sid) = series_id.parse::<SeriesId>() else {
        return rsx! {
            EmptyBox { message: i18n.t("console.sync.badSeriesId") }
        };
    };

    let mappings = {
        use_resource(move || {
            reload.track();
            let client = api.client();
            async move {
                client
                    .list_sync_mappings_for_series()
                    .id(sid)
                    .send()
                    .await
                    .map(ResponseValue::into_inner)
                    .map_err(|e| api::friendly_error(i18n, e))
            }
        })
    };

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

    // Both feed one editor grid whose own empty state covers the not-yet-loaded case, so
    // neither gets its own loading or error chrome.
    let map_list: Vec<AdminSyncMapping> = match &*mappings.read() {
        Some(Ok(l)) => l.clone(),
        _ => Vec::new(),
    };
    let prov_list: Vec<ProviderInfo> = match &*providers.read() {
        Some(Ok(l)) => l.clone(),
        _ => Vec::new(),
    };

    rsx! {
        div { class: "ik-flex", style: "justify-content:space-between;align-items:center;margin-bottom:10px;",
            button { class: "ik-btn", onclick: move |_| selected.set(None),
                {i18n.t("console.sync.backToSearch")}
            }
            button { class: "ik-btn", onclick: move |_| reload.bump(),
                {i18n.t("console.live.refresh")}
            }
        }
        SeriesMiniCard { series_id: sid }
        div { class: "ik-muted", style: "font-size:12px;margin:14px 0 6px;",
            {i18n.t("console.sync.externalMappings")}
        }
        if prov_list.is_empty() && map_list.is_empty() {
            EmptyBox { message: i18n.t("console.sync.noProvidersRegistered") }
        }
        for p in prov_list.clone() {
            {
                let current = map_list
                    .iter()
                    .find(|m| m.provider == p.slug)
                    .map(|m| m.external_id.clone());
                rsx! {
                    MappingEditorRow {
                        key: "{p.slug}",
                        series_id: sid,
                        provider: p.slug.clone(),
                        provider_name: p.name.clone(),
                        current,
                        reload,
                    }
                }
            }
        }
        for m in map_list.clone() {
            if !prov_list.iter().any(|p| p.slug == m.provider) {
                MappingEditorRow {
                    key: "orphan-{m.provider}",
                    series_id: sid,
                    provider: m.provider.clone(),
                    provider_name: m.provider.clone(),
                    current: Some(m.external_id.clone()),
                    reload,
                }
            }
        }
    }
}

/// One provider's mapping editor: an input prefilled with the current external id, plus
/// Save (upsert) and, when a mapping exists, Clear (delete).
#[component]
pub(super) fn MappingEditorRow(
    series_id: SeriesId,
    provider: String,
    provider_name: String,
    current: Option<String>,
    reload: Reload,
) -> Element {
    let api = api::use_api();
    let i18n = use_i18n();
    let session = use_session();
    let has_current = current.is_some();
    let mut value = use_signal(|| current.clone().unwrap_or_default());
    let mut busy = use_signal(|| false);
    let pill_class = if has_current {
        "ik-pill jade"
    } else {
        "ik-pill"
    };

    let save = {
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

    let clear = {
        let provider = provider.clone();
        let _client = api.client();
        move |_| {
            if *busy.peek() {
                return;
            }
            busy.set(true);
            let provider = provider.clone();
            spawn(async move {
                let client = api.client();
                let body = tankovault_api_client::types::SyncMappingTarget {
                    provider,
                    series_id,
                };
                if session.token_value().is_some()
                    && client.clear_sync_mapping().body(body).send().await.is_ok()
                {
                    reload.bump();
                }
                busy.set(false);
            });
        }
    };

    rsx! {
        div { class: "ik-row",
            div { style: "min-width:120px;",
                span { class: "{pill_class}", "{provider_name}" }
            }
            input {
                class: "ik-input ik-mono",
                style: "flex:1;",
                r#type: "text",
                placeholder: i18n.t("console.sync.externalIdHint"),
                value: "{value}",
                oninput: move |e| value.set(e.value()),
            }
            button { class: "ik-btn primary", disabled: *busy.read(), onclick: save,
                {i18n.t("common.save")}
            }
            if has_current {
                button { class: "ik-btn", disabled: *busy.read(), onclick: clear,
                    {i18n.t("console.sync.clear")}
                }
            }
        }
    }
}
