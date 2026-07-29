//! Console · Providers — the fetch pipeline's control surface, as a list and an inspector.
//!
//! The list is health at a glance; the inspector is where a provider is actually changed.
//! Two rules the design makes explicit and this module enforces:
//!
//! 1. **A config edit cannot be saved until a dry-run passes.** Editing the adapter config
//!    clears the gate; a successful `POST /v1/admin/providers/:id/test` re-arms it. Name and
//!    base-URL edits are not gated — they cannot break parsing.
//! 2. **Reversible and irreversible destructive actions look different.** Pause and blocklist
//!    act inline. Delete is type-to-confirm and names the exact blast radius with real counts.
//!
//! Deliberately absent, because the API has no field for them: the adapter kind is fixed at
//! registration (`UpdateProvider` carries no `adapter`), so it is shown rather than offered as
//! a segmented control, and there is no per-provider `language`.

use super::shell::{
    InlineConfirm, ListFooter, ListSearch, NoSelection, Section, SliderRow, TypeToConfirm,
};
use crate::api;
use crate::components::{async_view, ErrorLine, OutcomeLine, SkeletonBlock};
use crate::hooks::{use_busy, use_outcome, use_reload, Reload};
use crate::i18n::use_i18n;
use crate::icons::{Ic, Icon};
use crate::models::*;
use crate::state::capabilities::use_capabilities;
use crate::util::{monogram, rel_time, thousands};
use crate::views::console::{
    adapter_token, config_editor_text, politeness_json, run_state_pill, ADAPTER_KINDS,
};
use crate::wire::types::Permission;
use dioxus::prelude::*;
use progenitor_client::ResponseValue;

/// The inspector's tab strip.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Config,
    Politeness,
    Coverage,
    Runs,
    Danger,
}

impl Tab {
    const ALL: [Tab; 5] = [
        Self::Config,
        Self::Politeness,
        Self::Coverage,
        Self::Runs,
        Self::Danger,
    ];

    fn label_key(self) -> &'static str {
        match self {
            Self::Config => "console.providers.tab.config",
            Self::Politeness => "console.providers.tab.politeness",
            Self::Coverage => "console.providers.tab.coverage",
            Self::Runs => "console.providers.tab.runs",
            Self::Danger => "console.providers.tab.danger",
        }
    }
}

/// The list pane and the inspector pane, as the console shell's two grid children.
#[component]
pub(super) fn ProvidersEntity() -> Element {
    let api = api::use_api();
    let i18n = use_i18n();
    let caps = use_capabilities();
    let can_create = caps.can(Permission::ProvidersCreate);
    let reload = use_reload();
    let query = use_signal(String::new);
    let mut selected = use_signal(|| Option::<ProviderId>::None);
    let mut creating = use_signal(|| false);

    let providers = use_resource(move || {
        reload.track();
        let client = api.client();
        async move {
            client
                .list_providers()
                .send()
                .await
                .map(ResponseValue::into_inner)
                .map_err(|e| api::friendly_error(i18n, e))
        }
    });

    // Health and volume for the list rows, from the aggregate endpoint rather than a per-row
    // fetch. It is permission-gated separately, so a reader with `providers.read` alone still
    // gets a list — just without the meter under each name.
    let stats = use_resource(move || {
        reload.track();
        let client = api.client();
        async move {
            client
                .provider_stats()
                .send()
                .await
                .map(ResponseValue::into_inner)
                .unwrap_or_default()
        }
    });

    let all = providers.read_unchecked().clone();
    let stat_rows = stats.read_unchecked().clone().unwrap_or_default();
    let needle = query.read().trim().to_lowercase();
    let rows: Vec<Provider> = match &all {
        Some(Ok(list)) => list
            .iter()
            .filter(|p| {
                needle.is_empty()
                    || p.slug.to_lowercase().contains(&needle)
                    || p.name.to_lowercase().contains(&needle)
            })
            .cloned()
            .collect(),
        _ => Vec::new(),
    };

    // Land on the first row rather than an empty inspector: the console is read far more often
    // than it is edited, and an empty right pane wastes the first look.
    let current = selected.read().or_else(|| rows.first().map(|p| p.id));
    let chosen = current.and_then(|id| rows.iter().find(|p| p.id == id).cloned());

    rsx! {
        div { class: "ik-cons-list",
            div { class: "ik-cons-listhead",
                ListSearch {
                    placeholder: i18n.t("console.providers.filter"),
                    query,
                    hits: i18n.plural(
                        "console.providers.hits",
                        i64::try_from(rows.len()).unwrap_or(0),
                        &[],
                    ),
                }
                if can_create {
                    button {
                        class: "ik-btn xs",
                        style: "align-self:flex-start;",
                        onclick: move |_| {
                            let next = !*creating.read();
                            creating.set(next);
                        },
                        Ic { icon: Icon::Add, size: 13 }
                        {i18n.t("console.providers.new")}
                    }
                }
            }
            {
                async_view(
                    &providers,
                    reload,
                    || rsx! {
                        div { style: "padding:12px;",
                            SkeletonBlock { height: 180 }
                        }
                    },
                    |_| {
                        if rows.is_empty() {
                            return rsx! {
                                div { class: "ik-empty", style: "margin:12px;padding:24px;",
                                    {i18n.t("console.providers.empty")}
                                }
                            };
                        }
                        rsx! {
                            for provider in rows.clone() {
                                ProviderRow {
                                    key: "{provider.id}",
                                    provider: provider.clone(),
                                    stat: stat_rows.iter().find(|s| s.slug == provider.slug).cloned(),
                                    selected: current == Some(provider.id),
                                    on_pick: move |id| {
                                        creating.set(false);
                                        selected.set(Some(id));
                                    },
                                }
                            }
                        }
                    },
                )
            }
            ListFooter {
                count: i18n.plural(
                    "console.providers.count",
                    i64::try_from(rows.len()).unwrap_or(0),
                    &[],
                ),
                keys: false,
            }
        }
        if *creating.read() {
            div { class: "ik-cons-pane",
                CreateProviderForm {
                    reload,
                    on_done: move |()| creating.set(false),
                }
            }
        } else if let Some(provider) = chosen {
            ProviderInspector {
                key: "{provider.id}",
                provider: provider.clone(),
                stat: stat_rows.iter().find(|s| s.slug == provider.slug).cloned(),
                reload,
                on_deleted: move |()| selected.set(None),
            }
        } else {
            NoSelection { message: i18n.t("console.providers.pick") }
        }
    }
}

