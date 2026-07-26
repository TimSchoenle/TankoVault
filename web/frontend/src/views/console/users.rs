//! User administration: the searchable directory, and a detail drawer per account with
//! identity edits, suspension, forced sign-out, permission grants and erasure.
//!
//! Deliberately **not** on the shared auto-refresh tick. The directory is a work surface —
//! someone is typing in a search box or holding a permission checklist half-edited — and a
//! background refetch landing mid-edit would discard it. It reloads after its own writes.
//!
//! Every control here appears only when the reader holds the capability behind it, so an
//! operator granted `users.read` alone sees a directory and no buttons. The server checks each
//! call regardless; this only avoids offering work that would be refused.

use crate::api;
use crate::components::{async_view, ErrorLine, OutcomeLine, SkeletonRows};
use crate::hooks::{use_busy, use_outcome, use_reload, Reload};
use crate::i18n::use_i18n;
use crate::icons::{Ic, Icon};
use crate::models::{AccountStatusExt as _, PermissionPresetExt as _};
use crate::state::capabilities::use_capabilities;
use crate::state::use_session;
use crate::util::{iso_date, thousands};
use crate::views::console::RefreshTick;
use crate::wire::types::{
    AccountStatus, AdminProfileUpdate, DeleteUser, DirectoryRow, GrantRow, Permission,
    PermissionCatalogue, PermissionGroup, SetPermissions, SetUserStatus, UserDetailResponse,
};
use dioxus::prelude::*;
use progenitor_client::ResponseValue;
use std::collections::BTreeSet;

/// Directory page size. Matches the server's own default; the server clamps regardless.
const PAGE_SIZE: i64 = 25;

#[component]
pub(super) fn UsersPanel(tick: RefreshTick) -> Element {
    let api = api::use_api();
    let i18n = use_i18n();
    let reload = use_reload();
    let mut search = use_signal(String::new);
    let mut page = use_signal(|| 0i64);
    // `None` closes the drawer; `Some(id)` opens it on that account.
    let selected = use_signal(|| Option::<String>::None);

    let directory = use_resource(move || {
        // The tick is tracked so a manual Refresh still reaches this panel, but the pause
        // switch is what an operator uses while editing — see the module docs.
        tick.track();
        reload.track();
        let query = search.read().clone();
        let offset = *page.read() * PAGE_SIZE;
        let client = api.client();
        async move {
            client
                .list_users()
                .search(query)
                .limit(PAGE_SIZE)
                .offset(offset)
                .send()
                .await
                .map(ResponseValue::into_inner)
                .map_err(|e| api::friendly_error(i18n, e))
        }
    });

    rsx! {
        section { style: "margin-bottom:18px;",
            h3 { {i18n.t("console.tab.users")} }

            div { class: "ik-flex", style: "gap:8px;margin-bottom:12px;flex-wrap:wrap;",
                input {
                    class: "ik-input",
                    style: "max-width:320px;",
                    r#type: "search",
                    placeholder: i18n.t("console.users.searchPlaceholder"),
                    value: "{search}",
                    oninput: move |e| {
                        search.set(e.value());
                        // A new query invalidates the current offset; staying on page 3 of the
                        // old result set shows an empty page and reads as "no matches".
                        page.set(0);
                    },
                }
            }

            {
                async_view(
                    &directory,
                    reload,
                    || rsx! { SkeletonRows { count: 4, height: 20 } },
                    |data| {
                        let total = data.total;
                        let shown = i64::try_from(data.users.len()).unwrap_or(i64::MAX);
                        let offset = *page.read() * PAGE_SIZE;
                        rsx! {
                            div { class: "ik-kpis", style: "margin-bottom:14px;",
                                div { class: "ik-kpi",
                                    div { class: "ik-kpi-label", {i18n.t("console.users.registered")} }
                                    div { class: "ik-kpi-value", "{thousands(total)}" }
                                }
                            }
                            if data.users.is_empty() {
                                div { class: "ik-empty", {i18n.t("console.users.empty")} }
                            } else {
                                div { class: "ik-tablewrap",
                                    table { class: "ik-table ik-table-compact",
                                        thead {
                                            tr {
                                                th { {i18n.t("console.users.col.user")} }
                                                th { {i18n.t("console.users.col.email")} }
                                                th { {i18n.t("console.users.col.status")} }
                                                th { style: "text-align:right;", {i18n.t("console.users.col.permissions")} }
                                                th { style: "text-align:right;", {i18n.t("console.users.col.tracked")} }
                                                th { {i18n.t("console.users.col.lastLogin")} }
                                                th { {i18n.t("console.users.col.joined")} }
                                                th {}
                                            }
                                        }
                                        tbody {
                                            for row in data.users.iter().cloned() {
                                                DirectoryRowView { key: "{row.id}", row, selected }
                                            }
                                        }
                                    }
                                }
                                Pager { offset, shown, total, page }
                            }
                        }
                    },
                )
            }

            if let Some(id) = selected.read().clone() {
                UserDrawer { key: "{id}", user_id: id, selected, reload }
            }
        }
    }
}

