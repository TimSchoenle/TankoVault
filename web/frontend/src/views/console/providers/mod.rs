//! Console · Providers — the provider list and inspector.
//!
//! Saving a config edit is gated on a passing dry-run; name and base-URL edits are not, since
//! they cannot break parsing.

mod config;
mod coverage;
mod create;
mod danger;
mod politeness;
mod row;
mod runs;
mod test;

pub(in crate::views::console) use test::AdapterTestPanel;

use crate::api;
use crate::components::{
    async_view, HealthPill, ListFooter, ListSearch, NoSelection, OutcomeLine, Section,
    SkeletonBlock, SliderRow, TabBar, TabKind,
};
use crate::hooks::{use_busy, use_outcome, use_reload, Reload};
use crate::i18n::use_i18n;
use crate::icons::{Ic, Icon};
use crate::models::*;
use crate::state::capabilities::use_capabilities;
use crate::util::{monogram, rel_time, thousands};
use crate::views::console::{config_editor_text, use_console_nav};
use crate::wire::types::Permission;
use config::DryRunResult;
use coverage::CoverageTab;
use create::CreateProviderForm;
use danger::DangerTab;
use dioxus::prelude::*;
use politeness::{emulation_token, politeness_body, EMULATION_CHOICES};
use progenitor_client::ResponseValue;
use row::ProviderRow;
use runs::RunsTab;

/// The inspector's tab strip.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Config,
    Politeness,
    Coverage,
    Runs,
    Danger,
}

impl TabKind for Tab {
    fn all() -> &'static [Self] {
        &[
            Self::Config,
            Self::Politeness,
            Self::Coverage,
            Self::Runs,
            Self::Danger,
        ]
    }

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

impl Tab {
    /// This tab's `?tab=` token.
    fn token(self) -> &'static str {
        match self {
            Self::Config => "config",
            Self::Politeness => "politeness",
            Self::Coverage => "coverage",
            Self::Runs => "runs",
            Self::Danger => "danger",
        }
    }

    /// An unrecognised token opens the default tab rather than refusing the link.
    fn parse(token: &str) -> Self {
        <Self as TabKind>::all()
            .iter()
            .copied()
            .find(|tab| tab.token() == token)
            .unwrap_or(Self::Config)
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
    let nav = use_console_nav();
    let view = nav.query();
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

    // Aggregate endpoint, permission-gated separately: `providers.read` alone still gets a
    // list, just without the meter under each name.
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

    // Memoised: filtering reclones the list on every keystroke and every row selection.
    let needle = view.q.trim().to_lowercase();
    let filtered = use_memo(use_reactive!(|needle| {
        match &*providers.read() {
            Some(Ok(list)) => list
                .iter()
                .filter(|p| {
                    needle.is_empty()
                        || p.slug.to_lowercase().contains(&needle)
                        || p.name.to_lowercase().contains(&needle)
                })
                .cloned()
                .collect::<Vec<Provider>>(),
            _ => Vec::new(),
        }
    }));
    let stat_rows = stats.read().clone().unwrap_or_default();
    let rows = filtered.read().clone();

    // Falls back to the first row so the inspector is never empty. A `sel` naming a provider
    // that is filtered out falls back too, rather than showing an inspector with no lit row.
    let current = view
        .sel
        .as_deref()
        .and_then(|id| rows.iter().find(|p| p.id.to_string() == id).map(|p| p.id))
        .or_else(|| rows.first().map(|p| p.id));
    let chosen = current.and_then(|id| rows.iter().find(|p| p.id == id).cloned());

    rsx! {
        div { class: "ik-cons-list",
            div { class: "ik-cons-listhead",
                ListSearch {
                    placeholder: i18n.t("console.providers.filter"),
                    query: view.q.clone(),
                    on_input: move |text| nav.filter(nav.query().with_search(text)),
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
                                    on_pick: move |id: ProviderId| {
                                        creating.set(false);
                                        nav.select(nav.query().with_selection(Some(id.to_string())));
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
                on_deleted: move |()| nav.select(nav.query().with_selection(None)),
            }
        } else {
            NoSelection { message: i18n.t("console.providers.pick") }
        }
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
    // One control per capability: each appears exactly when the server would accept the call.
    let can_edit = caps.can(Permission::ProvidersWrite);
    let can_change_state = caps.can(Permission::ProvidersState);
    let can_scan = caps.can(Permission::ScansRun);
    let can_test = caps.can(Permission::ProvidersTest);
    let can_delete = caps.can(Permission::ProvidersDelete);

    let id = provider.id;
    let original_base = provider.base_url.clone();
    let original_config = config_editor_text(&provider.config);
    let is_disabled = provider.state == ProviderState::Disabled;

    let nav = use_console_nav();
    let tab = Tab::parse(nav.query().tab_token());
    // Fast scan checks deltas; full re-reads the catalogue for this provider only.
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
    #[expect(
        clippy::cast_precision_loss,
        reason = "crawl delays are milliseconds, well inside f64's exact integer range"
    )]
    let mut crawl_delay = use_signal(|| provider.politeness.crawl_delay_ms.unwrap_or(0) as f64);
    let mut user_agent = use_signal(|| provider.politeness.user_agent.clone().unwrap_or_default());
    let mut emulation = use_signal(|| emulation_token(provider.politeness.emulation.as_ref()));

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
            let politeness = match politeness_body(
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
                politeness: Some(politeness),
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
                            HealthPill { state: Some(provider.state) }
                        }
                        div { class: "ik-meta-line",
                            span { "{provider.adapter}" }
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
                TabBar {
                    selected: tab,
                    on_select: move |next: Tab| nav.filter(nav.query().with_tab(next.token())),
                    flush: true,
                }
            }
            div { style: "padding:0 22px;",
                OutcomeLine { outcome: outcome.read().clone() }
            }
            match tab {
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
                                                "{provider.adapter}"
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
                                            for (profile , label) in EMULATION_CHOICES {
                                                option { key: "{profile}", value: "{profile}", "{label}" }
                                            }
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