/// One provider in the list: name, state, a mono meta line and the healthy-links meter.
#[component]
fn ProviderRow(
    provider: Provider,
    stat: Option<ProviderStat>,
    selected: bool,
    on_pick: EventHandler<ProviderId>,
) -> Element {
    let i18n = use_i18n();
    let id = provider.id;
    let disabled = provider.state == ProviderState::Disabled;
    let healthy = stat.as_ref().and_then(healthy_percent);

    let meta = match &stat {
        Some(stat) => i18n.args(
            "console.providers.rowMeta",
            &[
                ("series", &thousands(stat.series_count)),
                (
                    "healthy",
                    &healthy.map_or_else(|| "—".to_owned(), |p| format!("{p:.0}")),
                ),
                ("when", &rel_time(i18n, stat.last_scanned_at.as_deref())),
            ],
        ),
        None => provider.base_url.clone(),
    };

    let class = match (selected, disabled) {
        (true, _) => "ik-cons-row selected",
        (false, true) => "ik-cons-row dim",
        (false, false) => "ik-cons-row",
    };

    rsx! {
        button {
            class: "{class}",
            "aria-current": if selected { "true" } else { "false" },
            onclick: move |_| on_pick.call(id),
            div { class: "ik-flex", style: "gap:8px;",
                span { style: "font-weight:600;font-size:13.5px;", "{provider.name}" }
                HealthPill { state: provider_state_token(provider.state).to_owned() }
            }
            div { class: "ik-mono", style: "font-size:10.5px;color:var(--muted);margin-top:3px;word-break:break-word;",
                "{meta}"
            }
            if let Some(percent) = healthy {
                div { class: "ik-bar",
                    span {
                        class: if percent >= 95.0 { "" } else { "warn" },
                        style: "width:{percent}%;",
                    }
                }
            }
        }
    }
}

/// Share of this provider's source links that are in a serving state.
///
/// This is what the API actually measures. The design calls the meter "solve %"; there is no
/// challenge-solve ratio on the wire, and inventing one would put a number on the screen that
/// nothing computes.
fn healthy_percent(stat: &ProviderStat) -> Option<f64> {
    if stat.source_count <= 0 {
        return None;
    }
    let serving = (stat.source_count - stat.blocked_sources).max(0);
    // Both counts are row totals, far inside `f64`'s exact integer range.
    #[allow(clippy::cast_precision_loss)]
    Some((serving as f64 / stat.source_count as f64) * 100.0)
}

