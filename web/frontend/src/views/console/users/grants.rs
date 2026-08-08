//! The permission checklist: groups, provenance, presets and the tick that changes a grant.

use crate::api;
use crate::components::{ErrorLine, Section, SkeletonBlock};
use crate::i18n::use_i18n;
use crate::models::PermissionPresetExt as _;
use crate::util::rel_time;
use crate::wire::types::{
    GrantRow, Permission, PermissionCatalogue, PermissionGroup, PermissionInfo,
};
use dioxus::prelude::*;
use progenitor_client::ResponseValue;
use std::collections::BTreeSet;

/// The permission checklist, grouped, with provenance and the preset bundles as starting
/// points.
///
/// The whole set is submitted at once on Save; the server replaces it wholesale, so concurrent
/// edits by two administrators produce one intent rather than an interleaving of both.
#[component]
pub(super) fn PermissionGrants(
    grants: Vec<GrantRow>,
    chosen: Signal<BTreeSet<Permission>>,
    editable: bool,
    /// Whether this account holds the super-user grant. The checklist cannot show it — the
    /// catalogue deliberately omits it — so an unannotated checklist would render the deployment
    /// owner as holding nothing at all.
    owner: bool,
) -> Element {
    let api = api::use_api();
    let i18n = use_i18n();
    let catalogue = use_resource(move || {
        let client = api.client();
        async move {
            client
                .permission_catalogue()
                .send()
                .await
                .map(ResponseValue::into_inner)
                .map_err(|e| api::friendly_error(i18n, e))
        }
    });

    let unknown: Vec<String> = grants
        .iter()
        .filter(|g| !g.known)
        .map(|g| g.permission.clone())
        .collect();
    let loaded = catalogue.read_unchecked().clone();

    rsx! {
        Section {
            label: i18n.t("console.users.permissions"),
            trailing: match &loaded {
                Some(Ok(cat)) if editable => rsx! {
                    PresetPicker { catalogue: cat.clone(), chosen }
                },
                _ => rsx! {},
            },
            if owner {
                p {
                    class: "ik-note star",
                    style: "font-size:11.5px;line-height:1.5;margin:0 0 9px;",
                    {i18n.t("console.users.superUserNotice")}
                }
            }
            if !unknown.is_empty() {
                ErrorLine {
                    message: i18n.args("console.users.unknownGrants", &[("tokens", &unknown.join(", "))]),
                }
            }
            match loaded {
                None => rsx! { SkeletonBlock { height: 180 } },
                Some(Err(message)) => rsx! { ErrorLine { message } },
                Some(Ok(cat)) => rsx! {
                    div { class: "ik-listbox",
                        for (group , title_key) in PERMISSION_GROUPS {
                            GrantGroup {
                                key: "{title_key}",
                                title: i18n.t(title_key),
                                entries: cat
                                    .permissions
                                    .iter()
                                    .filter(|p| p.group == group)
                                    .cloned()
                                    .collect::<Vec<PermissionInfo>>(),
                                grants: grants.clone(),
                                chosen,
                                editable,
                            }
                        }
                    }
                },
            }
            p { class: "ik-muted", style: "font-size:11.5px;line-height:1.5;margin:8px 0 0;",
                {i18n.t("console.users.grantLifetime")}
            }
        }
    }
}

/// The preset bundles, applied as a starting point the operator then edits.
///
/// Applying one replaces the current selection rather than adding to it; presets are never
/// stored, since what gets saved is whatever is ticked afterwards.
#[component]
pub(super) fn PresetPicker(
    catalogue: PermissionCatalogue,
    chosen: Signal<BTreeSet<Permission>>,
) -> Element {
    let i18n = use_i18n();
    let mut chosen = chosen;
    rsx! {
        select {
            class: "ik-select",
            style: "font-size:11.5px;padding:5px 8px;",
            "aria-label": i18n.t("console.users.presets"),
            onchange: move |event| {
                let picked = event.value();
                if let Some(preset) = catalogue.presets.iter().find(|p| p.key.to_string() == picked)
                {
                    chosen.set(preset.permissions.iter().copied().collect());
                }
            },
            option { value: "", {i18n.t("console.users.presets")} }
            for preset in catalogue.presets.iter() {
                option { key: "{preset.key}", value: "{preset.key}", {i18n.t(preset.key.label_key())} }
            }
        }
    }
}

/// One permission group: a sub-header and its rows.
#[component]
pub(super) fn GrantGroup(
    title: String,
    entries: Vec<PermissionInfo>,
    grants: Vec<GrantRow>,
    chosen: Signal<BTreeSet<Permission>>,
    editable: bool,
) -> Element {
    if entries.is_empty() {
        return rsx! {};
    }
    rsx! {
        div { class: "ik-grouphead", "{title}" }
        for entry in entries {
            GrantRowView {
                key: "{entry.key}",
                entry: entry.clone(),
                provenance: grants
                    .iter()
                    .find(|g| g.permission == entry.key.to_string())
                    .cloned(),
                chosen,
                editable,
            }
        }
    }
}

/// One permission: the token, who granted it and when, and the tick that changes it.
#[component]
pub(super) fn GrantRowView(
    entry: PermissionInfo,
    provenance: Option<GrantRow>,
    chosen: Signal<BTreeSet<Permission>>,
    editable: bool,
) -> Element {
    let i18n = use_i18n();
    let mut chosen = chosen;
    let key = entry.key;
    let checked = chosen.read().contains(&key);
    let token = key.to_string();

    let by = provenance.and_then(|grant| {
        let who = grant.granted_by?;
        Some(i18n.args(
            "console.users.grantedBy",
            &[
                ("who", &who),
                ("when", &rel_time(i18n, Some(&grant.granted_at))),
            ],
        ))
    });

    rsx! {
        label {
            class: "ik-listrow",
            style: "gap:10px;cursor:pointer;",
            title: "{entry.description}",
            input {
                class: "ik-cbx",
                r#type: "checkbox",
                disabled: !editable,
                checked,
                onchange: move |event| {
                    let mut set = chosen.write();
                    if event.checked() {
                        set.insert(key);
                    } else {
                        set.remove(&key);
                    }
                },
            }
            span {
                class: "ik-mono",
                style: if checked { "font-size:12.5px;color:var(--text);" } else { "font-size:12.5px;color:var(--muted);" },
                "{token}"
            }
            if let Some(by) = by {
                span { style: "margin-left:auto;font-size:11px;color:var(--faint);flex:none;", "{by}" }
            }
        }
    }
}

/// The permission groups in display order, each with the catalogue key that titles it.
pub(super) const PERMISSION_GROUPS: [(PermissionGroup, &str); 8] = [
    (PermissionGroup::Catalogue, "console.perm.group.catalogue"),
    (PermissionGroup::Providers, "console.perm.group.providers"),
    (PermissionGroup::Scanning, "console.perm.group.scanning"),
    (PermissionGroup::Sync, "console.perm.group.sync"),
    (PermissionGroup::Users, "console.perm.group.users"),
    (PermissionGroup::Privacy, "console.perm.group.privacy"),
    (
        PermissionGroup::Observability,
        "console.perm.group.observability",
    ),
    (PermissionGroup::Flags, "console.perm.group.flags"),
];
