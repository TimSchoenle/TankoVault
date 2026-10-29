//! Owns Console · Providers: the provider list and inspector.
//!
//! Saving a config edit is gated on a passing dry-run; name and base-URL edits are not, since
//! they cannot break parsing.

mod config;
mod coverage;
mod create;
mod danger;
mod inspector;
mod politeness;
mod row;
mod runs;
mod tabs;
mod test;

pub(in crate::views::console) use test::AdapterTestPanel;

use crate::api;
use crate::components::{
    async_view, HealthPill, InlineConfirm, ListFooter, ListSearch, NoSelection, OutcomeLine,
    SkeletonBlock, StepUpGuard, TabBar, TabKind,
};
use crate::hooks::{use_reload, Reload};
use crate::i18n::use_i18n;
use crate::icons::{Ic, Icon};
use crate::models::*;
use crate::state::capabilities::use_capabilities;
use crate::util::{monogram, rel_time, thousands};
use crate::views::console::{config_editor_text, landing_selection, use_console_nav};
use crate::wire::types::Permission;
use coverage::CoverageTab;
use create::{CloneSeed, CreateProviderForm};
use danger::DangerTab;
use dioxus::prelude::*;
use inkstone_ui::{Button, Pill, Size, Tone};
use inspector::{Inspector, Rights};
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

    /// An unrecognised token opens the default tab instead of refusing the link.
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
    // Set together with `creating`: which provider the registration form was opened as a copy
    // of, or `None` for a blank one.
    let mut clone_seed: Signal<Option<CloneSeed>> = use_signal(|| None);

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

    // The preset catalogue the last install run recorded. Loaded once for the whole pane: the
    // inspector needs the shipped definition to show what re-linking would restore, and the
    // list needs nothing more than each row's own link.
    let presets = use_resource(move || {
        reload.track();
        let client = api.client();
        async move {
            client
                .list_provider_presets()
                .send()
                .await
                .map(ResponseValue::into_inner)
                .unwrap_or_default()
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
    // that is filtered out falls back too, instead of showing an inspector with no lit row.
    let current = view
        .sel
        .as_deref()
        .and_then(|id| rows.iter().find(|p| p.id.to_string() == id).map(|p| p.id))
        .or_else(|| rows.first().map(|p| p.id));
    let chosen = current.and_then(|id| rows.iter().find(|p| p.id == id).cloned());

    // …and the fallback goes into the URL, so the address names the provider on screen,
    // not whichever one sorts first under the filter that happens to be applied. It replaces,
    // not pushes (the operator did not choose this row and must not have to back out
    // of it), and the next query is built out here, not inside the effect, because
    // reading `nav.query()` in there would subscribe the effect to the memo the write changes.
    let landing = landing_selection(view.sel.as_deref(), current.map(|id| id.to_string()))
        .map(|sel| view.with_selection(Some(sel)));
    use_effect(use_reactive!(|landing| {
        if let Some(next) = landing {
            nav.filter(next);
        }
    }));

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
                    Button {
                        size: Size::Xs,
                        style: "align-self:flex-start;",
                        on_click: move |_| {
                            let next = !*creating.read();
                            clone_seed.set(None);
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
                    seed: clone_seed.read().clone(),
                    on_done: move |()| {
                        clone_seed.set(None);
                        creating.set(false);
                    },
                }
            }
        } else if let Some(provider) = chosen {
            ProviderInspector {
                key: "{provider.id}",
                provider: provider.clone(),
                stat: stat_rows.iter().find(|s| s.slug == provider.slug).cloned(),
                preset: preset_for(presets.read().as_ref(), &provider),
                reload,
                on_clone: move |seed: CloneSeed| {
                    clone_seed.set(Some(seed));
                    creating.set(true);
                },
                on_deleted: move |()| nav.select(nav.query().with_selection(None)),
            }
        } else {
            NoSelection { message: i18n.t("console.providers.pick") }
        }
    }
}

