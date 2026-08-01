//! The feature-flag control plane: every switchable capability, grouped, with its current
//! state and who last changed it.
//!
//! Deliberately not on the shared auto-refresh tick: a background refetch landing between a
//! reader deciding to flip a switch and their click would change what they hit. Reloads after
//! its own writes instead.

use crate::api;
use crate::components::async_view;
use crate::hooks::{use_busy, use_reload, Busy, Reload};
use crate::i18n::use_i18n;
use crate::icons::{Ic, Icon};
use crate::state::capabilities::use_capabilities;
use crate::util::iso_date;
use crate::wire::types::{FeatureGroup, FlagView, Permission, SetFlag};
use dioxus::prelude::*;
use progenitor_client::ResponseValue;

/// The groups in display order, each with the catalogue key that titles it.
const GROUPS: [(FeatureGroup, &str); 8] = [
    (FeatureGroup::Catalogue, "console.flags.group.catalogue"),
    (FeatureGroup::Accounts, "console.flags.group.accounts"),
    (FeatureGroup::Privacy, "console.flags.group.privacy"),
    (FeatureGroup::Tracking, "console.flags.group.tracking"),
    (
        FeatureGroup::Notifications,
        "console.flags.group.notifications",
    ),
    (FeatureGroup::Sync, "console.flags.group.sync"),
    (FeatureGroup::Scanning, "console.flags.group.scanning"),
    (FeatureGroup::Operations, "console.flags.group.operations"),
];

#[component]
pub(super) fn FeatureFlagsPanel() -> Element {
    let api = api::use_api();
    let i18n = use_i18n();
    let caps = use_capabilities();
    let reload = use_reload();
    let can_write = caps.can(Permission::FlagsWrite);

    let flags = use_resource(move || {
        reload.track();
        let client = api.client();
        async move {
            client
                .list_flags()
                .send()
                .await
                .map(ResponseValue::into_inner)
                .map_err(|e| api::friendly_error(i18n, e))
        }
    });

    rsx! {
        section { style: "margin-bottom:18px;",
            h3 { {i18n.t("console.tab.flags")} }
            p { class: "ik-muted", style: "font-size:13px;margin-top:0;max-width:70ch;",
                {i18n.t("console.flags.intro")}
            }
            if !can_write {
                p { class: "ik-muted", style: "font-size:12px;",
                    {i18n.t("console.flags.readOnly")}
                }
            }
            {
                async_view(
                    &flags,
                    reload,
                    || rsx! { crate::components::SkeletonBlock { height: 320 } },
                    |rows| rsx! {
                        for (group, title_key) in GROUPS {
                            FlagGroup {
                                key: "{title_key}",
                                title: i18n.t(title_key),
                                flags: rows.iter().filter(|f| f.group == group).cloned().collect::<Vec<_>>(),
                                can_write,
                                reload,
                            }
                        }
                    },
                )
            }
        }
    }
}

/// One group heading and its rows. Rendered even when empty is avoided: a heading over nothing
/// reads as a loading failure.
#[component]
fn FlagGroup(title: String, flags: Vec<FlagView>, can_write: bool, reload: Reload) -> Element {
    if flags.is_empty() {
        return rsx! {};
    }
    rsx! {
        div { class: "ik-subhead", style: "margin-top:18px;", "{title}" }
        div { class: "ik-tablewrap",
            for flag in flags {
                FlagRow { key: "{flag.key}", flag, can_write, reload }
            }
        }
    }
}

/// One feature: its state, what switching it off does, and the controls to change it.
#[component]
fn FlagRow(flag: FlagView, can_write: bool, reload: Reload) -> Element {
    let i18n = use_i18n();
    let busy = use_busy();

    let key = flag.key.to_string();
    let enabled = flag.enabled;
    let toggle_label = if enabled {
        i18n.t("console.flags.disable")
    } else {
        i18n.t("console.flags.enable")
    };

    rsx! {
        div { class: "ik-row", style: "align-items:flex-start;",
            div { class: "grow",
                div { class: "ik-flex", style: "gap:8px;align-items:center;",
                    span {
                        class: if enabled { "ik-pill jade" } else { "ik-pill vermilion" },
                        if enabled {
                            {i18n.t("console.flags.on")}
                        } else {
                            {i18n.t("console.flags.off")}
                        }
                    }
                    strong { style: "font-size:13px;", "{flag.title}" }
                    if flag.locked {
                        span { class: "ik-pill", title: i18n.t("console.flags.lockedHint"),
                            {i18n.t("console.flags.locked")}
                        }
                    }
                    // Show only when it differs from the shipped default: that's what an operator scans for.
                    if flag.overridden && flag.enabled != flag.default_enabled {
                        span { class: "ik-pill acc", {i18n.t("console.flags.changed")} }
                    }
                }
                div { class: "ik-mono ik-muted", style: "font-size:11px;margin-top:2px;", "{key}" }
                p { class: "ik-muted", style: "font-size:12px;margin:6px 0 0;max-width:74ch;",
                    "{flag.description}"
                }
                if let Some(note) = flag.note.clone() {
                    p { style: "font-size:12px;margin:4px 0 0;", "“{note}”" }
                }
                if let Some(by) = flag.updated_by.clone() {
                    div { class: "ik-mono ik-muted", style: "font-size:11px;margin-top:2px;",
                        {
                            let when = iso_date(flag.updated_at.as_deref()).to_owned();
                            i18n.args("console.flags.changedBy", &[("user", &by), ("date", &when)])
                        }
                    }
                }
            }
            if can_write && !flag.locked {
                div { class: "ik-flex", style: "gap:6px;flex-shrink:0;",
                    ToggleButton {
                        feature: key.clone(),
                        enable: !enabled,
                        label: toggle_label,
                        busy,
                        reload,
                    }
                    // Only offered when there's an override to withdraw; otherwise reset would do nothing.
                    if flag.overridden {
                        ResetButton { feature: key.clone(), busy, reload }
                    }
                }
            }
        }
    }
}

/// Switch one feature on or off.
#[component]
fn ToggleButton(
    feature: String,
    enable: bool,
    label: String,
    busy: Busy,
    reload: Reload,
) -> Element {
    let api = api::use_api();
    let click = move |_| {
        if !busy.claim() {
            return;
        }
        let key = feature.clone();
        let client = api.client();
        spawn(async move {
            let _ = client
                .set_flag()
                .key(key)
                .body(SetFlag {
                    enabled: enable,
                    note: None,
                })
                .send()
                .await;
            // Refetch either way: the list is what tells the reader whether it actually changed.
            reload.bump();
            busy.release();
        });
    };
    rsx! {
        button {
            class: if enable { "ik-btn primary" } else { "ik-btn" },
            disabled: busy.is_busy(),
            onclick: click,
            "{label}"
        }
    }
}

/// Withdraw the stored override, returning the feature to its shipped default.
#[component]
fn ResetButton(feature: String, busy: Busy, reload: Reload) -> Element {
    let api = api::use_api();
    let i18n = use_i18n();
    let click = move |_| {
        if !busy.claim() {
            return;
        }
        let key = feature.clone();
        let client = api.client();
        spawn(async move {
            let _ = client.reset_flag().key(key).send().await;
            reload.bump();
            busy.release();
        });
    };
    rsx! {
        button {
            class: "ik-btn",
            disabled: busy.is_busy(),
            title: i18n.t("console.flags.resetHint"),
            onclick: click,
            Ic { icon: Icon::Refresh, size: 14 }
        }
    }
}
