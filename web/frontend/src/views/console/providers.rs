//! Provider lifecycle: health tiles, the admin-only create form, and a full editor card
//! per provider (edit, state toggle, scan, adapter dry-run, delete).

use crate::api;
use crate::components::ErrorBox;
use crate::hooks::{use_reload, Reload};
use crate::models::*;
use crate::state::use_session;
use crate::views::console::adapter_token;
use crate::views::console::config_editor_text;
use crate::views::console::politeness_json;
use crate::views::console::ADAPTER_KINDS;
use dioxus::prelude::*;
use progenitor_client::ResponseValue;

/// Provider management: health tiles, an admin-only create form, and a full editor card
/// per provider (edit, state toggle, scan, adapter test, delete).
#[component]
pub(super) fn ProvidersPanel() -> Element {
    let api = api::use_api();
    let session = use_session();
    let is_admin = session.role.read().is_admin();
    let reload = use_reload();
    let resource = use_resource(move || {
        reload.track();
        let client = api.client();
        async move {
            if session.is_authenticated() {
                client
                    .list_providers()
                    .send()
                    .await
                    .map(ResponseValue::into_inner)
                    .map_err(api::friendly_error)
            } else {
                Ok(Vec::new())
            }
        }
    });

    let body = match &*resource.read_unchecked() {
        None => rsx! { div { class: "ik-skeleton", style: "height:80px;" } },
        Some(Err(e)) => {
            let msg = e.clone();
            rsx! {
                ErrorBox { message: msg, on_retry: move |()| reload.bump() }
            }
        }
        Some(Ok(list)) => {
            let tiles = list.clone();
            let cards = list.clone();
            rsx! {
                if tiles.is_empty() {
                    div { class: "ik-empty", "No providers yet. Add one below." }
                } else {
                    div { class: "ik-tiles",
                        for p in tiles {
                            div { class: "ik-tile",
                                div { style: "font-weight:600;", "{p.name}" }
                                HealthPill { state: provider_state_token(p.state).to_owned() }
                            }
                        }
                    }
                }
                for p in cards {
                    ProviderCard { key: "{p.id}", provider: Signal::new(p), reload }
                }
            }
        }
    };

    rsx! {
        section { style: "margin-bottom:18px;",
            h3 { "Providers" }
            if is_admin {
                CreateProviderForm { reload }
            }
            {body}
        }
    }
}

#[component]
pub(super) fn HealthPill(state: String) -> Element {
    let class = match state.as_str() {
        "active" => "ik-pill jade",
        "blocked" | "disabled" => "ik-pill vermilion",
        _ => "ik-pill",
    };
    let label = if state.is_empty() {
        "unknown".to_owned()
    } else {
        state
    };
    rsx! {
        span { class: "{class}", "{label}" }
    }
}

/// The wire token for a loaded provider's health state (matches the SQL enum / `HealthPill`).
pub(super) fn provider_state_token(s: ProviderState) -> &'static str {
    match s {
        ProviderState::Active => "active",
        ProviderState::Degraded => "degraded",
        ProviderState::Challenged => "challenged",
        ProviderState::Solving => "solving",
        ProviderState::Blocked => "blocked",
        ProviderState::Disabled => "disabled",
    }
}

