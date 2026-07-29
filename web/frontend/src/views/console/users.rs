//! Console · Users — the account directory and a full inspector per account.
//!
//! Deliberately **off** the shared auto-refresh tick. This is a work surface: someone is
//! holding a permission checklist half-edited or typing in an address field, and a background
//! refetch landing mid-edit would discard it. It reloads after its own writes, and offers a
//! manual reload for everything else.
//!
//! One `Save changes` applies whatever the reader is actually allowed to change — identity,
//! account status, grants — each behind its own capability, so an operator with `users.read`
//! alone gets a directory and no buttons. The server checks every call regardless; this only
//! avoids offering work that would be refused.
//!
//! Two blocks the design draws have no endpoint and are therefore absent rather than stubbed:
//!
//! - *Per-device sessions* (device, location, per-row revoke) — the admin API exposes a live
//!   session **count** and a revoke-all, not the rows.
//!   TODO(api): needs `GET /v1/admin/users/:id/sessions`.
//! - *Export everything* — subject export is reachable only through a filed privacy request
//!   (`GET /v1/admin/privacy/requests/:id/export`), not per account.
//!   TODO(api): needs `GET /v1/admin/users/:id/export`.

use super::shell::{InlineConfirm, ListSearch, NoSelection, Section, TypeToConfirm};
use crate::api;
use crate::components::{async_view, ErrorLine, OutcomeLine, SkeletonBlock};
use crate::hooks::{use_busy, use_outcome, use_reload, Reload};
use crate::i18n::use_i18n;
use crate::icons::{Ic, Icon};
use crate::models::{AccountStatusExt as _, AdminSyncAccount, PermissionPresetExt as _};
use crate::state::capabilities::use_capabilities;
use crate::state::use_session;
use crate::util::{initial, iso_date, monogram, rel_time, thousands};
use crate::wire::types::{
    AccountStatus, AdminProfileUpdate, DeleteUser, DirectoryRow, GrantRow, Permission,
    PermissionCatalogue, PermissionGroup, PermissionInfo, SetPermissions, SetUserStatus,
    SyncAccountTarget, UserDetailResponse, UserId,
};
use dioxus::prelude::*;
use progenitor_client::ResponseValue;
use std::collections::BTreeSet;

/// Directory page size. Matches the server's own default; the server clamps regardless.
const PAGE_SIZE: i64 = 25;

/// The inspector's tab strip.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Identity,
    Sessions,
    Library,
    Privacy,
    Activity,
}

impl Tab {
    const ALL: [Tab; 5] = [
        Self::Identity,
        Self::Sessions,
        Self::Library,
        Self::Privacy,
        Self::Activity,
    ];

    fn label_key(self) -> &'static str {
        match self {
            Self::Identity => "console.users.tab.identity",
            Self::Sessions => "console.users.tab.sessions",
            Self::Library => "console.users.tab.library",
            Self::Privacy => "console.users.tab.privacy",
            Self::Activity => "console.users.tab.activity",
        }
    }
}

/// Which accounts the list is showing.
#[derive(Clone, Copy, PartialEq, Eq)]
enum StatusFilter {
    Any,
    Active,
    Suspended,
}

impl StatusFilter {
    fn next(self) -> Self {
        match self {
            Self::Any => Self::Active,
            Self::Active => Self::Suspended,
            Self::Suspended => Self::Any,
        }
    }

    fn label_key(self) -> &'static str {
        match self {
            Self::Any => "console.users.filter.anyStatus",
            Self::Active => "enum.accountStatus.active",
            Self::Suspended => "enum.accountStatus.suspended",
        }
    }

    fn accepts(self, row: &DirectoryRow) -> bool {
        match self {
            Self::Any => true,
            Self::Active => row.status == AccountStatus::Active,
            Self::Suspended => row.status == AccountStatus::Suspended,
        }
    }
}