/// The preset link, stated in the one place an operator is about to edit the fields it governs.
///
/// Three states, and each has exactly one action: locked offers "unlock to edit"; unlocked
/// offers "follow the preset again" behind a confirmation, because that discards local edits;
/// a retired preset offers nothing, and says why.
#[component]
fn PresetBanner(
    link: PresetLink,
    /// The build no longer ships this preset, so there is nothing to follow.
    retired: bool,
    can_edit: bool,
    busy: bool,
    relinking: Signal<bool>,
    on_unlock: EventHandler<()>,
    on_relink: EventHandler<()>,
) -> Element {
    let i18n = use_i18n();
    let mut relinking = relinking;
    let synced = rel_time(i18n, link.synced_at.as_deref());
    let args = [("preset", link.slug.as_str()), ("when", synced.as_str())];

    rsx! {
        // Tinted with the accent while the row follows its preset and with the warning colour
        // once it has stopped: styled inline instead of as a new class, so this needs no
        // Tailwind rebuild of the shipped stylesheet.
        div {
            style: if link.locked {
                "margin-bottom:14px;padding:11px 13px;border-radius:var(--radius);border:1px solid var(--border-soft);background:color-mix(in srgb, var(--acc) 6%, transparent);"
            } else {
                "margin-bottom:14px;padding:11px 13px;border-radius:var(--radius);border:1px solid var(--border-soft);background:color-mix(in srgb, var(--star) 8%, transparent);"
            },
            div { style: "font-weight:600;font-size:12.5px;margin-bottom:3px;",
                if retired {
                {i18n.t("console.providers.preset.retiredHead")}
                } else if link.locked {
                {i18n.t("console.providers.preset.lockedHead")}
                } else {
                {i18n.t("console.providers.preset.customHead")}
                }
            }
            p { class: "ik-muted", style: "font-size:11.5px;line-height:1.55;margin:0;",
                if retired {
                {i18n.args("console.providers.preset.retiredBody", &args)}
                } else if link.locked {
                {i18n.args("console.providers.preset.lockedBody", &args)}
                } else {
                {i18n.args("console.providers.preset.customBody", &args)}
                }
            }
            if can_edit && !retired {
                div { class: "ik-flex", style: "gap:7px;margin-top:9px;flex-wrap:wrap;",
                    if link.locked {
                        Button {
                            size: Size::Sm,
                            disabled: busy,
                            on_click: move |_| on_unlock.call(()),
                            {i18n.t("console.providers.preset.unlockCta")}
                        }
                    } else if *relinking.read() {
                        InlineConfirm {
                            title: i18n.t("console.providers.preset.relinkConfirmHead"),
                            body: i18n.t("console.providers.preset.relinkConfirmBody"),
                            cta: i18n.t("console.providers.preset.relinkCta"),
                            busy,
                            on_cancel: move |()| relinking.set(false),
                            on_confirm: move |()| on_relink.call(()),
                        }
                    } else {
                        Button {
                            size: Size::Sm,
                            disabled: busy,
                            on_click: move |_| relinking.set(true),
                            {i18n.t("console.providers.preset.relinkCta")}
                        }
                    }
                }
            }
        }
    }
}

/// The shipped definition governing `provider`, if it came from a preset this build still
/// ships.
///
/// A miss is meaningful instead of a loading artefact: it is a provider whose preset the
/// build has retired, and the inspector says so instead of offering a re-link it cannot honour.
fn preset_for(
    catalogue: Option<&Vec<PresetDefinition>>,
    provider: &Provider,
) -> Option<PresetDefinition> {
    let link = provider.preset.as_ref()?;
    catalogue?
        .iter()
        .find(|preset| preset.slug == link.slug)
        .cloned()
}