/// Previous/next paging. Rendered only when there is more than one page to move between.
#[component]
fn Pager(offset: i64, shown: i64, total: i64, page: Signal<i64>) -> Element {
    let i18n = use_i18n();
    let mut page = page;
    let first = offset + 1;
    let last = offset + shown;
    let has_prev = offset > 0;
    let has_next = last < total;

    if !has_prev && !has_next {
        return rsx! {};
    }
    rsx! {
        div { class: "ik-flex", style: "gap:8px;margin-top:10px;align-items:center;",
            button {
                class: "ik-btn",
                disabled: !has_prev,
                onclick: move |_| page -= 1,
                {i18n.t("common.previous")}
            }
            span { class: "ik-muted", style: "font-size:12px;",
                {
                    i18n.args(
                        "console.users.range",
                        &[
                            ("first", &first.to_string()),
                            ("last", &last.to_string()),
                            ("total", &thousands(total)),
                        ],
                    )
                }
            }
            button {
                class: "ik-btn",
                disabled: !has_next,
                onclick: move |_| page += 1,
                {i18n.t("common.next")}
            }
        }
    }
}

/// One directory row. Clicking it opens the detail drawer.
#[component]
fn DirectoryRowView(row: DirectoryRow, selected: Signal<Option<String>>) -> Element {
    let i18n = use_i18n();
    let mut selected = selected;
    let id = row.id.clone();
    let joined = iso_date(Some(&row.created_at)).to_owned();
    let last_login = row.last_login_at.as_deref().map_or_else(
        || i18n.t("console.users.never"),
        |ts| iso_date(Some(ts)).to_owned(),
    );

    rsx! {
        tr {
            td {
                "{row.username}"
                if !row.email_verified {
                    span {
                        class: "ik-pill",
                        style: "margin-left:6px;",
                        title: i18n.t("console.users.unverifiedHint"),
                        {i18n.t("console.users.unverified")}
                    }
                }
            }
            td { class: "ik-mono ik-muted", style: "font-size:12px;", "{row.email}" }
            td { StatusPill { status: row.status } }
            td { class: "ik-mono", style: "text-align:right;", "{row.permission_count}" }
            td { class: "ik-mono", style: "text-align:right;", "{thousands(row.tracked_count)}" }
            td { class: "ik-mono ik-muted", style: "font-size:12px;", "{last_login}" }
            td { class: "ik-mono ik-muted", style: "font-size:12px;", "{joined}" }
            td {
                button {
                    class: "ik-btn",
                    onclick: move |_| selected.set(Some(id.clone())),
                    {i18n.t("console.users.manage")}
                }
            }
        }
    }
}

/// Active/suspended, coloured so a suspended account is impossible to skim past.
#[component]
fn StatusPill(status: AccountStatus) -> Element {
    let i18n = use_i18n();
    rsx! {
        span { class: status.pill_class(), {i18n.t(status.label_key())} }
    }
}