#[component]
pub(super) fn HealthPill(state: String) -> Element {
    let i18n = use_i18n();
    let class = match state.as_str() {
        "active" => "ik-pill jade",
        "blocked" | "disabled" => "ik-pill vermilion",
        _ => "ik-pill",
    };
    // The token doubles as the catalogue key. A state the frontend does not know a word for
    // still renders as its raw token rather than as `Key '…' not found`.
    let label = match state.as_str() {
        "" => i18n.t("console.providerState.unknown"),
        "active" | "degraded" | "challenged" | "solving" | "blocked" | "disabled" => {
            i18n.t(&format!("console.providerState.{state}"))
        }
        _ => state,
    };
    rsx! {
        span { class: "{class}", style: "font-size:9.5px;", "{label}" }
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

/// The full editor for one provider.
#[component]
fn ProviderInspector(
    provider: Provider,
    stat: Option<ProviderStat>,
    reload: Reload,
    on_deleted: EventHandler<()>,
) -> Element {
    let api = api::use_api();
    let i18n = use_i18n();
    let caps = use_capabilities();
    // One control per capability, rather than one tier unlocking all of them: each appears
    // exactly when the server would accept the call behind it.
    let can_edit = caps.can(Permission::ProvidersWrite);
    let can_change_state = caps.can(Permission::ProvidersState);
    let can_scan = caps.can(Permission::ScansRun);
    let can_test = caps.can(Permission::ProvidersTest);
    let can_delete = caps.can(Permission::ProvidersDelete);

    let id = provider.id;
    let original_base = provider.base_url.clone();
    let original_config = config_editor_text(&provider.config);
    let is_disabled = provider.state == ProviderState::Disabled;

    let mut tab = use_signal(|| Tab::Config);
    // A fast scan walks what changed; a full one re-reads the catalogue. Per provider, because
    // re-reading every provider to re-check one is what operators were doing without it.
    let mut scan_mode = use_signal(|| ScanMode::Fast);
    let busy = use_busy();
    let mut outcome = use_outcome();

    let mut name = use_signal(|| provider.name.clone());
    let mut base_url = use_signal(|| provider.base_url.clone());
    let mut config = use_signal(|| original_config.clone());
    // The dry-run gate: cleared by every config edit, re-armed by a passing dry run.
    let mut dry_run_passed = use_signal(|| false);
    let mut dry_run: Signal<Option<Result<serde_json::Value, String>>> = use_signal(|| None);

    let mut rps = use_signal(|| provider.politeness.rps.unwrap_or(1.0));
    let mut concurrency = use_signal(|| f64::from(provider.politeness.concurrency.unwrap_or(2)));
    // Crawl delays are milliseconds and always well inside `f64`'s exact integer range.
    #[allow(clippy::cast_precision_loss)]
    let mut crawl_delay = use_signal(|| provider.politeness.crawl_delay_ms.unwrap_or(0) as f64);
    let mut user_agent = use_signal(|| provider.politeness.user_agent.clone().unwrap_or_default());
    // Empty string is the "no emulation" sentinel, matching `politeness_json`. The generated
    // client models a nullable `$ref` as an untagged two-variant enum; `Variant0` is the raw
    // JSON fallback.
    let mut emulation = use_signal(|| match &provider.politeness.emulation {
        Some(PolitenessEmulation::Variant1(e)) => e.to_string(),
        Some(PolitenessEmulation::Variant0(v)) => v.as_str().unwrap_or_default().to_owned(),
        None => String::new(),
    });

    let config_dirty = *config.read() != original_config;
    let base_changed = *base_url.read() != original_base;
    let config_valid = serde_json::from_str::<serde_json::Value>(&config.read()).is_ok();
    // Saving is blocked while an unproven config edit is pending; everything else is free.
    let save_blocked = config_dirty && !*dry_run_passed.read();

    let save = {
        let original_base = original_base.clone();
        move |_| {
            if !busy.claim() {
                return;
            }
            outcome.set(None);
            let parsed = match serde_json::from_str::<serde_json::Value>(&config.peek()) {
                Ok(value) => value,
                Err(e) => {
                    outcome.set(Some(Err(i18n.args(
                        "console.providers.badConfig",
                        &[("message", &e.to_string())],
                    ))));
                    busy.release();
                    return;
                }
            };
            let politeness = match politeness_json(
                &format!("{}", *rps.peek()),
                &format!("{:.0}", *concurrency.peek()),
                &format!("{:.0}", *crawl_delay.peek()),
                &user_agent.peek(),
                &emulation.peek(),
            ) {
                Ok(value) => value,
                Err(key) => {
                    outcome.set(Some(Err(i18n.t(key))));
                    busy.release();
                    return;
                }
            };
            let migrating = *base_url.peek() != original_base;
            let body = UpdateProvider {
                name: name.peek().clone(),
                base_url: base_url.peek().clone(),
                config: Some(parsed),
                politeness: serde_json::from_value::<Politeness>(politeness).ok(),
            };
            let client = api.client();
            spawn(async move {
                match client.update_provider().id(id).body(body).send().await {
                    Ok(_) => {
                        outcome.set(Some(Ok(i18n.t(if migrating {
                            "console.providers.savedMigrating"
                        } else {
                            "console.providers.saved"
                        }))));
                        dry_run_passed.set(false);
                        reload.bump();
                    }
                    Err(e) => outcome.set(Some(Err(api::friendly_error(i18n, e)))),
                }
                busy.release();
            });
        }
    };

    let mut set_state = move |target: ProviderState| {
        if !busy.claim() {
            return;
        }
        outcome.set(None);
        let client = api.client();
        spawn(async move {
            match client
                .set_provider_state()
                .id(id)
                .body(SetProviderStateBody { state: target })
                .send()
                .await
            {
                Ok(_) => reload.bump(),
                Err(e) => outcome.set(Some(Err(api::friendly_error(i18n, e)))),
            }
            busy.release();
        });
    };

    let scan = move |_| {
        outcome.set(None);
        let client = api.client();
        spawn(async move {
            let body = TriggerScan {
                mode: *scan_mode.peek(),
                provider_id: Some(TriggerScanProviderId::Variant1(id)),
            };
            match client.trigger_scan().body(body).send().await {
                Ok(_) => outcome.set(Some(Ok(i18n.t("console.providers.scanQueued")))),
                Err(e) => outcome.set(Some(Err(api::friendly_error(i18n, e)))),
            }
        });
    };

    let run_dry = move |_| {
        if !busy.claim() {
            return;
        }
        dry_run.set(None);
        let client = api.client();
        spawn(async move {
            let outcome_value = client
                .test_adapter()
                .id(id)
                .body(TestAdapterBody::Variant1(TestAdapterRequest { path: None }))
                .send()
                .await
                .map(ResponseValue::into_inner)
                .map_err(|e| api::friendly_error(i18n, e));
            dry_run_passed.set(outcome_value.is_ok());
            dry_run.set(Some(outcome_value));
            busy.release();
        });
    };

    let format_config = move |_| {
        // Read the text out before writing back: `peek` holds a borrow for the whole `if let`.
        let text = config.peek().clone();
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) {
            config.set(config_editor_text(&value));
        }
    };

    let meta_scanned = rel_time(
        i18n,
        stat.as_ref()
            .and_then(|s| s.last_scanned_at.as_deref())
            .or(provider.last_full_scan_at.as_deref()),
    );
    let tile = monogram(&provider.name);

    rsx! {
        div { class: "ik-cons-insp",
            div { class: "ik-cons-insphead",
                div { class: "ik-flex", style: "align-items:flex-start;gap:14px;",
                    span { class: "ik-mono-tile xl", "{tile}" }
                    div { style: "min-width:0;flex:1;",
                        div { class: "ik-flex", style: "gap:10px;flex-wrap:wrap;",
                            h2 { class: "ik-insp-title", "{provider.name}" }
                            HealthPill { state: provider_state_token(provider.state).to_owned() }
                        }
                        div { class: "ik-meta-line",
                            span { "{adapter_token(provider.adapter)}" }
                            if let Some(stat) = stat.clone() {
                                span {
                                    {
                                        i18n.args(
                                            "console.providers.metaSeries",
                                            &[("count", &thousands(stat.series_count))],
                                        )
                                    }
                                }
                                span {
                                    {
                                        i18n.args(
                                            "console.providers.metaToday",
                                            &[("count", &thousands(stat.chapters_24h))],
                                        )
                                    }
                                }
                            }
                            span { class: "ok", "{meta_scanned}" }
                        }
                    }
                    div { class: "ik-flex", style: "gap:7px;flex:none;flex-wrap:wrap;justify-content:flex-end;",
                        if can_scan {
                            select {
                                class: "ik-select",
                                style: "font-size:12px;padding:8px 10px;",
                                "aria-label": i18n.t("console.providers.scanNow"),
                                value: if *scan_mode.read() == ScanMode::Full { "full" } else { "fast" },
                                onchange: move |e| {
                                    scan_mode
                                        .set(if e.value() == "full" { ScanMode::Full } else { ScanMode::Fast });
                                },
                                option { value: "fast", {i18n.t("console.providers.fast")} }
                                option { value: "full", {i18n.t("console.providers.full")} }
                            }
                            button { class: "ik-btn sm", onclick: scan,
                                {i18n.t("console.providers.scanNow")}
                            }
                        }
                        if can_change_state {
                            button {
                                class: "ik-btn sm",
                                disabled: busy.is_busy(),
                                onclick: move |_| {
                                    set_state(
                                        if is_disabled { ProviderState::Active } else { ProviderState::Disabled },
                                    );
                                },
                                if is_disabled {
                                    {i18n.t("console.providers.enable")}
                                } else {
                                    {i18n.t("console.providers.pause")}
                                }
                            }
                        }
                        if can_edit {
                            button {
                                class: "ik-btn sm primary",
                                disabled: busy.is_busy() || save_blocked,
                                title: if save_blocked { i18n.t("console.providers.saveGate") } else { String::new() },
                                onclick: save,
                                {i18n.t("console.providers.saveChanges")}
                            }
                        }
                    }
                }
                div { class: "ik-tabs flush", style: "margin-top:14px;",
                    for entry in Tab::ALL {
                        button {
                            key: "{entry.label_key()}",
                            class: if *tab.read() == entry { "ik-tab active" } else { "ik-tab" },
                            onclick: move |_| tab.set(entry),
                            {i18n.t(entry.label_key())}
                        }
                    }
                }
            }
            div { style: "padding:0 22px;",
                OutcomeLine { outcome: outcome.read().clone() }
            }
            match *tab.read() {
                Tab::Config => rsx! {
                    div { class: "ik-cons-inspbody",
                        div { class: "ik-cons-col",
                            Section { label: i18n.t("console.providers.identity"),
                                div { style: "display:flex;flex-direction:column;gap:9px;",
                                    div { class: "ik-kv",
                                        label { class: "k", r#for: "tv-p-name", {i18n.t("console.providers.field.name")} }
                                        input {
                                            id: "tv-p-name",
                                            class: "ik-input",
                                            style: "font-size:12.5px;padding:9px 11px;",
                                            disabled: !can_edit,
                                            value: "{name}",
                                            oninput: move |e| name.set(e.value()),
                                        }
                                    }
                                    div { class: "ik-kv",
                                        label { class: "k", r#for: "tv-p-base", {i18n.t("console.providers.field.baseUrl")} }
                                        input {
                                            id: "tv-p-base",
                                            class: "ik-input ik-mono",
                                            style: "font-size:12.5px;padding:9px 11px;",
                                            disabled: !can_edit,
                                            value: "{base_url}",
                                            oninput: move |e| base_url.set(e.value()),
                                        }
                                    }
                                    if base_changed {
                                        div { class: "ik-kv",
                                            span {}
                                            span { class: "warn",
                                                {
                                                    i18n.args(
                                                        "console.providers.migrateWarning",
                                                        &[
                                                            (
                                                                "count",
                                                                &stat
                                                                    .as_ref()
                                                                    .map_or_else(|| "—".to_owned(), |s| thousands(s.series_count)),
                                                            ),
                                                        ],
                                                    )
                                                }
                                            }
                                        }
                                    }
                                    div { class: "ik-kv",
                                        span { class: "k", {i18n.t("console.providers.field.slug")} }
                                        span { class: "ik-mono", style: "font-size:12.5px;", "{provider.slug}" }
                                    }
                                    div { class: "ik-kv",
                                        span { class: "k", {i18n.t("console.providers.field.adapter")} }
                                        span {
                                            span { class: "ik-mono", style: "font-size:12.5px;",
                                                "{adapter_token(provider.adapter)}"
                                            }
                                            span { class: "ik-muted", style: "font-size:11.5px;display:block;margin-top:2px;",
                                                {i18n.t("console.providers.adapterFixed")}
                                            }
                                        }
                                    }
                                }
                            }
                            Section {
                                label: i18n.t("console.providers.adapterConfig"),
                                trailing: rsx! {
                                    span {
                                        class: "ik-mono",
                                        style: if config_valid { "font-size:11px;color:var(--faint);" } else { "font-size:11px;color:var(--star);" },
                                        if config_valid {
                                            {i18n.t("console.providers.jsonValid")}
                                        } else {
                                            {i18n.t("console.providers.jsonInvalid")}
                                        }
                                    }
                                },
                                textarea {
                                    class: if config_valid { "ik-jsonblock" } else { "ik-jsonblock bad" },
                                    spellcheck: "false",
                                    disabled: !can_edit,
                                    "aria-label": i18n.t("console.providers.adapterConfig"),
                                    value: "{config}",
                                    oninput: move |e| {
                                        config.set(e.value());
                                        dry_run_passed.set(false);
                                    },
                                }
                                div { class: "ik-flex", style: "gap:7px;margin-top:9px;flex-wrap:wrap;",
                                    if can_test {
                                        button {
                                            class: "ik-btn sm",
                                            disabled: busy.is_busy(),
                                            onclick: run_dry,
                                            {i18n.t("console.providers.dryRun")}
                                        }
                                    }
                                    button {
                                        class: "ik-btn sm",
                                        disabled: !config_valid || !can_edit,
                                        onclick: format_config,
                                        {i18n.t("console.providers.format")}
                                    }
                                    span { class: "ik-mono", style: "margin-left:auto;align-self:center;font-size:11.5px;color:var(--faint);",
                                        {i18n.t("console.providers.saveGate")}
                                    }
                                }
                            }
                        }
                        div { class: "ik-cons-col",
                            DryRunResult { result: dry_run.read().clone() }
                        }
                    }
                },
                Tab::Politeness => rsx! {
                    div { class: "ik-cons-inspbody",
                        div { class: "ik-cons-col",
                            Section { label: i18n.t("console.providers.politeness"),
                                div { style: "display:flex;flex-direction:column;gap:12px;",
                                    SliderRow {
                                        label: i18n.t("console.providers.field.rps"),
                                        value: *rps.read(),
                                        min: 0.1,
                                        max: 10.0,
                                        step: 0.1,
                                        display: format!("{:.1}", *rps.read()),
                                        on_input: move |v| rps.set(v),
                                    }
                                    SliderRow {
                                        label: i18n.t("console.providers.field.concurrency"),
                                        value: *concurrency.read(),
                                        min: 1.0,
                                        max: 16.0,
                                        step: 1.0,
                                        display: format!("{:.0}", *concurrency.read()),
                                        on_input: move |v| concurrency.set(v),
                                    }
                                    SliderRow {
                                        label: i18n.t("console.providers.field.crawlDelay"),
                                        value: *crawl_delay.read(),
                                        min: 0.0,
                                        max: 5000.0,
                                        step: 50.0,
                                        display: format!("{:.0}ms", *crawl_delay.read()),
                                        on_input: move |v| crawl_delay.set(v),
                                    }
                                    div { class: "ik-kv narrow",
                                        label { class: "k", r#for: "tv-p-emu", {i18n.t("console.providers.field.emulation")} }
                                        select {
                                            id: "tv-p-emu",
                                            class: "ik-select",
                                            disabled: !can_edit,
                                            value: "{emulation}",
                                            onchange: move |e| emulation.set(e.value()),
                                            option { value: "chrome", "Chrome" }
                                            option { value: "firefox", "Firefox" }
                                            option { value: "safari", "Safari" }
                                            option { value: "edge", "Edge" }
                                            option { value: "ok_http", "OkHttp (Android)" }
                                            option { value: "", {i18n.t("console.providers.emulationNone")} }
                                        }
                                    }
                                    div { class: "ik-kv narrow",
                                        label { class: "k", r#for: "tv-p-ua", {i18n.t("console.providers.field.userAgent")} }
                                        input {
                                            id: "tv-p-ua",
                                            class: "ik-input ik-mono",
                                            style: "font-size:11.5px;padding:8px 10px;",
                                            disabled: !can_edit || !emulation.read().is_empty(),
                                            value: "{user_agent}",
                                            oninput: move |e| user_agent.set(e.value()),
                                        }
                                    }
                                }
                                p { class: "ik-muted", style: "font-size:11.5px;line-height:1.5;margin:10px 0 0;",
                                    {i18n.t("console.providers.politenessNote")}
                                }
                            }
                        }
                        div { class: "ik-cons-col",
                            Section { label: i18n.t("console.providers.limitsHead"),
                                p { class: "ik-muted", style: "font-size:11.5px;line-height:1.5;margin:0 0 8px;",
                                    {i18n.t("console.providers.clampNote")}
                                }
                                p { class: "ik-muted", style: "font-size:11.5px;line-height:1.5;margin:0;",
                                    {i18n.t("console.providers.emulationNote")}
                                }
                            }
                        }
                    }
                },
                Tab::Coverage => rsx! {
                    div { class: "ik-cons-inspbody",
                        div { class: "ik-cons-col", style: "grid-column:1 / -1;",
                            CoverageTab { stat: stat.clone() }
                        }
                    }
                },
                Tab::Runs => rsx! {
                    div { class: "ik-cons-inspbody",
                        div { class: "ik-cons-col", style: "grid-column:1 / -1;",
                            RunsTab { provider_id: id }
                        }
                    }
                },
                Tab::Danger => rsx! {
                    div { class: "ik-cons-inspbody",
                        div { class: "ik-cons-col", style: "grid-column:1 / -1;max-width:620px;",
                            DangerTab {
                                provider: provider.clone(),
                                stat: stat.clone(),
                                can_change_state,
                                can_delete,
                                reload,
                                on_deleted,
                            }
                        }
                    }
                },
            }
        }
    }
}

/// The dry-run panel: what the adapter parsed, and the raw sample behind the claim.
#[component]
fn DryRunResult(result: Option<Result<serde_json::Value, String>>) -> Element {
    let i18n = use_i18n();
    let Some(result) = result else {
        return rsx! {
            Section { label: i18n.t("console.providers.dryRunHead"),
                p { class: "ik-muted", style: "font-size:12px;line-height:1.5;margin:0;",
                    {i18n.t("console.adapterTest.hint")}
                }
            }
        };
    };

    match result {
        Err(message) => rsx! {
            Section { label: i18n.t("console.providers.dryRunHead"),
                ErrorLine { message }
            }
        },
        Ok(value) => {
            let parsed = parsed_count(&value);
            let text = serde_json::to_string_pretty(&value).unwrap_or_else(|_| value.to_string());
            rsx! {
                Section {
                    label: i18n.t("console.providers.dryRunHead"),
                    trailing: rsx! {
                        span { class: "ik-pill jade", style: "font-size:9.5px;",
                            match parsed {
                                Some(count) => {
                                    i18n.plural(
                                        "console.providers.parsed",
                                        i64::try_from(count).unwrap_or(0),
                                        &[],
                                    )
                                }
                                None => i18n.t("console.providers.parsedOk"),
                            }
                        }
                    },
                    pre {
                        class: "ik-jsonblock",
                        style: "max-height:340px;white-space:pre-wrap;word-break:break-word;",
                        "{text}"
                    }
                }
            }
        }
    }
}

/// How many entries a dry-run returned, when the payload's shape makes that answerable.
///
/// Adapter output is adapter-defined, so this looks for the two shapes every adapter in the
/// tree produces — a bare array, or an object with one array in it — and otherwise declines to
/// guess rather than reporting a number it cannot stand behind.
fn parsed_count(value: &serde_json::Value) -> Option<usize> {
    if let Some(array) = value.as_array() {
        return Some(array.len());
    }
    let object = value.as_object()?;
    let mut arrays = object.values().filter_map(serde_json::Value::as_array);
    let first = arrays.next()?;
    arrays.next().is_none().then_some(first.len())
}

/// What this provider actually carries.
#[component]
fn CoverageTab(stat: Option<ProviderStat>) -> Element {
    let i18n = use_i18n();
    let Some(stat) = stat else {
        return rsx! {
            div { class: "ik-empty", {i18n.t("console.providers.noStats")} }
        };
    };
    rsx! {
        div { class: "ik-kpis",
            Kpi { label: i18n.t("console.stats.col.series"), value: thousands(stat.series_count) }
            Kpi { label: i18n.t("console.stats.col.sources"), value: thousands(stat.source_count) }
            Kpi { label: i18n.t("console.stats.col.chapters"), value: thousands(stat.chapter_count) }
            Kpi { label: i18n.t("console.providers.blocked"), value: thousands(stat.blocked_sources) }
            Kpi { label: i18n.t("console.stats.col.new24h"), value: thousands(stat.chapters_24h) }
            Kpi { label: i18n.t("console.stats.col.new7d"), value: thousands(stat.chapters_7d) }
        }
        div { class: "ik-meta-line", style: "margin-top:14px;",
            span {
                {
                    i18n.args(
                        "console.providers.lastScan",
                        &[("when", &rel_time(use_i18n(), stat.last_scanned_at.as_deref()))],
                    )
                }
            }
            span {
                {
                    i18n.args(
                        "console.providers.lastFullScan",
                        &[("when", &rel_time(use_i18n(), stat.last_full_scan_at.as_deref()))],
                    )
                }
            }
            span {
                {
                    i18n.args(
                        "console.providers.lastChapter",
                        &[("when", &rel_time(use_i18n(), stat.last_chapter_at.as_deref()))],
                    )
                }
            }
        }
    }
}

#[component]
fn Kpi(label: String, value: String) -> Element {
    rsx! {
        div { class: "ik-kpi",
            div { class: "ik-kpi-label", "{label}" }
            div { class: "ik-kpi-value", style: "font-size:24px;", "{value}" }
        }
    }
}

/// This provider's recent scan runs, filtered out of the run list.
#[component]
fn RunsTab(provider_id: ProviderId) -> Element {
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
                .iter()
                .filter(|run| {
                    matches!(&run.provider_id, Some(ScanRunProviderId::Variant1(id)) if *id == provider_id)
                })
                .cloned()
                .collect();
            if mine.is_empty() {
                return rsx! {
                    div { class: "ik-empty", {i18n.t("console.providers.noRuns")} }
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

/// Two tiers, as designed: blocklisting is reversible and acts inline; deleting is not and is
/// gated on typing the slug.
#[component]
fn DangerTab(
    provider: Provider,
    stat: Option<ProviderStat>,
    can_change_state: bool,
    can_delete: bool,
    reload: Reload,
    on_deleted: EventHandler<()>,
) -> Element {
    let api = api::use_api();
    let i18n = use_i18n();
    let busy = use_busy();
    let mut outcome = use_outcome();
    let mut confirming_block = use_signal(|| false);
    let id = provider.id;
    let blocked = provider.state == ProviderState::Blocked;

    let mut set_blocked = move |target: ProviderState| {
        if !busy.claim() {
            return;
        }
        outcome.set(None);
        let client = api.client();
        spawn(async move {
            match client
                .set_provider_state()
                .id(id)
                .body(SetProviderStateBody { state: target })
                .send()
                .await
            {
                Ok(_) => {
                    confirming_block.set(false);
                    reload.bump();
                }
                Err(e) => outcome.set(Some(Err(api::friendly_error(i18n, e)))),
            }
            busy.release();
        });
    };

    let delete = move |()| {
        if !busy.claim() {
            return;
        }
        outcome.set(None);
        let client = api.client();
        spawn(async move {
            match client.delete_provider().id(id).send().await {
                Ok(_) => {
                    on_deleted.call(());
                    reload.bump();
                }
                Err(e) => outcome.set(Some(Err(api::friendly_error(i18n, e)))),
            }
            busy.release();
        });
    };

    // The blast radius, in the numbers the operator can check against the Coverage tab.
    let radius = i18n.args(
        "console.providers.deleteRadius",
        &[
            (
                "sources",
                &stat
                    .as_ref()
                    .map_or_else(|| "—".to_owned(), |s| thousands(s.source_count)),
            ),
            (
                "chapters",
                &stat
                    .as_ref()
                    .map_or_else(|| "—".to_owned(), |s| thousands(s.chapter_count)),
            ),
        ],
    );

    rsx! {
        Section { label: i18n.t("console.providers.tab.danger"),
            div { class: "ik-danger",
                if can_change_state {
                    if *confirming_block.read() {
                        InlineConfirm {
                            title: i18n.t("console.providers.blocklist"),
                            body: i18n.t("console.providers.blocklistWhy"),
                            cta: i18n.t("console.providers.blocklistCta"),
                            busy: busy.is_busy(),
                            on_cancel: move |()| confirming_block.set(false),
                            on_confirm: move |()| set_blocked(ProviderState::Blocked),
                        }
                    } else {
                        div { class: "ik-flex", style: "padding:10px 12px;gap:10px;",
                            div { style: "min-width:0;",
                                div { class: "ttl", {i18n.t("console.providers.blocklist")} }
                                div { class: "why", {i18n.t("console.providers.blocklistWhy")} }
                            }
                            button {
                                class: "ik-btn xs",
                                style: "margin-left:auto;flex:none;",
                                disabled: busy.is_busy(),
                                onclick: move |_| {
                                    if blocked {
                                        set_blocked(ProviderState::Active);
                                    } else {
                                        confirming_block.set(true);
                                    }
                                },
                                if blocked {
                                    {i18n.t("console.providers.unblock")}
                                } else {
                                    {i18n.t("console.providers.blocklistCta")}
                                }
                            }
                        }
                    }
                }
                if can_delete {
                    TypeToConfirm {
                        title: i18n.t("console.providers.delete"),
                        body: radius,
                        expect: provider.slug.clone(),
                        cta: i18n.t("console.providers.deleteCta"),
                        busy: busy.is_busy(),
                        on_confirm: delete,
                    }
                }
            }
            OutcomeLine { outcome: outcome.read().clone() }
        }
    }
}

/// Register a provider. Politeness is left at the polite server defaults and tuned afterwards
/// from the provider's own inspector.
#[component]
pub(super) fn CreateProviderForm(reload: Reload, on_done: EventHandler<()>) -> Element {
    let api = api::use_api();
    let i18n = use_i18n();
    let busy = use_busy();
    let mut outcome = use_outcome();
    let mut slug = use_signal(String::new);
    let mut name = use_signal(String::new);
    let mut base_url = use_signal(String::new);
    let mut adapter = use_signal(|| "generic_config".to_owned());
    let mut config = use_signal(|| "{}".to_owned());

    let submit = move |_| {
        if !busy.claim() {
            return;
        }
        outcome.set(None);
        let parsed = match serde_json::from_str::<serde_json::Value>(&config.peek()) {
            Ok(value) => value,
            Err(e) => {
                outcome.set(Some(Err(i18n.args(
                    "console.providers.badConfig",
                    &[("message", &e.to_string())],
                ))));
                busy.release();
                return;
            }
        };
        let (s, n, b) = (
            slug.peek().trim().to_owned(),
            name.peek().trim().to_owned(),
            base_url.peek().trim().to_owned(),
        );
        if s.is_empty() || n.is_empty() || b.is_empty() {
            outcome.set(Some(Err(i18n.t("console.providers.missingFields"))));
            busy.release();
            return;
        }
        let kind = match adapter.peek().as_str() {
            "madara" => AdapterKind::Madara,
            "generic_config" => AdapterKind::GenericConfig,
            _ => AdapterKind::Custom,
        };
        let client = api.client();
        spawn(async move {
            let body = CreateProvider {
                slug: s,
                name: n,
                base_url: b,
                adapter: kind,
                config: Some(parsed),
                politeness: None,
            };
            match client.create_provider().body(body).send().await {
                Ok(_) => {
                    slug.set(String::new());
                    name.set(String::new());
                    base_url.set(String::new());
                    config.set("{}".to_owned());
                    reload.bump();
                    on_done.call(());
                }
                Err(e) => outcome.set(Some(Err(api::friendly_error(i18n, e)))),
            }
            busy.release();
        });
    };

    rsx! {
        div { style: "max-width:620px;",
            h2 { class: "ik-insp-title", style: "margin-bottom:16px;",
                {i18n.t("console.providers.add")}
            }
            div { style: "display:flex;flex-direction:column;gap:10px;",
                div { class: "ik-kv",
                    label { class: "k", r#for: "tv-new-slug", {i18n.t("console.providers.field.slug")} }
                    input {
                        id: "tv-new-slug",
                        class: "ik-input ik-mono",
                        placeholder: "acme-scans",
                        value: "{slug}",
                        oninput: move |e| slug.set(e.value()),
                    }
                }
                div { class: "ik-kv",
                    label { class: "k", r#for: "tv-new-name", {i18n.t("console.providers.field.name")} }
                    input {
                        id: "tv-new-name",
                        class: "ik-input",
                        placeholder: "Acme Scans",
                        value: "{name}",
                        oninput: move |e| name.set(e.value()),
                    }
                }
                div { class: "ik-kv",
                    label { class: "k", r#for: "tv-new-base", {i18n.t("console.providers.field.baseUrl")} }
                    input {
                        id: "tv-new-base",
                        class: "ik-input ik-mono",
                        placeholder: "https://acmescans.example",
                        value: "{base_url}",
                        oninput: move |e| base_url.set(e.value()),
                    }
                }
                div { class: "ik-kv",
                    label { class: "k", r#for: "tv-new-adapter", {i18n.t("console.providers.field.adapter")} }
                    select {
                        id: "tv-new-adapter",
                        class: "ik-select",
                        value: "{adapter}",
                        onchange: move |e| adapter.set(e.value()),
                        for (token , label_key) in ADAPTER_KINDS.iter().copied() {
                            option { key: "{token}", value: "{token}", {i18n.t(label_key)} }
                        }
                    }
                }
                div {
                    div { class: "ik-sec-lbl", style: "margin-bottom:8px;",
                        {i18n.t("console.providers.adapterConfig")}
                    }
                    textarea {
                        class: "ik-jsonblock",
                        spellcheck: "false",
                        "aria-label": i18n.t("console.providers.adapterConfig"),
                        value: "{config}",
                        oninput: move |e| config.set(e.value()),
                    }
                }
                OutcomeLine { outcome: outcome.read().clone() }
                div { class: "ik-flex", style: "gap:8px;",
                    button {
                        class: "ik-btn sm primary",
                        disabled: busy.is_busy(),
                        onclick: submit,
                        {i18n.t("console.providers.create")}
                    }
                    button {
                        class: "ik-btn sm",
                        onclick: move |_| on_done.call(()),
                        {i18n.t("common.cancel")}
                    }
                }
            }
        }
    }
}

/// Live adapter dry-run against the provider's site, as the standalone Adapter test surface
/// uses it. Runs on demand only; shows the raw parsed sample so operators can validate
/// selectors without a deploy.
#[component]
pub(super) fn AdapterTestPanel(provider_id: ProviderId) -> Element {
    let api = api::use_api();
    let i18n = use_i18n();
    let mut path = use_signal(String::new);
    let busy = use_busy();
    let mut result: Signal<Option<Result<serde_json::Value, String>>> = use_signal(|| None);

    let run = move |_| {
        if !busy.claim() {
            return;
        }
        let p = path.peek().trim().to_owned();
        let client = api.client();
        spawn(async move {
            let body = TestAdapterRequest {
                path: (!p.is_empty()).then_some(p),
            };
            let out = client
                .test_adapter()
                .id(provider_id)
                .body(TestAdapterBody::Variant1(body))
                .send()
                .await
                .map(ResponseValue::into_inner)
                .map_err(|e| api::friendly_error(i18n, e));
            result.set(Some(out));
            busy.release();
        });
    };

    rsx! {
        div { style: "margin-top:14px;border-top:1px solid var(--border);padding-top:14px;",
            div { class: "ik-flex",
                input {
                    class: "ik-input ik-mono",
                    style: "flex:1;",
                    placeholder: i18n.t("console.adapterTest.pathPlaceholder"),
                    value: "{path}",
                    oninput: move |e| path.set(e.value()),
                }
                button { class: "ik-btn primary", disabled: busy.is_busy(), onclick: run,
                    if busy.is_busy() {
                        {i18n.t("console.adapterTest.running")}
                    } else {
                        {i18n.t("console.adapterTest.run")}
                    }
                }
            }
            DryRunResult { result: result.read().clone() }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::parsed_count;
    use serde_json::json;

    #[test]
    fn a_bare_array_is_counted() {
        assert_eq!(parsed_count(&json!([1, 2, 3])), Some(3));
    }

    #[test]
    fn an_object_with_exactly_one_array_is_counted() {
        assert_eq!(
            parsed_count(&json!({ "ok": true, "series": [1, 2] })),
            Some(2)
        );
    }

    #[test]
    fn an_ambiguous_shape_declines_to_guess() {
        assert_eq!(parsed_count(&json!({ "a": [1], "b": [2] })), None);
        assert_eq!(parsed_count(&json!({ "ok": true })), None);
        assert_eq!(parsed_count(&json!("text")), None);
    }
}