/// The full editor for one provider.
#[component]
fn ProviderInspector(
    provider: Provider,
    stat: Option<ProviderStat>,
    /// The shipped preset this provider follows, when the build still ships it.
    preset: Option<PresetDefinition>,
    reload: Reload,
    on_clone: EventHandler<CloneSeed>,
    on_deleted: EventHandler<()>,
) -> Element {
    let inspector = Inspector::use_new(&provider, reload);
    let Inspector {
        i18n,
        busy,
        outcome,
        gate,
        config,
        dry_run_passed,
        mut scan_mode,
        ..
    } = inspector;
    let rights = Rights::use_for(&provider);
    let nav = use_console_nav();
    let tab = Tab::parse(nav.query().tab_token());

    let id = provider.id;
    let is_disabled = provider.state == ProviderState::Disabled;
    let link = provider.preset.clone();
    let retired_preset = link.is_some() && preset.is_none();
    // Saving is blocked while an unproven config edit is pending; everything else is free.
    let save_blocked =
        *config.read() != config_editor_text(&provider.config) && !*dry_run_passed.read();

    let save = {
        let original_base = provider.base_url.clone();
        move |_| {
            let original_base = original_base.clone();
            gate.attempt(move || inspector.save(id, &original_base));
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
                            if let Some(link) = link.clone() {
                                Pill {
                                    tone: if link.locked { Tone::Accent } else { Tone::Caution },
                                    title: i18n.args(
                                        if link.locked {
                                            "console.providers.preset.pillLockedTitle"
                                        } else {
                                            "console.providers.preset.pillCustomTitle"
                                        },
                                        &[("preset", &link.slug)],
                                    ),
                                    if link.locked {
                                    {i18n.t("console.providers.preset.pillLocked")}
                                    } else {
                                    {i18n.t("console.providers.preset.pillCustom")}
                                    }
                                }
                            }
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
                        if rights.scan {
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
                            Button {
                                size: Size::Sm,
                                on_click: move |_| gate.attempt(move || inspector.scan(id)),
                                {i18n.t("console.providers.scanNow")}
                            }
                        }
                        if rights.change_state {
                            Button {
                                size: Size::Sm,
                                disabled: busy.is_busy(),
                                on_click: move |_| {
                                    gate.attempt(move || {
                                        inspector.set_state(id, if is_disabled { ProviderState::Active } else { ProviderState::Disabled });
                                    });
                                },
                                if is_disabled {
                                {i18n.t("console.providers.enable")}
                                } else {
                                {i18n.t("console.providers.pause")}
                                }
                            }
                        }
                        if rights.create {
                            Button {
                                size: Size::Sm,
                                title: i18n.t("console.providers.cloneHint"),
                                on_click: {
                                    let provider = provider.clone();
                                    move |_| {
                                        on_clone
                                            .call(CloneSeed {
                                                slug: format!("{}-copy", provider.slug),
                                                name: i18n
                                                    .args(
                                                        "console.providers.cloneName",
                                                        &[("name", &provider.name)],
                                                    ),
                                                base_url: provider.base_url.clone(),
                                                adapter: provider.adapter.to_string(),
                                                config: config_editor_text(&provider.config),
                                            });
                                    }
                                },
                                {i18n.t("console.providers.clone")}
                            }
                        }
                        if rights.edit {
                            Button {
                                size: Size::Sm,
                                tone: Tone::Primary,
                                disabled: busy.is_busy() || save_blocked,
                                title: if save_blocked { i18n.t("console.providers.saveGate") } else { String::new() },
                                on_click: save,
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
                StepUpGuard { gate, intro: Some(i18n.t("console.stepUp.intro")) }
                OutcomeLine { outcome: outcome.read().clone() }
            }
            match tab {
                Tab::Config => tabs::config(&inspector, &provider, stat.as_ref(), retired_preset, rights),
                Tab::Politeness => tabs::politeness(&inspector, preset.as_ref(), rights),
                Tab::Coverage => rsx! {
                    div { class: "ik-cons-inspbody",
                        div { class: "ik-cons-col",
                            CoverageTab { stat: stat.clone() }
                        }
                    }
                },
                Tab::Runs => rsx! {
                    div { class: "ik-cons-inspbody",
                        div { class: "ik-cons-col",
                            RunsTab { provider_id: id }
                        }
                    }
                },
                Tab::Danger => rsx! {
                    div { class: "ik-cons-inspbody",
                        div { class: "ik-cons-col", style: "max-width:620px;",
                            DangerTab {
                                provider: provider.clone(),
                                stat: stat.clone(),
                                can_change_state: rights.change_state,
                                can_delete: rights.delete,
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