/// The list pane and the inspector pane, as the console shell's two grid children.
#[component]
pub(super) fn UsersEntity() -> Element {
    let api = api::use_api();
    let i18n = use_i18n();
    let reload = use_reload();
    let query = use_signal(String::new);
    let mut page = use_signal(|| 0i64);
    let mut staff_only = use_signal(|| false);
    let mut status = use_signal(|| StatusFilter::Any);
    let mut selected = use_signal(|| Option::<String>::None);

    // The search term goes to the server (it matches username *and* email across the whole
    // table); the two chips filter the page that comes back.
    let directory = use_resource(move || {
        reload.track();
        let search = query.read().clone();
        let offset = *page.read() * PAGE_SIZE;
        let client = api.client();
        async move {
            client
                .list_users()
                .search(search)
                .limit(PAGE_SIZE)
                .offset(offset)
                .send()
                .await
                .map(ResponseValue::into_inner)
                .map_err(|e| api::friendly_error(i18n, e))
        }
    });

    let loaded = directory.read_unchecked().clone();
    let (rows, total) = match &loaded {
        Some(Ok(page_data)) => {
            let filtered: Vec<DirectoryRow> = page_data
                .users
                .iter()
                .filter(|row| status.read().accepts(row))
                .filter(|row| !*staff_only.read() || row.permission_count > 0)
                .cloned()
                .collect();
            (filtered, page_data.total)
        }
        _ => (Vec::new(), 0),
    };
    let current = selected
        .read()
        .clone()
        .or_else(|| rows.first().map(|r| r.id.clone()));
    let offset = *page.read() * PAGE_SIZE;
    let has_prev = offset > 0;
    let has_next = offset + i64::try_from(rows.len()).unwrap_or(0) < total;

    rsx! {
        div { class: "ik-cons-list",
            div { class: "ik-cons-listhead",
                ListSearch {
                    placeholder: i18n.t("console.users.searchPlaceholder"),
                    query,
                    hits: i18n.plural(
                        "console.users.hits",
                        i64::try_from(rows.len()).unwrap_or(0),
                        &[],
                    ),
                }
                div { class: "ik-flex", style: "gap:6px;flex-wrap:wrap;",
                    button {
                        class: if *staff_only.read() { "ik-chip active" } else { "ik-chip" },
                        style: "font-size:11.5px;padding:4px 9px;",
                        onclick: move |_| {
                            let next = !*staff_only.read();
                            staff_only.set(next);
                        },
                        {i18n.t("console.users.filter.staff")}
                        if *staff_only.read() {
                            Ic { icon: Icon::Close, size: 11 }
                        }
                    }
                    button {
                        class: if *status.read() == StatusFilter::Any { "ik-chip" } else { "ik-chip active" },
                        style: "font-size:11.5px;padding:4px 9px;",
                        onclick: move |_| {
                            let next = status.read().next();
                            status.set(next);
                        },
                        {i18n.t(status.read().label_key())}
                    }
                    button {
                        class: "ik-btn xs",
                        style: "margin-left:auto;",
                        onclick: move |_| reload.bump(),
                        Ic { icon: Icon::Refresh, size: 12 }
                        {i18n.t("console.live.refresh")}
                    }
                }
            }
            {
                async_view(
                    &directory,
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
                                    {i18n.t("console.users.empty")}
                                }
                            };
                        }
                        rsx! {
                            for row in rows.clone() {
                                UserRow {
                                    key: "{row.id}",
                                    row: row.clone(),
                                    selected: current.as_deref() == Some(row.id.as_str()),
                                    on_pick: move |id| selected.set(Some(id)),
                                }
                            }
                        }
                    },
                )
            }
            div { class: "ik-cons-foot",
                span {
                    {
                        i18n.args(
                            "console.users.range",
                            &[
                                ("first", &(offset + 1).to_string()),
                                (
                                    "last",
                                    &(offset + i64::try_from(rows.len()).unwrap_or(0)).to_string(),
                                ),
                                ("total", &thousands(total)),
                            ],
                        )
                    }
                }
                span { class: "hint", style: "display:flex;gap:6px;",
                    button {
                        class: "ik-btn xs",
                        disabled: !has_prev,
                        onclick: move |_| page -= 1,
                        {i18n.t("common.previous")}
                    }
                    button {
                        class: "ik-btn xs",
                        disabled: !has_next,
                        onclick: move |_| page += 1,
                        {i18n.t("common.next")}
                    }
                }
            }
        }
        if let Some(id) = current {
            UserInspector {
                key: "{id}",
                user_id: id,
                reload,
                on_erased: move |()| selected.set(None),
            }
        } else {
            NoSelection { message: i18n.t("console.users.pick") }
        }
    }
}