/// The per-account management surface: identity, state, sessions, grants, erasure.
#[component]
fn UserDrawer(user_id: String, selected: Signal<Option<String>>, reload: Reload) -> Element {
    let api = api::use_api();
    let i18n = use_i18n();
    let session = use_session();
    let caps = use_capabilities();
    let detail_reload = use_reload();
    let mut selected = selected;

    let can_write = caps.can(Permission::UsersWrite);
    let can_grant = caps.can(Permission::UsersPermissions);
    let can_sessions = caps.can(Permission::UsersSessions);
    let can_delete = caps.can(Permission::UsersDelete);

    let id_for_fetch = user_id.clone();
    let detail = use_resource(move || {
        detail_reload.track();
        let id = id_for_fetch.clone();
        let client = api.client();
        async move {
            client
                .get_user()
                .id(id)
                .send()
                .await
                .map(ResponseValue::into_inner)
                .map_err(|e| api::friendly_error(i18n, e))
        }
    });

    rsx! {
        div { class: "ik-sidebar-card", style: "margin-top:16px;",
            div { class: "ik-flex", style: "justify-content:space-between;align-items:center;",
                div { class: "ik-flex", style: "gap:8px;",
                    Ic { icon: Icon::Person, size: 18 }
                    strong { {i18n.t("console.users.manageTitle")} }
                }
                button {
                    class: "ik-btn",
                    onclick: move |_| selected.set(None),
                    {i18n.t("common.close")}
                }
            }
            {
                async_view(
                    &detail,
                    detail_reload,
                    || rsx! { crate::components::SkeletonBlock { height: 240 } },
                    |data| {
                        // An administrator acting on their own account is refused by the server;
                        // saying so here explains why the controls are absent instead of leaving
                        // a drawer that looks broken.
                        let is_self = session
                            .username()
                            .is_some_and(|name| name == data.user.username);
                        rsx! {
                            SummaryBlock { data: data.clone() }
                            if is_self {
                                p { class: "ik-muted", style: "font-size:12px;margin-top:12px;",
                                    {i18n.t("console.users.selfNotice")}
                                }
                            }
                            if can_write && !is_self {
                                IdentityForm {
                                    user_id: data.user.id.clone(),
                                    username: data.user.username.clone(),
                                    email: data.user.email.clone(),
                                    reload,
                                    detail_reload,
                                }
                                StatusControls {
                                    user_id: data.user.id.clone(),
                                    status: data.user.status,
                                    email_verified: data.user.email_verified,
                                    reload,
                                    detail_reload,
                                }
                            }
                            if can_sessions {
                                SessionControls {
                                    user_id: data.user.id.clone(),
                                    active_sessions: data.user.active_sessions,
                                    detail_reload,
                                }
                            }
                            if can_grant && !is_self {
                                PermissionEditor {
                                    user_id: data.user.id.clone(),
                                    granted: data.permissions.clone(),
                                    reload,
                                    detail_reload,
                                }
                            }
                            if can_delete && !is_self {
                                DangerZone {
                                    user_id: data.user.id.clone(),
                                    username: data.user.username.clone(),
                                    selected,
                                    reload,
                                }
                            }
                        }
                    },
                )
            }
        }
    }
}

/// The read-only facts about an account, above every control that changes them.
#[component]
fn SummaryBlock(data: UserDetailResponse) -> Element {
    let i18n = use_i18n();
    let user = &data.user;
    let joined = iso_date(Some(&user.created_at)).to_owned();
    let last_login = user.last_login_at.as_deref().map_or_else(
        || i18n.t("console.users.never"),
        |ts| iso_date(Some(ts)).to_owned(),
    );

    rsx! {
        div { style: "margin-top:12px;",
            div { class: "ik-flex", style: "gap:8px;align-items:center;",
                strong { style: "font-size:15px;", "{user.username}" }
                StatusPill { status: user.status }
                if !user.email_verified {
                    span { class: "ik-pill", {i18n.t("console.users.unverified")} }
                }
            }
            div { class: "ik-mono ik-muted", style: "font-size:12px;margin-top:2px;", "{user.email}" }
            if let Some(reason) = user.suspension_reason.clone() {
                p { style: "font-size:12px;margin:6px 0 0;color:var(--vermilion);",
                    {i18n.args("console.users.suspendedFor", &[("reason", &reason)])}
                }
            }
            div { class: "ik-kpis", style: "margin-top:12px;",
                Stat { label: i18n.t("console.users.stat.sessions"), value: user.active_sessions }
                Stat { label: i18n.t("console.users.stat.tracked"), value: user.tracked_count }
                Stat { label: i18n.t("console.users.stat.linked"), value: user.linked_accounts }
                Stat { label: i18n.t("console.users.stat.privacy"), value: user.open_privacy_requests }
            }
            div { class: "ik-mono ik-muted", style: "font-size:11px;margin-top:8px;",
                {i18n.args("console.users.joinedOn", &[("date", &joined)])}
                " · "
                {i18n.args("console.users.lastLoginOn", &[("date", &last_login)])}
            }
        }
    }
}