/// Admin-only create form. Politeness is left at the polite server defaults on creation and
/// can be tuned immediately afterwards from the provider's editor card.
#[component]
pub(super) fn CreateProviderForm(reload: Reload) -> Element {
    let api = api::use_api();
    let mut open = use_signal(|| false);
    let mut slug = use_signal(String::new);
    let mut name = use_signal(String::new);
    let mut base_url = use_signal(String::new);
    let mut adapter = use_signal(|| "generic_config".to_owned());
    let mut config = use_signal(|| "{}".to_owned());
    let mut busy = use_signal(|| false);
    let mut msg = use_signal(|| Option::<String>::None);

    let submit = {
        move |_| {
            let cfg = match serde_json::from_str::<serde_json::Value>(&config.read()) {
                Ok(v) => v,
                Err(e) => {
                    msg.set(Some(format!("Config is not valid JSON: {e}")));
                    return;
                }
            };
            let (s, n, b, a_str) = (
                slug.read().trim().to_owned(),
                name.read().trim().to_owned(),
                base_url.read().trim().to_owned(),
                adapter.read().clone(),
            );
            if s.is_empty() || n.is_empty() || b.is_empty() {
                msg.set(Some("Slug, name and base URL are all required.".to_owned()));
                return;
            }
            busy.set(true);
            msg.set(None);
            let _client = api.client();
            spawn(async move {
                let client = api.client();
                let a = match a_str.as_str() {
                    "madara" => AdapterKind::Madara,
                    "generic_config" => AdapterKind::GenericConfig,
                    _ => AdapterKind::Custom,
                };
                let body = CreateProvider {
                    slug: s,
                    name: n,
                    base_url: b,
                    adapter: a,
                    config: Some(cfg),
                    politeness: None,
                };
                let outcome = client
                    .create_provider()
                    .body(body)
                    .send()
                    .await
                    .map(ResponseValue::into_inner)
                    .map_err(api::friendly_error);
                match outcome {
                    Ok(_) => {
                        slug.set(String::new());
                        name.set(String::new());
                        base_url.set(String::new());
                        config.set("{}".to_owned());
                        busy.set(false);
                        open.set(false);
                        reload.bump();
                    }
                    Err(e) => {
                        msg.set(Some(e));
                        busy.set(false);
                    }
                }
            });
        }
    };

    rsx! {
        section { class: "ik-tile", style: "margin-bottom:14px;",
            div { class: "ik-flex", style: "justify-content:space-between;",
                h3 { style: "margin:0;", "Add provider" }
                button {
                    class: "ik-btn",
                    onclick: move |_| {
                        let o = *open.read();
                        open.set(!o);
                    },
                    if *open.read() { "Cancel" } else { "New provider" }
                }
            }
            if *open.read() {
                div { style: "margin-top:12px;display:grid;gap:10px;",
                    Field { label: "Slug (stable, unique)",
                        input {
                            class: "ik-input ik-mono",
                            placeholder: "acme-scans",
                            value: "{slug}",
                            oninput: move |e| slug.set(e.value()),
                        }
                    }
                    Field { label: "Display name",
                        input {
                            class: "ik-input",
                            placeholder: "Acme Scans",
                            value: "{name}",
                            oninput: move |e| name.set(e.value()),
                        }
                    }
                    Field { label: "Base URL",
                        input {
                            class: "ik-input ik-mono",
                            placeholder: "https://acmescans.example",
                            value: "{base_url}",
                            oninput: move |e| base_url.set(e.value()),
                        }
                    }
                    Field { label: "Adapter",
                        select {
                            class: "ik-input",
                            value: "{adapter}",
                            onchange: move |e| adapter.set(e.value()),
                            for (token , label) in ADAPTER_KINDS.iter().copied() {
                                option { value: "{token}", "{label}" }
                            }
                        }
                    }
                    Field { label: "Adapter config (JSON)",
                        textarea {
                            class: "ik-input ik-mono",
                            style: "min-height:120px;resize:vertical;",
                            value: "{config}",
                            oninput: move |e| config.set(e.value()),
                        }
                    }
                    div {
                        button {
                            class: "ik-btn primary",
                            disabled: *busy.read(),
                            onclick: submit,
                            "Create provider"
                        }
                    }
                    if let Some(m) = msg.read().clone() {
                        p { style: "margin:0;color:var(--vermilion);font-size:13px;", "{m}" }
                    }
                }
            }
        }
    }
}

/// A labelled form field wrapper for consistent spacing in the editor grids.
#[component]
pub(super) fn Field(label: String, children: Element) -> Element {
    rsx! {
        label { style: "display:grid;gap:4px;",
            span { class: "ik-muted", style: "font-size:12px;", "{label}" }
            {children}
        }
    }
}