/// One directory row: who they are, what they hold, and the state their account is in.
#[component]
fn UserRow(row: DirectoryRow, selected: bool, on_pick: EventHandler<String>) -> Element {
    let i18n = use_i18n();
    let id = row.id.clone();
    let staff = row.permission_count > 0;
    let suspended = row.status == AccountStatus::Suspended;

    let class = match (selected, suspended) {
        (true, _) => "ik-cons-row selected",
        (false, true) => "ik-cons-row dim",
        (false, false) => "ik-cons-row",
    };

    rsx! {
        button {
            class: "{class}",
            "aria-current": if selected { "true" } else { "false" },
            onclick: move |_| on_pick.call(id.clone()),
            div { class: "ik-flex", style: "gap:10px;",
                span { class: if staff { "ik-avatar sm" } else { "ik-avatar sm neutral" },
                    {initial(&row.username)}
                }
                span { style: "min-width:0;",
                    span { style: "display:block;font-weight:600;font-size:13px;", "{row.username}" }
                    span { class: "ik-mono", style: "display:block;font-size:10.5px;color:var(--muted);margin-top:2px;overflow:hidden;text-overflow:ellipsis;white-space:nowrap;",
                        "{row.email} · "
                        {
                            i18n.args(
                                "console.users.trackedShort",
                                &[("count", &thousands(row.tracked_count))],
                            )
                        }
                    }
                }
                span { style: "margin-left:auto;flex:none;display:flex;gap:6px;align-items:center;",
                    if staff && !suspended {
                        span { class: "ik-pill acc", style: "font-size:9.5px;",
                            {i18n.t("console.users.role.staff")}
                        }
                    }
                    span { class: row.status.pill_class(), style: "font-size:9.5px;",
                        {i18n.t(row.status.label_key())}
                    }
                }
            }
        }
    }
}

/// Fetches one account, then hands it to the editor keyed on its id so the editor's fields are
/// seeded from real values rather than from empty defaults it has to reconcile later.
#[component]
fn UserInspector(user_id: String, reload: Reload, on_erased: EventHandler<()>) -> Element {
    let api = api::use_api();
    let i18n = use_i18n();
    let detail_reload = use_reload();
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
        div { class: "ik-cons-insp",
            {
                async_view(
                    &detail,
                    detail_reload,
                    || rsx! {
                        div { style: "padding:22px;",
                            SkeletonBlock { height: 280 }
                        }
                    },
                    |data| rsx! {
                        UserEditor {
                            data: data.clone(),
                            reload,
                            detail_reload,
                            on_erased,
                        }
                    },
                )
            }
        }
    }
}