#[component]
fn Stat(label: String, value: i64) -> Element {
    rsx! {
        div { class: "ik-kpi",
            div { class: "ik-kpi-label", "{label}" }
            div { class: "ik-kpi-value", "{thousands(value)}" }
        }
    }
}

/// Rename an account or change its address on the owner's behalf.
#[component]
fn IdentityForm(
    user_id: String,
    username: String,
    email: String,
    reload: Reload,
    detail_reload: Reload,
) -> Element {
    let api = api::use_api();
    let i18n = use_i18n();
    let busy = use_busy();
    let mut outcome = use_outcome();
    let mut name_field = use_signal(|| username.clone());
    let mut email_field = use_signal(|| email.clone());

    let save = move |_| {
        if !busy.claim() {
            return;
        }
        outcome.set(None);
        let id = user_id.clone();
        // Only send what actually changed: the endpoint treats an omitted field as "leave it",
        // and resubmitting an unchanged email would be an audit record of an edit that was not
        // one.
        let body = AdminProfileUpdate {
            username: Some(name_field.peek().trim().to_owned()).filter(|v| *v != username),
            email: Some(email_field.peek().trim().to_owned()).filter(|v| *v != email),
        };
        if body.username.is_none() && body.email.is_none() {
            outcome.set(Some(Err(i18n.t("console.users.nothingToSave"))));
            busy.release();
            return;
        }
        let client = api.client();
        spawn(async move {
            match client.update_user().id(id).body(body).send().await {
                Ok(_) => {
                    outcome.set(Some(Ok(i18n.t("console.users.saved"))));
                    detail_reload.bump();
                    reload.bump();
                }
                Err(e) => outcome.set(Some(Err(api::friendly_error(i18n, e)))),
            }
            busy.release();
        });
    };

    rsx! {
        div { class: "ik-subhead", style: "margin-top:18px;", {i18n.t("console.users.identity")} }
        div { class: "ik-field",
            label { r#for: "tv-user-name", {i18n.t("account.profile.displayName")} }
            input {
                id: "tv-user-name",
                class: "ik-input",
                value: "{name_field}",
                oninput: move |e| name_field.set(e.value()),
            }
        }
        div { class: "ik-field",
            label { r#for: "tv-user-email", {i18n.t("auth.field.email")} }
            input {
                id: "tv-user-email",
                class: "ik-input",
                r#type: "email",
                value: "{email_field}",
                oninput: move |e| email_field.set(e.value()),
            }
        }
        OutcomeLine { outcome: outcome.read().clone() }
        button {
            class: "ik-btn primary",
            style: "margin-top:8px;",
            disabled: busy.is_busy(),
            onclick: save,
            {i18n.t("console.users.save")}
        }
    }
}

/// Suspend, reinstate, or confirm an address administratively.
#[component]
fn StatusControls(
    user_id: String,
    status: AccountStatus,
    email_verified: bool,
    reload: Reload,
    detail_reload: Reload,
) -> Element {
    let api = api::use_api();
    let i18n = use_i18n();
    let busy = use_busy();
    let mut outcome = use_outcome();
    let mut reason = use_signal(String::new);

    let suspended = status == AccountStatus::Suspended;
    let id_status = user_id.clone();
    let toggle = move |_| {
        if !busy.claim() {
            return;
        }
        outcome.set(None);
        let id = id_status.clone();
        let body = SetUserStatus {
            status: if suspended {
                AccountStatus::Active
            } else {
                AccountStatus::Suspended
            },
            reason: {
                let text = reason.peek().trim().to_owned();
                (!text.is_empty()).then_some(text)
            },
            // A suspension that leaves the account working until its access token expires is
            // not what anyone means by suspending it.
            revoke_sessions: Some(true),
        };
        let client = api.client();
        spawn(async move {
            match client.set_user_status().id(id).body(body).send().await {
                Ok(_) => {
                    reason.set(String::new());
                    detail_reload.bump();
                    reload.bump();
                }
                Err(e) => outcome.set(Some(Err(api::friendly_error(i18n, e)))),
            }
            busy.release();
        });
    };

    let id_verify = user_id.clone();
    let verify = move |_| {
        if !busy.claim() {
            return;
        }
        outcome.set(None);
        let id = id_verify.clone();
        let client = api.client();
        spawn(async move {
            match client.verify_user_email().id(id).send().await {
                Ok(_) => {
                    detail_reload.bump();
                    reload.bump();
                }
                Err(e) => outcome.set(Some(Err(api::friendly_error(i18n, e)))),
            }
            busy.release();
        });
    };

    rsx! {
        div { class: "ik-subhead", style: "margin-top:18px;", {i18n.t("console.users.access")} }
        if !suspended {
            div { class: "ik-field",
                label { r#for: "tv-user-reason", {i18n.t("console.users.suspendReason")} }
                input {
                    id: "tv-user-reason",
                    class: "ik-input",
                    placeholder: i18n.t("console.users.suspendReasonPlaceholder"),
                    value: "{reason}",
                    oninput: move |e| reason.set(e.value()),
                }
            }
        }
        OutcomeLine { outcome: outcome.read().clone() }
        div { class: "ik-flex", style: "gap:8px;margin-top:8px;flex-wrap:wrap;",
            button {
                class: "ik-btn",
                style: if suspended { "" } else { "color:var(--vermilion);" },
                disabled: busy.is_busy(),
                onclick: toggle,
                if suspended {
                    {i18n.t("console.users.reinstate")}
                } else {
                    {i18n.t("console.users.suspend")}
                }
            }
            if !email_verified {
                button { class: "ik-btn", disabled: busy.is_busy(), onclick: verify,
                    {i18n.t("console.users.verifyEmail")}
                }
            }
        }
    }
}

/// Force an account out of every device it is signed in on.
#[component]
fn SessionControls(user_id: String, active_sessions: i64, detail_reload: Reload) -> Element {
    let api = api::use_api();
    let i18n = use_i18n();
    let busy = use_busy();

    let revoke = move |_| {
        if !busy.claim() {
            return;
        }
        let id = user_id.clone();
        let client = api.client();
        spawn(async move {
            let _ = client.revoke_user_sessions().id(id).send().await;
            detail_reload.bump();
            busy.release();
        });
    };

    rsx! {
        div { class: "ik-subhead", style: "margin-top:18px;", {i18n.t("console.users.sessions")} }
        div { class: "ik-flex", style: "gap:8px;align-items:center;",
            span { class: "ik-muted", style: "font-size:13px;",
                {
                    i18n.args(
                        "console.users.activeSessions",
                        &[("count", &active_sessions.to_string())],
                    )
                }
            }
            button {
                class: "ik-btn",
                disabled: busy.is_busy() || active_sessions == 0,
                onclick: revoke,
                {i18n.t("console.users.revokeSessions")}
            }
        }
    }
}

/// The permission checklist, grouped, with the preset bundles as starting points.
///
/// Edits are local until submitted, and the whole set is sent at once — the server replaces it
/// wholesale, so two administrators editing concurrently produce one of their two intents
/// rather than an interleaving of both.
#[component]
fn PermissionEditor(
    user_id: String,
    granted: Vec<GrantRow>,
    reload: Reload,
    detail_reload: Reload,
) -> Element {
    let api = api::use_api();
    let i18n = use_i18n();
    let busy = use_busy();
    let mut outcome = use_outcome();

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

    // Seeded from the server's answer and edited locally from there. Only tokens this build
    // recognises are seeded: an inert grant left over from another version is shown separately
    // below rather than silently re-submitted as if it were current.
    let chosen = use_signal(|| {
        granted
            .iter()
            .filter(|g| g.known)
            .filter_map(|g| {
                serde_json::from_value::<Permission>(serde_json::json!(g.permission)).ok()
            })
            .collect::<BTreeSet<Permission>>()
    });
    let unknown: Vec<String> = granted
        .iter()
        .filter(|g| !g.known)
        .map(|g| g.permission.clone())
        .collect();

    let save = move |_| {
        if !busy.claim() {
            return;
        }
        outcome.set(None);
        let id = user_id.clone();
        let body = SetPermissions {
            permissions: chosen.peek().iter().copied().collect(),
        };
        let client = api.client();
        spawn(async move {
            match client.set_user_permissions().id(id).body(body).send().await {
                Ok(_) => {
                    outcome.set(Some(Ok(i18n.t("console.users.permissionsSaved"))));
                    detail_reload.bump();
                    reload.bump();
                }
                Err(e) => outcome.set(Some(Err(api::friendly_error(i18n, e)))),
            }
            busy.release();
        });
    };

    rsx! {
        div { class: "ik-subhead", style: "margin-top:18px;", {i18n.t("console.users.permissions")} }
        p { class: "ik-muted", style: "font-size:12px;margin-top:0;max-width:70ch;",
            {i18n.t("console.users.permissionsIntro")}
        }
        if !unknown.is_empty() {
            ErrorLine {
                message: i18n.args(
                    "console.users.unknownGrants",
                    &[("tokens", &unknown.join(", "))],
                ),
            }
        }
        {
            match &*catalogue.read_unchecked() {
                None => rsx! { crate::components::SkeletonBlock { height: 160 } },
                Some(Err(message)) => rsx! { ErrorLine { message: message.clone() } },
                Some(Ok(cat)) => rsx! {
                    PresetRow { catalogue: cat.clone(), chosen }
                    for (group, title_key) in PERMISSION_GROUPS {
                        PermissionGroupBlock {
                            key: "{title_key}",
                            title: i18n.t(title_key),
                            entries: cat
                                .permissions
                                .iter()
                                .filter(|p| p.group == group)
                                .cloned()
                                .collect::<Vec<_>>(),
                            chosen,
                        }
                    }
                },
            }
        }
        OutcomeLine { outcome: outcome.read().clone() }
        button {
            class: "ik-btn primary",
            style: "margin-top:10px;",
            disabled: busy.is_busy(),
            onclick: save,
            {i18n.t("console.users.savePermissions")}
        }
    }
}

/// The permission groups in display order, each with the catalogue key that titles it.
const PERMISSION_GROUPS: [(PermissionGroup, &str); 8] = [
    (PermissionGroup::Providers, "console.perm.group.providers"),
    (PermissionGroup::Scanning, "console.perm.group.scanning"),
    (PermissionGroup::Catalogue, "console.perm.group.catalogue"),
    (PermissionGroup::Sync, "console.perm.group.sync"),
    (PermissionGroup::Users, "console.perm.group.users"),
    (PermissionGroup::Privacy, "console.perm.group.privacy"),
    (
        PermissionGroup::Observability,
        "console.perm.group.observability",
    ),
    (PermissionGroup::Flags, "console.perm.group.flags"),
];

/// The preset bundles, applied as a starting point the operator then edits.
///
/// Applying one *replaces* the current selection rather than adding to it, which is what makes
/// it a starting point rather than a cumulative grant — and why it is safe that presets are
/// never stored: what gets saved is whatever is ticked afterwards.
#[component]
fn PresetRow(catalogue: PermissionCatalogue, chosen: Signal<BTreeSet<Permission>>) -> Element {
    let i18n = use_i18n();
    let mut chosen = chosen;
    rsx! {
        div { class: "ik-flex", style: "gap:6px;margin:8px 0;flex-wrap:wrap;align-items:center;",
            span { class: "ik-muted", style: "font-size:12px;", {i18n.t("console.users.presets")} }
            for preset in catalogue.presets.iter().cloned() {
                button {
                    key: "{preset.key}",
                    class: "ik-btn",
                    onclick: move |_| chosen.set(preset.permissions.iter().copied().collect()),
                    {i18n.t(preset.key.label_key())}
                }
            }
        }
    }
}

/// One group of permission checkboxes.
#[component]
fn PermissionGroupBlock(
    title: String,
    entries: Vec<crate::wire::types::PermissionInfo>,
    chosen: Signal<BTreeSet<Permission>>,
) -> Element {
    if entries.is_empty() {
        return rsx! {};
    }
    rsx! {
        div { style: "margin-top:10px;",
            div { class: "ik-muted", style: "font-size:12px;font-weight:600;", "{title}" }
            for entry in entries {
                PermissionCheckbox { key: "{entry.key}", entry, chosen }
            }
        }
    }
}

/// One permission, with the description that says what granting it actually allows.
#[component]
fn PermissionCheckbox(
    entry: crate::wire::types::PermissionInfo,
    chosen: Signal<BTreeSet<Permission>>,
) -> Element {
    let mut chosen = chosen;
    let key = entry.key;
    let checked = chosen.read().contains(&key);
    let token = key.to_string();

    rsx! {
        label {
            class: "ik-flex",
            style: "gap:8px;align-items:flex-start;padding:4px 0;",
            input {
                r#type: "checkbox",
                style: "margin-top:3px;",
                checked,
                onchange: move |e| {
                    let mut set = chosen.write();
                    if e.checked() {
                        set.insert(key);
                    } else {
                        set.remove(&key);
                    }
                },
            }
            span {
                span { class: "ik-mono", style: "font-size:12px;", "{token}" }
                span { class: "ik-muted", style: "font-size:12px;display:block;",
                    "{entry.description}"
                }
            }
        }
    }
}

/// Erase the account. Behind an explicit arming step and a typed confirmation, because it
/// cascades across every table and cannot be undone.
#[component]
fn DangerZone(
    user_id: String,
    username: String,
    selected: Signal<Option<String>>,
    reload: Reload,
) -> Element {
    let api = api::use_api();
    let i18n = use_i18n();
    let busy = use_busy();
    let mut outcome = use_outcome();
    let mut armed = use_signal(|| false);
    let mut typed = use_signal(String::new);
    let mut reason = use_signal(String::new);
    let mut selected = selected;

    let matches_username = typed.read().trim() == username;

    let delete = move |_| {
        if !busy.claim() {
            return;
        }
        outcome.set(None);
        let id = user_id.clone();
        let body = DeleteUser {
            confirm_username: typed.peek().trim().to_owned(),
            reason: {
                let text = reason.peek().trim().to_owned();
                (!text.is_empty()).then_some(text)
            },
        };
        let client = api.client();
        spawn(async move {
            match client.delete_user().id(id).body(body).send().await {
                Ok(_) => {
                    // The account is gone, so the drawer has nothing left to show.
                    selected.set(None);
                    reload.bump();
                }
                Err(e) => outcome.set(Some(Err(api::friendly_error(i18n, e)))),
            }
            busy.release();
        });
    };

    rsx! {
        div { class: "ik-subhead", style: "margin-top:18px;color:var(--vermilion);",
            {i18n.t("console.users.dangerZone")}
        }
        p { class: "ik-muted", style: "font-size:12px;margin-top:0;max-width:70ch;",
            {i18n.t("console.users.deleteIntro")}
        }
        if *armed.read() {
            div { class: "ik-field",
                label { r#for: "tv-user-delete-reason", {i18n.t("console.users.deleteReason")} }
                input {
                    id: "tv-user-delete-reason",
                    class: "ik-input",
                    value: "{reason}",
                    oninput: move |e| reason.set(e.value()),
                }
            }
            div { class: "ik-field",
                label { r#for: "tv-user-delete-confirm",
                    {i18n.args("console.users.deleteConfirmLabel", &[("username", &username)])}
                }
                input {
                    id: "tv-user-delete-confirm",
                    class: "ik-input",
                    autocomplete: "off",
                    value: "{typed}",
                    oninput: move |e| typed.set(e.value()),
                }
            }
            OutcomeLine { outcome: outcome.read().clone() }
            div { class: "ik-flex", style: "gap:8px;margin-top:8px;",
                button {
                    class: "ik-btn",
                    style: "color:var(--vermilion);",
                    disabled: busy.is_busy() || !matches_username,
                    onclick: delete,
                    Ic { icon: Icon::Delete, size: 14 }
                    span { {i18n.t("console.users.deleteConfirmCta")} }
                }
                button {
                    class: "ik-btn",
                    onclick: move |_| {
                        armed.set(false);
                        typed.set(String::new());
                    },
                    {i18n.t("common.cancel")}
                }
            }
        } else {
            button {
                class: "ik-btn",
                style: "color:var(--vermilion);",
                onclick: move |_| armed.set(true),
                {i18n.t("console.users.delete")}
            }
        }
    }
}