/// Full editor card for one provider. The adapter kind is immutable (recreate to change
/// it); everything else — name, base URL, config, politeness, health state — is editable,
/// plus per-provider scan, a live adapter dry-run, and delete.
#[component]
pub(super) fn ProviderCard(provider: Signal<Provider>, reload: Reload) -> Element {
    let api = api::use_api();
    let session = use_session();
    let is_admin = session.role.read().is_admin();
    let pro = provider.read();

    let id = pro.id;
    let original_base = pro.base_url.clone();
    let is_disabled = pro.state == ProviderState::Disabled;

    let mut expanded = use_signal(|| false);
    let mut show_test = use_signal(|| false);
    let mut busy = use_signal(|| false);
    let mut msg = use_signal(|| Option::<String>::None);
    let mut confirm_migrate = use_signal(|| false);
    let mut confirm_delete = use_signal(|| false);
    let mut scan_mode = use_signal(|| ScanMode::Fast);

    let mut name = use_signal(|| pro.name.clone());
    let mut base_url = use_signal(|| pro.base_url.clone());
    let mut config = use_signal(|| config_editor_text(&pro.config));
    let mut rps = use_signal(|| pro.politeness.rps.unwrap_or(1.0).to_string());
    let mut concurrency = use_signal(|| pro.politeness.concurrency.unwrap_or(2).to_string());
    let mut crawl_delay_ms = use_signal(|| pro.politeness.crawl_delay_ms.unwrap_or(0).to_string());
    let mut user_agent = use_signal(|| pro.politeness.user_agent.clone().unwrap_or_default());

    let on_save_logic = {
        let original_base = original_base.clone();
        move || {
            if *base_url.read() != original_base && !*confirm_migrate.read() {
                confirm_migrate.set(true);
                return;
            }

            let cfg = match serde_json::from_str::<serde_json::Value>(&config.read()) {
                Ok(v) => v,
                Err(e) => {
                    msg.set(Some(format!("Config is not valid JSON: {e}")));
                    return;
                }
            };
            let pol = match politeness_json(
                &rps.read(),
                &concurrency.read(),
                &crawl_delay_ms.read(),
                &user_agent.read(),
            ) {
                Ok(v) => v,
                Err(e) => {
                    msg.set(Some(e));
                    return;
                }
            };
            let name_v = name.read().clone();
            let base_v = base_url.read().clone();
            confirm_migrate.set(false);
            busy.set(true);
            msg.set(None);
            let _client = api.client();
            spawn(async move {
                let client = api.client();
                let pol_dto = serde_json::from_value::<Politeness>(pol).ok();
                let outcome = client
                    .update_provider()
                    .id(id)
                    .body(UpdateProvider {
                        name: name_v,
                        base_url: base_v,
                        config: Some(cfg),
                        politeness: pol_dto,
                    })
                    .send()
                    .await
                    .map(ResponseValue::into_inner)
                    .map_err(api::friendly_error);
                match outcome {
                    Ok(_) => reload.bump(),
                    Err(e) => {
                        msg.set(Some(e));
                        busy.set(false);
                    }
                }
            });
        }
    };
    let on_save = {
        let mut logic = on_save_logic.clone();
        move |_| (logic)()
    };
    let on_confirm_migrate = {
        let mut logic = on_save_logic.clone();
        move |_| (logic)()
    };

    let toggle_state = {
        move |_| {
            let target = if is_disabled {
                ProviderState::Active
            } else {
                ProviderState::Disabled
            };
            busy.set(true);
            msg.set(None);
            let _client = api.client();
            spawn(async move {
                let client = api.client();
                if session.token_value().is_some() {
                    if let Err(e) = client
                        .set_provider_state()
                        .id(id)
                        .body(SetProviderStateBody { state: target })
                        .send()
                        .await
                        .map(|_| ())
                        .map_err(api::friendly_error)
                    {
                        msg.set(Some(e));
                        busy.set(false);
                        return;
                    }
                }
                reload.bump();
            });
        }
    };

    let scan = {
        move |_| {
            let m = *scan_mode.read();
            msg.set(None);
            let _client = api.client();
            spawn(async move {
                let client = api.client();
                let body = TriggerScan {
                    mode: m,
                    provider_id: Some(TriggerScanProviderId::Variant1(id)),
                };
                match client
                    .trigger_scan()
                    .body(body)
                    .send()
                    .await
                    .map(ResponseValue::into_inner)
                    .map_err(api::friendly_error)
                {
                    Ok(()) => msg.set(Some("Scan queued for this provider.".to_owned())),
                    Err(e) => msg.set(Some(e)),
                }
            });
        }
    };

    let delete = {
        move |_| {
            busy.set(true);
            let _client = api.client();
            spawn(async move {
                let client = api.client();
                let outcome = client
                    .delete_provider()
                    .id(id)
                    .send()
                    .await
                    .map(|_| ())
                    .map_err(api::friendly_error);
                match outcome {
                    Ok(()) => reload.bump(),
                    Err(e) => {
                        msg.set(Some(e));
                        busy.set(false);
                        confirm_delete.set(false);
                    }
                }
            });
        }
    };

    let base_changed = *base_url.read() != original_base;
    let test_id = id;

    rsx! {
        div { class: "ik-tile", style: "margin-bottom:12px;",
            div { class: "ik-flex", style: "justify-content:space-between;align-items:flex-start;gap:12px;",
                div { class: "grow",
                    div { class: "ik-flex",
                        span { style: "font-weight:600;", "{pro.name}" }
                        HealthPill { state: provider_state_token(pro.state).to_owned() }
                    }
                    div { class: "ik-muted ik-mono", style: "font-size:12px;margin-top:2px;word-break:break-all;",
                        "{pro.slug} · {adapter_token(pro.adapter)} · {pro.base_url}"
                    }
                }
                div { class: "ik-flex", style: "flex-wrap:wrap;justify-content:flex-end;",
                    select {
                        class: "ik-input",
                        style: "width:auto;",
                        value: if *scan_mode.read() == ScanMode::Full { "full" } else { "fast" },
                        onchange: move |e| {
                            scan_mode
                                .set(if e.value() == "full" { ScanMode::Full } else { ScanMode::Fast });
                        },
                        option { value: "fast", "Fast" }
                        option { value: "full", "Full" }
                    }
                    button { class: "ik-btn", onclick: scan, "Scan" }
                    button { class: "ik-btn", disabled: *busy.read(), onclick: toggle_state,
                        if is_disabled { "Enable" } else { "Disable" }
                    }
                    button {
                        class: "ik-btn",
                        onclick: move |_| {
                            let s = *show_test.read();
                            show_test.set(!s);
                        },
                        "Test"
                    }
                    button {
                        class: "ik-btn",
                        onclick: move |_| {
                            let e = *expanded.read();
                            expanded.set(!e);
                        },
                        if *expanded.read() { "Close" } else { "Edit" }
                    }
                    if is_admin {
                        button {
                            class: "ik-btn",
                            style: "color:var(--vermilion);",
                            onclick: move |_| confirm_delete.set(true),
                            "Delete"
                        }
                    }
                }
            }

            if let Some(m) = msg.read().clone() {
                p { style: "margin:8px 0 0;color:var(--vermilion);font-size:13px;", "{m}" }
            }

            if *confirm_delete.read() {
                div { class: "ik-flex", style: "margin-top:10px;",
                    span { class: "ik-muted", style: "font-size:13px;",
                        "Delete this provider and all its source links? This cannot be undone."
                    }
                    button { class: "ik-btn primary", disabled: *busy.read(), onclick: delete, "Confirm delete" }
                    button { class: "ik-btn", onclick: move |_| confirm_delete.set(false), "Cancel" }
                }
            }

            if *expanded.read() {
                div { style: "margin-top:14px;display:grid;gap:10px;border-top:1px solid var(--border);padding-top:14px;",
                    Field { label: "Display name",
                        input {
                            class: "ik-input",
                            value: "{name}",
                            oninput: move |e| name.set(e.value()),
                        }
                    }
                    Field { label: "Base URL (changing it re-resolves every stored link)",
                        input {
                            class: "ik-input ik-mono",
                            value: "{base_url}",
                            oninput: move |e| base_url.set(e.value()),
                        }
                    }
                    Field { label: "Adapter config (JSON)",
                        textarea {
                            class: "ik-input ik-mono",
                            style: "min-height:140px;resize:vertical;",
                            value: "{config}",
                            oninput: move |e| config.set(e.value()),
                        }
                    }
                    div { style: "display:grid;grid-template-columns:repeat(3,1fr);gap:10px;",
                        Field { label: "Requests / sec",
                            input {
                                class: "ik-input ik-mono",
                                value: "{rps}",
                                oninput: move |e| rps.set(e.value()),
                            }
                        }
                        Field { label: "Concurrency",
                            input {
                                class: "ik-input ik-mono",
                                value: "{concurrency}",
                                oninput: move |e| concurrency.set(e.value()),
                            }
                        }
                        Field { label: "Crawl delay (ms)",
                            input {
                                class: "ik-input ik-mono",
                                value: "{crawl_delay_ms}",
                                oninput: move |e| crawl_delay_ms.set(e.value()),
                            }
                        }
                    }
                    Field { label: "User agent",
                        input {
                            class: "ik-input ik-mono",
                            value: "{user_agent}",
                            oninput: move |e| user_agent.set(e.value()),
                        }
                    }
                    div { class: "ik-muted", style: "font-size:12px;",
                        "Rate limits are clamped to the system ceilings on save (≤ 4 req/s, ≤ 8 concurrent)."
                    }
                    if *confirm_migrate.read() {
                        div { class: "ik-flex",
                            span { class: "ik-muted", style: "font-size:13px;",
                                "The base URL changed — re-resolve every stored link to the new domain?"
                            }
                            button { class: "ik-btn primary", disabled: *busy.read(), onclick: on_confirm_migrate, "Confirm migration" }
                            button { class: "ik-btn", onclick: move |_| confirm_migrate.set(false), "Cancel" }
                        }
                    } else {
                        div {
                            button { class: "ik-btn primary", disabled: *busy.read(), onclick: on_save,
                                if base_changed { "Save changes (migrates domain)" } else { "Save changes" }
                            }
                        }
                    }
                }
            }

            if *show_test.read() {
                AdapterTestPanel { provider_id: test_id }
            }
        }
    }
}