#[component]
fn UserEditor(
    data: UserDetailResponse,
    reload: Reload,
    detail_reload: Reload,
    on_erased: EventHandler<()>,
) -> Element {
    let api = api::use_api();
    let i18n = use_i18n();
    let session = use_session();
    let caps = use_capabilities();
    let busy = use_busy();
    let mut outcome = use_outcome();
    let mut tab = use_signal(|| Tab::Identity);

    let can_write = caps.can(Permission::UsersWrite);
    let can_grant = caps.can(Permission::UsersPermissions);
    let can_sessions = caps.can(Permission::UsersSessions);
    let can_delete = caps.can(Permission::UsersDelete);
    let can_sync = caps.can(Permission::SyncAdminWrite);

    let user = data.user.clone();
    let id = user.id.clone();
    // The server refuses an administrator acting on their own account; saying so explains why
    // the controls are absent instead of leaving an inspector that looks broken.
    let is_self = session.username().is_some_and(|name| name == user.username);

    let mut name_field = use_signal(|| user.username.clone());
    let mut email_field = use_signal(|| user.email.clone());
    let mut status_field = use_signal(|| user.status);
    let mut suspend_reason = use_signal(String::new);
    // Recorded in the audit trail, which is the only place it can survive the erasure.
    let mut erase_reason = use_signal(String::new);
    // Seeded from the server's answer and edited locally. Only tokens this build recognises are
    // seeded: an inert grant left over from another version is shown separately rather than
    // silently re-submitted as if it were current.
    let chosen = use_signal(|| {
        data.permissions
            .iter()
            .filter(|g| g.known)
            .filter_map(|g| {
                serde_json::from_value::<Permission>(serde_json::json!(g.permission)).ok()
            })
            .collect::<BTreeSet<Permission>>()
    });
    let granted_now: BTreeSet<Permission> = data
        .permissions
        .iter()
        .filter(|g| g.known)
        .filter_map(|g| serde_json::from_value::<Permission>(serde_json::json!(g.permission)).ok())
        .collect();

    let identity_dirty = *name_field.read() != user.username || *email_field.read() != user.email;
    let status_dirty = *status_field.read() != user.status;
    let grants_dirty = *chosen.read() != granted_now;
    let dirty = (can_write && (identity_dirty || status_dirty)) || (can_grant && grants_dirty);

    // One control, up to three calls — each only when the reader may make it and something
    // actually changed. They run in order and stop at the first failure, so a rejected rename
    // never silently applies a grant change alongside it.
    let save = {
        let user = user.clone();
        move |_| {
            if !busy.claim() {
                return;
            }
            outcome.set(None);
            let id = user.id.clone();
            let identity = (can_write && identity_dirty).then(|| AdminProfileUpdate {
                username: Some(name_field.peek().trim().to_owned()).filter(|v| *v != user.username),
                email: Some(email_field.peek().trim().to_owned()).filter(|v| *v != user.email),
            });
            let status = (can_write && status_dirty).then(|| SetUserStatus {
                status: *status_field.peek(),
                reason: {
                    let text = suspend_reason.peek().trim().to_owned();
                    (!text.is_empty()).then_some(text)
                },
                // A suspension that leaves the account working until its access token expires
                // is not what anyone means by suspending it.
                revoke_sessions: Some(true),
            });
            let grants = (can_grant && grants_dirty).then(|| SetPermissions {
                permissions: chosen.peek().iter().copied().collect(),
            });
            let client = api.client();
            spawn(async move {
                let mut failure = None;
                if let Some(body) = identity {
                    if let Err(e) = client.update_user().id(id.clone()).body(body).send().await {
                        failure = Some(api::friendly_error(i18n, e));
                    }
                }
                if failure.is_none() {
                    if let Some(body) = status {
                        if let Err(e) = client
                            .set_user_status()
                            .id(id.clone())
                            .body(body)
                            .send()
                            .await
                        {
                            failure = Some(api::friendly_error(i18n, e));
                        }
                    }
                }
                if failure.is_none() {
                    if let Some(body) = grants {
                        if let Err(e) = client
                            .set_user_permissions()
                            .id(id.clone())
                            .body(body)
                            .send()
                            .await
                        {
                            failure = Some(api::friendly_error(i18n, e));
                        }
                    }
                }
                match failure {
                    None => {
                        outcome.set(Some(Ok(i18n.t("console.users.saved"))));
                        suspend_reason.set(String::new());
                        detail_reload.bump();
                        reload.bump();
                    }
                    Some(message) => outcome.set(Some(Err(message))),
                }
                busy.release();
            });
        }
    };

    // One clone per consumer: `rsx!` moves each into its own closure or child component.
    let (id_header, id_sessions, id_verify, id_sync, id_erase) =
        (id.clone(), id.clone(), id.clone(), id.clone(), id);
    let staff = !data.permissions.is_empty();
    let joined = iso_date(Some(&user.created_at)).to_owned();
    let last_seen = rel_time(i18n, user.last_login_at.as_deref());
    // The id is a uuid; its leading group is enough to recognise an account by, and the whole
    // thing would crowd the meta line.
    let short_id = user.id.get(0..8).unwrap_or(&user.id).to_owned();

    rsx! {
        div { class: "ik-cons-insphead",
            div { class: "ik-flex", style: "align-items:flex-start;gap:14px;",
                span { class: if staff { "ik-avatar lg" } else { "ik-avatar lg neutral" },
                    {initial(&user.username)}
                }
                div { style: "min-width:0;flex:1;",
                    div { class: "ik-flex", style: "gap:10px;flex-wrap:wrap;",
                        h2 { class: "ik-insp-title", "{user.username}" }
                        span { class: if staff { "ik-pill acc" } else { "ik-pill" }, style: "font-size:10px;",
                            if staff {
                                {i18n.t("console.users.role.staff")}
                            } else {
                                {i18n.t("console.users.role.reader")}
                            }
                        }
                        if user.status == AccountStatus::Suspended {
                            span { class: user.status.pill_class(), style: "font-size:10px;",
                                {i18n.t(user.status.label_key())}
                            }
                        }
                    }
                    div { class: "ik-meta-line",
                        span { "id {short_id}" }
                        span { {i18n.args("console.users.joinedOn", &[("date", &joined)])} }
                        span {
                            {
                                i18n.args(
                                    "console.users.trackedShort",
                                    &[("count", &thousands(user.tracked_count))],
                                )
                            }
                        }
                        span { class: "ok",
                            {i18n.args("console.users.lastSeen", &[("when", &last_seen)])}
                        }
                    }
                }
                div { class: "ik-flex", style: "gap:7px;flex:none;flex-wrap:wrap;justify-content:flex-end;",
                    if can_sessions {
                        button {
                            class: "ik-btn sm",
                            disabled: busy.is_busy() || user.active_sessions == 0,
                            onclick: move |_| {
                                revoke_all(api, i18n, busy, outcome, detail_reload, id_header.clone());
                            },
                            {i18n.t("console.users.revokeSessions")}
                        }
                    }
                    if (can_write || can_grant) && !is_self {
                        button {
                            class: "ik-btn sm primary",
                            disabled: busy.is_busy() || !dirty,
                            onclick: save,
                            {i18n.t("console.users.save")}
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
            if is_self {
                p { class: "ik-muted", style: "font-size:12px;margin:12px 0 0;",
                    {i18n.t("console.users.selfNotice")}
                }
            }
            OutcomeLine { outcome: outcome.read().clone() }
        }
        match *tab.read() {
            Tab::Identity => rsx! {
                div { class: "ik-cons-inspbody",
                    div { class: "ik-cons-col",
                        Section { label: i18n.t("console.users.identity"),
                            div { style: "display:flex;flex-direction:column;gap:9px;",
                                div { class: "ik-kv narrow",
                                    label { class: "k", r#for: "tv-u-name", {i18n.t("auth.field.username")} }
                                    input {
                                        id: "tv-u-name",
                                        class: "ik-input",
                                        style: "font-size:12.5px;padding:9px 11px;",
                                        disabled: !can_write || is_self,
                                        value: "{name_field}",
                                        oninput: move |e| name_field.set(e.value()),
                                    }
                                }
                                div { class: "ik-kv narrow",
                                    label { class: "k", r#for: "tv-u-email", {i18n.t("auth.field.email")} }
                                    input {
                                        id: "tv-u-email",
                                        class: "ik-input",
                                        style: "font-size:12.5px;padding:9px 11px;",
                                        r#type: "email",
                                        disabled: !can_write || is_self,
                                        value: "{email_field}",
                                        oninput: move |e| email_field.set(e.value()),
                                    }
                                }
                                div { class: "ik-kv narrow",
                                    span { class: "k", {i18n.t("console.users.verified")} }
                                    if user.email_verified {
                                        span { class: "ik-flex ik-mono", style: "gap:7px;font-size:12px;color:var(--jade-bright);",
                                            Ic { icon: Icon::Check, size: 14 }
                                            "{joined}"
                                        }
                                    } else {
                                        VerifyEmailAction {
                                            user_id: id_verify.clone(),
                                            enabled: can_write && !is_self,
                                            reload,
                                            detail_reload,
                                        }
                                    }
                                }
                                div { class: "ik-kv narrow",
                                    span { class: "k", {i18n.t("console.users.status")} }
                                    div { class: "ik-seg", role: "radiogroup",
                                        for option in [AccountStatus::Active, AccountStatus::Suspended] {
                                            button {
                                                key: "{option.label_key()}",
                                                class: if *status_field.read() == option { "on" } else { "" },
                                                role: "radio",
                                                disabled: !can_write || is_self,
                                                "aria-checked": if *status_field.read() == option { "true" } else { "false" },
                                                onclick: move |_| status_field.set(option),
                                                {i18n.t(option.label_key())}
                                            }
                                        }
                                    }
                                }
                                if *status_field.read() == AccountStatus::Suspended && status_dirty {
                                    div { class: "ik-kv narrow",
                                        label { class: "k", r#for: "tv-u-reason", {i18n.t("console.users.suspendReason")} }
                                        input {
                                            id: "tv-u-reason",
                                            class: "ik-input",
                                            style: "font-size:12.5px;padding:9px 11px;",
                                            placeholder: i18n.t("console.users.suspendReasonPlaceholder"),
                                            value: "{suspend_reason}",
                                            oninput: move |e| suspend_reason.set(e.value()),
                                        }
                                    }
                                }
                                if let Some(reason) = user.suspension_reason.clone() {
                                    div { class: "ik-kv narrow",
                                        span {}
                                        span { class: "warn",
                                            {i18n.args("console.users.suspendedFor", &[("reason", &reason)])}
                                        }
                                    }
                                }
                            }
                        }
                        if can_grant {
                            PermissionGrants {
                                grants: data.permissions.clone(),
                                chosen,
                                editable: !is_self,
                            }
                        }
                    }
                    div { class: "ik-cons-col",
                        ExternalSync {
                            user_id: id_sync.clone(),
                            username: user.username.clone(),
                            editable: can_sync,
                        }
                    }
                }
            },
            Tab::Sessions => rsx! {
                div { class: "ik-cons-inspbody",
                    div { class: "ik-cons-col", style: "grid-column:1 / -1;max-width:620px;",
                        Section { label: i18n.t("console.users.sessions"),
                            div { class: "ik-listbox",
                                div { class: "ik-listrow",
                                    span { style: "font-size:12.5px;",
                                        {
                                            i18n.args(
                                                "console.users.activeSessions",
                                                &[("count", &user.active_sessions.to_string())],
                                            )
                                        }
                                    }
                                    if can_sessions {
                                        button {
                                            class: "ik-btn xs",
                                            style: "margin-left:auto;",
                                            disabled: busy.is_busy() || user.active_sessions == 0,
                                            onclick: move |_| {
                                                revoke_all(
                                                    api,
                                                    i18n,
                                                    busy,
                                                    outcome,
                                                    detail_reload,
                                                    id_sessions.clone(),
                                                );
                                            },
                                            {i18n.t("console.users.revokeSessions")}
                                        }
                                    }
                                }
                            }
                            // TODO(api): per-device rows need GET /v1/admin/users/:id/sessions.
                            p { class: "ik-muted", style: "font-size:11.5px;line-height:1.5;margin:8px 0 0;",
                                {i18n.t("console.users.sessionRowsUnavailable")}
                            }
                        }
                    }
                }
            },
            Tab::Library => rsx! {
                div { class: "ik-cons-inspbody",
                    div { class: "ik-cons-col", style: "grid-column:1 / -1;",
                        div { class: "ik-kpis",
                            div { class: "ik-kpi",
                                div { class: "ik-kpi-label", {i18n.t("console.users.stat.tracked")} }
                                div { class: "ik-kpi-value", style: "font-size:24px;",
                                    "{thousands(user.tracked_count)}"
                                }
                            }
                            div { class: "ik-kpi",
                                div { class: "ik-kpi-label", {i18n.t("console.users.stat.linked")} }
                                div { class: "ik-kpi-value", style: "font-size:24px;",
                                    "{thousands(user.linked_accounts)}"
                                }
                            }
                            div { class: "ik-kpi",
                                div { class: "ik-kpi-label", {i18n.t("console.users.stat.sessions")} }
                                div { class: "ik-kpi-value", style: "font-size:24px;",
                                    "{thousands(user.active_sessions)}"
                                }
                            }
                        }
                    }
                }
            },
            Tab::Privacy => rsx! {
                div { class: "ik-cons-inspbody",
                    div { class: "ik-cons-col", style: "grid-column:1 / -1;max-width:620px;",
                        Section { label: i18n.t("console.users.tab.privacy"),
                            p { class: "ik-muted", style: "font-size:12px;line-height:1.5;margin:0 0 12px;",
                                {
                                    i18n.plural(
                                        "console.users.openRequests",
                                        user.open_privacy_requests,
                                        &[],
                                    )
                                }
                            }
                            if can_delete && !is_self {
                                div { class: "ik-field",
                                    label { r#for: "tv-u-erase-reason", {i18n.t("console.users.deleteReason")} }
                                    input {
                                        id: "tv-u-erase-reason",
                                        class: "ik-input",
                                        style: "font-size:12.5px;padding:9px 11px;",
                                        value: "{erase_reason}",
                                        oninput: move |e| erase_reason.set(e.value()),
                                    }
                                }
                                div { class: "ik-danger",
                                    TypeToConfirm {
                                        title: i18n.t("console.users.delete"),
                                        body: i18n.args(
                                            "console.users.eraseRadius",
                                            &[("count", &thousands(user.tracked_count))],
                                        ),
                                        expect: user.username.clone(),
                                        cta: i18n.t("console.users.deleteConfirmCta"),
                                        busy: busy.is_busy(),
                                        on_confirm: move |()| {
                                            erase(
                                                api,
                                                i18n,
                                                busy,
                                                outcome,
                                                id_erase.clone(),
                                                user.username.clone(),
                                                erase_reason.peek().trim().to_owned(),
                                                reload,
                                                on_erased,
                                            );
                                        },
                                    }
                                }
                            }
                        }
                    }
                }
            },
            Tab::Activity => rsx! {
                div { class: "ik-cons-inspbody",
                    div { class: "ik-cons-col", style: "grid-column:1 / -1;max-width:620px;",
                        RecentActions { username: user.username.clone() }
                    }
                }
            },
        }
    }
}

/// Erase an account.
///
/// A free function rather than a closure: it needs every handle the editor holds, and a closure
/// that moves its captured signals into a spawned task can only be called once.
#[allow(clippy::too_many_arguments)]
fn erase(
    api: api::Api,
    i18n: crate::i18n::Translator,
    busy: crate::hooks::Busy,
    outcome: Signal<crate::hooks::Outcome>,
    user_id: String,
    username: String,
    reason: String,
    reload: Reload,
    on_erased: EventHandler<()>,
) {
    if !busy.claim() {
        return;
    }
    let mut outcome = outcome;
    outcome.set(None);
    let client = api.client();
    spawn(async move {
        let body = DeleteUser {
            // The server re-checks this against the account it is about to erase;
            // `TypeToConfirm` has already proved the operator typed it.
            confirm_username: username,
            reason: (!reason.is_empty()).then_some(reason),
        };
        match client.delete_user().id(user_id).body(body).send().await {
            Ok(_) => {
                on_erased.call(());
                reload.bump();
            }
            Err(e) => outcome.set(Some(Err(api::friendly_error(i18n, e)))),
        }
        busy.release();
    });
}

/// Force an account out of every device it is signed in on. Free-standing for the same reason
/// as [`erase`]: the header and the Sessions tab both offer it.
fn revoke_all(
    api: api::Api,
    i18n: crate::i18n::Translator,
    busy: crate::hooks::Busy,
    outcome: Signal<crate::hooks::Outcome>,
    detail_reload: Reload,
    user_id: String,
) {
    if !busy.claim() {
        return;
    }
    let mut outcome = outcome;
    outcome.set(None);
    let client = api.client();
    spawn(async move {
        match client.revoke_user_sessions().id(user_id).send().await {
            Ok(_) => {
                outcome.set(Some(Ok(i18n.t("console.users.signedOutEverywhere"))));
                detail_reload.bump();
            }
            Err(e) => outcome.set(Some(Err(api::friendly_error(i18n, e)))),
        }
        busy.release();
    });
}

/// Confirm an address administratively, for an account that never clicked its link.
#[component]
fn VerifyEmailAction(
    user_id: String,
    enabled: bool,
    reload: Reload,
    detail_reload: Reload,
) -> Element {
    let api = api::use_api();
    let i18n = use_i18n();
    let busy = use_busy();

    let verify = move |_| {
        if !busy.claim() {
            return;
        }
        let id = user_id.clone();
        let client = api.client();
        spawn(async move {
            if client.verify_user_email().id(id).send().await.is_ok() {
                detail_reload.bump();
                reload.bump();
            }
            busy.release();
        });
    };

    rsx! {
        div { class: "ik-flex", style: "gap:8px;",
            span {
                class: "ik-mono",
                style: "font-size:12px;color:var(--star);",
                title: i18n.t("console.users.unverifiedHint"),
                {i18n.t("console.users.unverified")}
            }
            if enabled {
                button { class: "ik-btn xs", disabled: busy.is_busy(), onclick: verify,
                    {i18n.t("console.users.verifyEmail")}
                }
            }
        }
    }
}

/// The permission checklist, grouped, with provenance and the preset bundles as starting
/// points.
///
/// Edits are local until the inspector's one Save submits them, and the whole set goes at once
/// — the server replaces it wholesale, so two administrators editing concurrently produce one
/// of their two intents rather than an interleaving of both.
#[component]
fn PermissionGrants(
    grants: Vec<GrantRow>,
    chosen: Signal<BTreeSet<Permission>>,
    editable: bool,
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
/// Applying one *replaces* the current selection rather than adding to it, which is what makes
/// it a starting point rather than a cumulative grant — and why it is safe that presets are
/// never stored: what gets saved is whatever is ticked afterwards.
#[component]
fn PresetPicker(catalogue: PermissionCatalogue, chosen: Signal<BTreeSet<Permission>>) -> Element {
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
fn GrantGroup(
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
fn GrantRowView(
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
const PERMISSION_GROUPS: [(PermissionGroup, &str); 8] = [
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

/// This account's linked external trackers, with the two admin-side actions the API supports.
#[component]
fn ExternalSync(user_id: String, username: String, editable: bool) -> Element {
    let api = api::use_api();
    let i18n = use_i18n();
    let reload = use_reload();
    let Ok(uuid) = uuid::Uuid::parse_str(&user_id) else {
        return rsx! {};
    };

    let accounts = use_resource(move || {
        reload.track();
        let client = api.client();
        async move {
            client
                .list_sync_accounts()
                .send()
                .await
                .map(ResponseValue::into_inner)
                .map_err(|e| api::friendly_error(i18n, e))
        }
    });

    rsx! {
        Section { label: i18n.t("console.users.externalSync"),
            {
                async_view(
                    &accounts,
                    reload,
                    || rsx! { SkeletonBlock { height: 90 } },
                    |all| {
                        let mine: Vec<AdminSyncAccount> = all
                            .iter()
                            .filter(|row| row.user_id == uuid)
                            .cloned()
                            .collect();
                        if mine.is_empty() {
                            return rsx! {
                                p { class: "ik-muted", style: "font-size:12px;margin:0;",
                                    {i18n.t("console.users.noSyncLinks")}
                                }
                            };
                        }
                        rsx! {
                            div { class: "ik-listbox",
                                for account in mine {
                                    SyncLinkRow {
                                        key: "{account.provider}",
                                        account,
                                        username: username.clone(),
                                        editable,
                                        reload,
                                    }
                                }
                            }
                        }
                    },
                )
            }
        }
    }
}

/// One linked tracker: force a pull, or unlink it behind an inline confirmation. Unlinking is
/// reversible — the reader can relink themselves — so it asks once rather than demanding typing.
#[component]
fn SyncLinkRow(
    account: AdminSyncAccount,
    username: String,
    editable: bool,
    reload: Reload,
) -> Element {
    let api = api::use_api();
    let i18n = use_i18n();
    let busy = use_busy();
    let mut confirming = use_signal(|| false);
    let mut error = use_signal(|| Option::<String>::None);

    let target = SyncAccountTarget {
        provider: account.provider.clone(),
        user_id: UserId(account.user_id),
    };
    let external = account
        .external_username
        .clone()
        .unwrap_or_else(|| username.clone());

    let pull = {
        let target = target.clone();
        move |_| {
            if !busy.claim() {
                return;
            }
            error.set(None);
            let target = target.clone();
            let client = api.client();
            spawn(async move {
                if let Err(e) = client.admin_sync_pull().body(target).send().await {
                    error.set(Some(api::friendly_error(i18n, e)));
                }
                reload.bump();
                busy.release();
            });
        }
    };

    let unlink = {
        let target = target.clone();
        move |()| {
            if !busy.claim() {
                return;
            }
            error.set(None);
            let target = target.clone();
            let client = api.client();
            spawn(async move {
                match client.admin_sync_unlink().body(target).send().await {
                    Ok(_) => {
                        confirming.set(false);
                        reload.bump();
                    }
                    Err(e) => error.set(Some(api::friendly_error(i18n, e))),
                }
                busy.release();
            });
        }
    };

    if *confirming.read() {
        return rsx! {
            InlineConfirm {
                title: i18n.args("console.users.unlinkTitle", &[("provider", &account.provider)]),
                body: i18n.t("console.users.unlinkWhy"),
                cta: i18n.t("console.users.unlink"),
                busy: busy.is_busy(),
                on_cancel: move |()| confirming.set(false),
                on_confirm: unlink,
            }
        };
    }

    rsx! {
        div { class: "ik-listrow",
            span { class: "ik-mono-tile lg jade", {monogram(&account.provider)} }
            div { style: "min-width:0;",
                div { style: "font-weight:600;font-size:12.5px;", "{account.provider} · {external}" }
                div { class: "ik-mono", style: "font-size:10.5px;color:var(--muted);margin-top:1px;",
                    {
                        i18n.args(
                            "console.users.syncMeta",
                            &[
                                ("when", &rel_time(i18n, account.last_synced_at.as_deref())),
                                ("conflicts", &account.pending_conflicts.to_string()),
                            ],
                        )
                    }
                }
                if let Some(message) = error.read().clone() {
                    ErrorLine { message }
                }
            }
            if editable {
                div { class: "ik-flex", style: "margin-left:auto;gap:6px;flex:none;",
                    button { class: "ik-btn xs", disabled: busy.is_busy(), onclick: pull,
                        {i18n.t("console.users.forcePull")}
                    }
                    button {
                        class: "ik-btn xs acc",
                        disabled: busy.is_busy(),
                        onclick: move |_| confirming.set(true),
                        {i18n.t("console.users.unlink")}
                    }
                }
            }
        }
    }
}

/// What this account has actually done, out of the privileged-action trail.
#[component]
fn RecentActions(username: String) -> Element {
    let api = api::use_api();
    let i18n = use_i18n();
    let caps = use_capabilities();
    let reload = use_reload();

    if !caps.can(Permission::AuditRead) {
        return rsx! {};
    }

    let entries = use_resource(move || {
        reload.track();
        let client = api.client();
        async move {
            client
                .audit_log()
                .send()
                .await
                .map(ResponseValue::into_inner)
                .map_err(|e| api::friendly_error(i18n, e))
        }
    });

    rsx! {
        Section { label: i18n.t("console.users.recentActions"),
            {
                async_view(
                    &entries,
                    reload,
                    || rsx! { SkeletonBlock { height: 120 } },
                    |all| {
                        let mine: Vec<_> = all
                            .iter()
                            .filter(|entry| entry.actor.as_deref() == Some(username.as_str()))
                            .take(10)
                            .cloned()
                            .collect();
                        if mine.is_empty() {
                            return rsx! {
                                p { class: "ik-muted", style: "font-size:12px;margin:0;",
                                    {i18n.t("console.users.noRecentActions")}
                                }
                            };
                        }
                        rsx! {
                            div { class: "ik-timeline",
                                for entry in mine {
                                    div { key: "{entry.id}",
                                        span { class: "val", "{entry.action}" }
                                        if let Some(target) = entry.target.clone() {
                                            " · {target}"
                                        }
                                        " · "
                                        {rel_time(i18n, Some(&entry.created_at))}
                                    }
                                }
                            }
                        }
                    },
                )
            }
        }
    }
}
