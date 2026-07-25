//! The per-series sync inspector: find a series, then add, correct or clear its external
//! mapping for each known provider.

use crate::api;
use crate::components::ErrorBox;
use crate::hooks::Reload;
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
    let mut query = use_signal(String::new);

    // All hooks are declared unconditionally (Rules of Hooks) before we branch on whether a
    // series is currently open in the editor.
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
                    .map_err(api::friendly_error)
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
                    .map_err(api::friendly_error)
            }
        })
    };

    if let Some(sid) = selected.read().clone() {
        return rsx! {
            SeriesSyncEditor { key: "{sid}", series_id: sid, selected, reload }
        };
    }

    let results_body = match &*results.read_unchecked() {
        Some(Ok(list)) if !list.is_empty() => {
            let list = list.clone();
            rsx! {
                div { style: "margin-top:8px;",
                    for s in list {
                        SeriesPickRow { key: "{s.id}", series: s, selected }
                    }
                }
            }
        }
        Some(Err(e)) => rsx! {
            div { class: "ik-empty", style: "font-size:12px;", "Search failed: {e}" }
        },
        _ => rsx! {},
    };

    let mappings_body = match &*mappings.read_unchecked() {
        None => rsx! { div { class: "ik-skeleton", style: "height:40px;" } },
        Some(Err(e)) => {
            let msg = e.clone();
            rsx! { ErrorBox { message: msg, on_retry: move |()| reload.bump() } }
        }
        Some(Ok(list)) if list.is_empty() => rsx! {
            div { class: "ik-empty", "No series↔external mappings yet." }
        },
        Some(Ok(list)) => {
            let list = list.clone();
            rsx! {
                for m in list {
                    MappingPickRow { key: "{m.series_id}-{m.provider}", mapping: Signal::new(m), selected }
                }
            }
        }
    };

    rsx! {
        input {
            class: "ik-input",
            style: "width:100%;",
            r#type: "text",
            placeholder: "Search a series by title to open its info…",
            value: "{query}",
            oninput: move |e| query.set(e.value()),
        }
        {results_body}
        div { class: "ik-muted", style: "font-size:12px;margin:14px 0 6px;", "Recently mapped" }
        {mappings_body}
    }
}

/// A search-result row that opens the series in the inspector.
#[component]
pub(super) fn SeriesPickRow(series: SeriesSummary, selected: Signal<Option<String>>) -> Element {
    let sid = series.id;
    rsx! {
        div { class: "ik-row",
            div { class: "grow",
                span { style: "font-weight:600;", "{series.title}" }
                div { class: "ik-muted", style: "font-size:12px;", "{series.source_count} sources" }
            }
            button { class: "ik-btn", onclick: move |_| selected.set(Some(sid.to_string())), "Open" }
        }
    }
}

/// A recently-mapped row that opens the series in the inspector.
#[component]
pub(super) fn MappingPickRow(
    mapping: Signal<AdminSyncMapping>,
    selected: Signal<Option<String>>,
) -> Element {
    let m = mapping.read();
    let updated = rel_time(Some(&m.updated_at));
    let sid = m.series_id;
    rsx! {
        div { class: "ik-row",
            div { class: "grow",
                div { class: "ik-flex", style: "justify-content:space-between;",
                    span { style: "font-weight:600;", "{m.series_title}" }
                    span { class: "ik-pill", "{m.provider}" }
                }
                div { class: "ik-mono ik-muted", style: "font-size:12px;",
                    "id {m.external_id} · updated {updated}"
                }
            }
            button { class: "ik-btn", onclick: move |_| selected.set(Some(sid.to_string())), "Open" }
        }
    }
}

/// The editable per-series "manga info" view: the series card plus one editor row per known
/// sync provider (prefilled with its current external id), so an operator can add, correct
/// or clear a mapping by hand.
#[component]
pub(super) fn SeriesSyncEditor(
    series_id: String,
    selected: Signal<Option<String>>,
    reload: Reload,
) -> Element {
    let api = api::use_api();
    // `selected` (and therefore this component's `series_id` prop) is a plain `String` shared
    // with the search/pick-row flow above; parse it once here at the boundary.
    let Ok(sid) = series_id.parse::<SeriesId>() else {
        return rsx! { div { class: "ik-empty", "That series id doesn't look right." } };
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
                    .map_err(api::friendly_error)
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
                .map_err(api::friendly_error)
        }
    });

    let map_list: Vec<AdminSyncMapping> = match &*mappings.read_unchecked() {
        Some(Ok(l)) => l.clone(),
        _ => Vec::new(),
    };
    let prov_list: Vec<ProviderInfo> = match &*providers.read_unchecked() {
        Some(Ok(l)) => l.clone(),
        _ => Vec::new(),
    };

    rsx! {
        div { class: "ik-flex", style: "justify-content:space-between;align-items:center;margin-bottom:10px;",
            button { class: "ik-btn", onclick: move |_| selected.set(None), "← Back to search" }
            button { class: "ik-btn", onclick: move |_| reload.bump(), "Refresh" }
        }
        SeriesMiniCard { series_id: sid }
        div { class: "ik-muted", style: "font-size:12px;margin:14px 0 6px;", "External sync mappings" }
        if prov_list.is_empty() && map_list.is_empty() {
            div { class: "ik-empty", "No sync providers registered." }
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
                placeholder: "external id (e.g. AniList media id)",
                value: "{value}",
                oninput: move |e| value.set(e.value()),
            }
            button { class: "ik-btn primary", disabled: *busy.read(), onclick: save, "Save" }
            if has_current {
                button { class: "ik-btn", disabled: *busy.read(), onclick: clear, "Clear" }
            }
        }
    }
}