/// Live adapter dry-run against the provider's site. Runs on demand only; shows the raw
/// parsed sample so operators can validate selectors without a deploy.
#[component]
pub(super) fn AdapterTestPanel(provider_id: ProviderId) -> Element {
    let api = api::use_api();
    let session = use_session();
    let mut path = use_signal(String::new);
    let mut running = use_signal(|| false);
    let mut result: Signal<Option<Result<serde_json::Value, String>>> = use_signal(|| None);

    let run = {
        move |_| {
            let p = path.read().trim().to_owned();
            running.set(true);
            spawn(async move {
                let client = api.client();
                let out = match session.token_value() {
                    Some(_) => {
                        let body = TestAdapterRequest {
                            path: (!p.is_empty()).then_some(p),
                        };
                        client
                            .test_adapter()
                            .id(provider_id)
                            .body(TestAdapterBody::Variant1(body))
                            .send()
                            .await
                            .map(ResponseValue::into_inner)
                            .map_err(api::friendly_error)
                    }
                    None => Err("You are signed out.".to_owned()),
                };
                result.set(Some(out));
                running.set(false);
            });
        }
    };

    let output = match &*result.read() {
        None => rsx! {
            p { class: "ik-muted", style: "font-size:13px;margin:8px 0 0;",
                "Runs the adapter against the live site (latest list, and optionally one series path)."
            }
        },
        Some(Ok(v)) => {
            let text = serde_json::to_string_pretty(v).unwrap_or_else(|_| v.to_string());
            rsx! {
                pre {
                    class: "ik-mono",
                    style: "margin:10px 0 0;padding:12px;background:var(--surface);border:1px solid var(--border);border-radius:10px;font-size:12px;max-height:340px;overflow:auto;white-space:pre-wrap;word-break:break-word;",
                    "{text}"
                }
            }
        }
        Some(Err(e)) => rsx! {
            p { style: "margin:10px 0 0;color:var(--vermilion);font-size:13px;", "{e}" }
        },
    };

    rsx! {
        div { style: "margin-top:14px;border-top:1px solid var(--border);padding-top:14px;",
            div { class: "ik-flex",
                input {
                    class: "ik-input ik-mono",
                    style: "flex:1;",
                    placeholder: "optional series path, e.g. /manga/some-title",
                    value: "{path}",
                    oninput: move |e| path.set(e.value()),
                }
                button { class: "ik-btn primary", disabled: *running.read(), onclick: run,
                    if *running.read() { "Running…" } else { "Run test" }
                }
            }
            {output}
        }
    }
}
