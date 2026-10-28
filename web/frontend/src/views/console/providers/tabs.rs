//! The inspector's editing tab bodies: the adapter config and the politeness budget.

use dioxus::prelude::*;
use inkstone_ui::{Button, Size};

use super::config::DryRunResult;
use super::inspector::{Inspector, Rights};
use super::politeness::EMULATION_CHOICES;
use super::PresetBanner;
use crate::components::{Section, SliderRow};
use crate::models::*;
use crate::util::thousands;

/// Config: the preset link, the identity fields and the adapter config beside its dry run.
pub(super) fn config(
    inspector: &Inspector,
    provider: &Provider,
    stat: Option<&ProviderStat>,
    retired: bool,
    rights: Rights,
) -> Element {
    let inspector = *inspector;
    let Inspector {
        i18n,
        busy,
        gate,
        mut name,
        mut base_url,
        mut config,
        mut dry_run_passed,
        dry_run,
        mut relinking,
        ..
    } = inspector;
    let id = provider.id;
    let link = provider.preset.clone();
    let stat = stat.cloned();
    let base_changed = *base_url.read() != provider.base_url;
    let config_valid = serde_json::from_str::<serde_json::Value>(&config.read()).is_ok();
    rsx! {
        // Two-up: the dry-run result is the answer to the config beside it, and
        // stacking it under a 168px textarea puts it off screen while you edit.
        div { class: "ik-cons-inspbody two-up",
            div { class: "ik-cons-col",
                if let Some(link) = link.clone() {
                    PresetBanner {
                        link,
                        retired,
                        can_edit: rights.edit,
                        busy: busy.is_busy(),
                        relinking,
                        on_unlock: move |()| gate.attempt(move || inspector.set_lock(id, false)),
                        on_relink: move |()| {
                            relinking.set(false);
                            gate.attempt(move || inspector.set_lock(id, true));
                        },
                    }
                }
                Section { label: i18n.t("console.providers.identity"),
                    div { style: "display:flex;flex-direction:column;gap:9px;",
                        div { class: "ik-kv",
                            label { class: "k", r#for: "tv-p-name", {i18n.t("console.providers.field.name")} }
                            input {
                                id: "tv-p-name",
                                class: "ik-input",
                                style: "font-size:12.5px;padding:9px 11px;",
                                disabled: !rights.edit_owned,
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
                                disabled: !rights.edit_owned,
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
                            style: if config_valid { "font-size:11px;color:var(--muted);" } else { "font-size:11px;color:var(--star);" },
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
                        disabled: !rights.edit_owned,
                        "aria-label": i18n.t("console.providers.adapterConfig"),
                        value: "{config}",
                        oninput: move |e| {
                            config.set(e.value());
                            dry_run_passed.set(false);
                        },
                    }
                    div { class: "ik-flex", style: "gap:7px;margin-top:9px;flex-wrap:wrap;",
                        if rights.test {
                            Button {
                                size: Size::Sm,
                                disabled: busy.is_busy(),
                                on_click: move |_| inspector.run_dry(id),
                                {i18n.t("console.providers.dryRun")}
                            }
                        }
                        Button {
                            size: Size::Sm,
                            disabled: !config_valid || !rights.edit_owned,
                            on_click: move |_| inspector.format_config(),
                            {i18n.t("console.providers.format")}
                        }
                        span { class: "ik-mono", style: "margin-left:auto;align-self:center;font-size:11.5px;color:var(--muted);",
                            {i18n.t("console.providers.saveGate")}
                        }
                    }
                }
            }
            div { class: "ik-cons-col",
                DryRunResult { result: dry_run.read().clone() }
            }
        }
    }
}

/// Politeness: the request budget, the browser emulation and the preset's own budget.
pub(super) fn politeness(
    inspector: &Inspector,
    preset: Option<&PresetDefinition>,
    rights: Rights,
) -> Element {
    let inspector = *inspector;
    let Inspector {
        i18n,
        mut rps,
        mut concurrency,
        mut crawl_delay,
        mut user_agent,
        mut emulation,
        ..
    } = inspector;
    rsx! {
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
                                disabled: !rights.edit,
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
                                disabled: !rights.edit || !emulation.read().is_empty(),
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
                if let Some(preset) = preset.cloned() {
                    Section { label: i18n.t("console.providers.preset.budgetHead"),
                        p { class: "ik-muted", style: "font-size:11.5px;line-height:1.55;margin:0 0 9px;",
                            {i18n.t("console.providers.preset.budgetNote")}
                        }
                        div { class: "ik-mono", style: "font-size:11.5px;color:var(--muted);",
                            {
                                i18n.args(
                                    "console.providers.preset.budgetValues",
                                    &[
                                        ("rps", &format!("{:.1}", preset.politeness.rps.unwrap_or(1.0))),
                                        ("concurrency", &preset.politeness.concurrency.unwrap_or(2).to_string()),
                                        ("delay", &preset.politeness.crawl_delay_ms.unwrap_or(0).to_string()),
                                    ],
                                )
                            }
                        }
                        if rights.edit {
                            Button {
                                size: Size::Sm,
                                style: "margin-top:9px;",
                                on_click: {
                                    let preset = preset.clone();
                                    move |_| inspector.adopt_budget(&preset)
                                },
                                {i18n.t("console.providers.preset.budgetCta")}
                            }
                        }
                    }
                }
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
    }
}
