//! Console · Users — the account directory and a full inspector per account.
//!
//! Off the shared auto-refresh tick: a background refetch mid-edit would discard whatever the
//! operator is typing. Reloads only after its own writes, or a manual reload.
//!
//! Per-device session rows and per-account export are absent — no endpoint exists for either.
//! TODO(api): `GET /v1/admin/users/:id/sessions`, `GET /v1/admin/users/:id/export`.

mod actions;
mod activity;
mod grants;
mod row;
mod sync;

use crate::api;
use crate::components::{
    async_view, use_step_up_gate, CompactPager, Kpi, ListSearch, NoSelection, OutcomeLine, Section,
    SegControl, SkeletonBlock, StepUpPrompt, TabBar, TabKind, TypeToConfirm, Window,
};
use crate::hooks::{use_busy, use_outcome, use_reload, Reload};
use crate::i18n::use_i18n;
use crate::icons::{Ic, Icon};
use crate::models::AccountStatusExt as _;
use crate::state::capabilities::use_capabilities;
use crate::state::use_session;
use crate::util::{initial, iso_date, rel_time, thousands};
use crate::views::console::{use_console_nav, ConsoleQuery};
use crate::wire::types::{
    AccountStatus, AdminProfileUpdate, DirectoryRow, GrantRow, Permission, SetPermissions,
    SetUserStatus, UserDetailResponse,
};
use actions::{erase, revoke_all, VerifyEmailAction};
use activity::RecentActions;
use dioxus::prelude::*;
use grants::PermissionGrants;
use progenitor_client::ResponseValue;
use row::UserRow;
use std::collections::BTreeSet;
use sync::ExternalSync;

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

impl TabKind for Tab {
    fn all() -> &'static [Self] {
        &[
            Self::Identity,
            Self::Sessions,
            Self::Library,
            Self::Privacy,
            Self::Activity,
        ]
    }

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

impl Tab {
    /// This tab's `?tab=` token.
    fn token(self) -> &'static str {
        match self {
            Self::Identity => "identity",
            Self::Sessions => "sessions",
            Self::Library => "library",
            Self::Privacy => "privacy",
            Self::Activity => "activity",
        }
    }

    /// An unrecognised token opens the default tab rather than refusing the link.
    fn parse(token: &str) -> Self {
        <Self as TabKind>::all()
            .iter()
            .copied()
            .find(|tab| tab.token() == token)
            .unwrap_or(Self::Identity)
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

    /// This filter's `?status=` token. `Any` is the absence of the parameter, not a value.
    fn token(self) -> &'static str {
        match self {
            Self::Any => "",
            Self::Active => "active",
            Self::Suspended => "suspended",
        }
    }

    /// An unrecognised token shows every account rather than silently hiding most of the
    /// directory behind a filter the operator did not choose.
    fn parse(token: &str) -> Self {
        match token {
            "active" => Self::Active,
            "suspended" => Self::Suspended,
            _ => Self::Any,
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

/// The permissions in `grants` this build recognises, as a set.
///
/// Unknown tokens are surfaced separately as "unknown grant" rather than dropped, but must stay
/// out of this set — including one would re-submit an inert token as current on save.
fn known_permissions(grants: &[GrantRow]) -> BTreeSet<Permission> {
    grants
        .iter()
        .filter(|g| g.known)
        .filter_map(|g| serde_json::from_value::<Permission>(serde_json::json!(g.permission)).ok())
        .collect()
}

/// Whether these grants are the deployment owner's.
///
/// Asked of the grant list rather than of a count, because the super user holds exactly one
/// grant: counting says "an operator with one capability", which is the opposite of the truth.
/// The token is also absent from the permission catalogue by design, so the checklist below
/// renders it nowhere — hence the explicit notice this drives.
fn is_super_user(grants: &[GrantRow]) -> bool {
    known_permissions(grants).contains(&Permission::SystemSuperuser)
}

/// The list pane and the inspector pane, as the console shell's two grid children.
#[component]
pub(super) fn UsersEntity() -> Element {
    let api = api::use_api();
    let i18n = use_i18n();
    let reload = use_reload();
    let nav = use_console_nav();
    let view = nav.query();
    let status = StatusFilter::parse(view.status_token());
    let staff_only = view.staff;
    let page = i64::from(view.page);

    // The search term goes to the server (it matches username *and* email across the whole
    // table); the two chips filter the page that comes back.
    let search = view.q.clone();
    let directory = use_resource(use_reactive!(|(search, page)| {
        reload.track();
        let offset = page * PAGE_SIZE;
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
    }));

    // Memoised: without it, this reclones the whole page and every row on every render,
    // including hover-state changes and search keystrokes.
    //
    // Three separate counts, deliberately: `rows` is what shows after the status/staff
    // filters, `page_len` is what the server returned for this window, `total` is the whole
    // directory. Pagination arithmetic must use `page_len`, not `rows.len()` — `rows.len()`
    // made `has_next` go false while later pages still existed, and the "1-N of TOTAL" line
    // compared a client-side count to a server-side total.
    let page_state = use_memo(use_reactive!(|(status, staff_only)| {
        match &*directory.read() {
            Some(Ok(page_data)) => {
                let filtered: Vec<DirectoryRow> = page_data
                    .users
                    .iter()
                    .filter(|row| status.accepts(row))
                    .filter(|row| !staff_only || row.permission_count > 0)
                    .cloned()
                    .collect();
                (
                    filtered,
                    i64::try_from(page_data.users.len()).unwrap_or(0),
                    page_data.total,
                )
            }
            _ => (Vec::new(), 0, 0),
        }
    }));
    let (rows, page_len, total) = page_state.read().clone();
    // A `sel` naming an account the current filters exclude falls back to the first row, so a
    // stale link opens a lit row rather than an inspector with nothing selected beside it.
    let current = view
        .sel
        .clone()
        .filter(|id| rows.iter().any(|row| &row.id == id))
        .or_else(|| rows.first().map(|r| r.id.clone()));
    let window = Window {
        offset: page * PAGE_SIZE,
        page_len,
        total,
    };

    rsx! {
        div { class: "ik-cons-list",
            div { class: "ik-cons-listhead",
                ListSearch {
                    placeholder: i18n.t("console.users.searchPlaceholder"),
                    query: view.q.clone(),
                    on_input: move |text| nav.filter(nav.query().with_search(text)),
                    hits: i18n.plural(
                        "console.users.hits",
                        i64::try_from(rows.len()).unwrap_or(0),
                        &[],
                    ),
                }
                div { class: "ik-flex", style: "gap:6px;flex-wrap:wrap;",
                    button {
                        class: if staff_only { "ik-chip active" } else { "ik-chip" },
                        style: "font-size:11.5px;padding:4px 9px;",
                        onclick: move |_| {
                            // Page one: the filter narrows the directory, so page 4 of the old
                            // width is very likely past the end of the new one.
                            let next = ConsoleQuery { staff: !staff_only, page: 0, ..nav.query() };
                            nav.filter(next);
                        },
                        {i18n.t("console.users.filter.staff")}
                        if staff_only {
                            Ic { icon: Icon::Close, size: 11 }
                        }
                    }
                    button {
                        class: if status == StatusFilter::Any { "ik-chip" } else { "ik-chip active" },
                        style: "font-size:11.5px;padding:4px 9px;",
                        onclick: move |_| {
                            let token = status.next().token();
                            let next = ConsoleQuery {
                                status: (!token.is_empty()).then(|| token.to_owned()),
                                page: 0,
                                ..nav.query()
                            };
                            nav.filter(next);
                        },
                        {i18n.t(status.label_key())}
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
                                    on_pick: move |id: String| nav.select(nav.query().with_selection(Some(id))),
                                }
                            }
                        }
                    },
                )
            }
            CompactPager {
                page,
                window,
                on_page: move |next: i64| nav.select(nav.query().with_page(u32::try_from(next).unwrap_or(0))),
            }
        }
        if let Some(id) = current {
            UserInspector {
                key: "{id}",
                user_id: id,
                reload,
                on_erased: move |()| nav.select(nav.query().with_selection(None)),
            }
        } else {
            NoSelection { message: i18n.t("console.users.pick") }
        }
    }
}

/// Fetches one account, then hands it to the editor keyed on its id so its fields seed from
/// real values.
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
    let nav = use_console_nav();
    let tab = Tab::parse(nav.query().tab_token());
    // One gate for the whole editor: identity, status, grants, sessions and erasure are all
    // mutating operator capabilities, and the editor has one outcome line to answer through.
    let gate = use_step_up_gate();

    let can_write = caps.can(Permission::UsersWrite);
    let can_grant = caps.can(Permission::UsersPermissions);
    let can_sessions = caps.can(Permission::UsersSessions);
    let can_delete = caps.can(Permission::UsersDelete);
    let can_sync = caps.can(Permission::SyncAdminWrite);

    let user = data.user.clone();
    let id = user.id.clone();
    // The server refuses an administrator acting on their own account, hence the notice below
    // rather than an inspector that looks broken.
    let is_self = session.username().is_some_and(|name| name == user.username);

    let mut name_field = use_signal(|| user.username.clone());
    let mut email_field = use_signal(|| user.email.clone());
    let mut status_field = use_signal(|| user.status);
    let mut suspend_reason = use_signal(String::new);
    // Recorded in the audit trail, which is the only place it can survive the erasure.
    let mut erase_reason = use_signal(String::new);
    // Seeded from the server's answer and edited locally; only recognised tokens are seeded.
    //
    // `granted_now` is built once and used for both the seed and the dirty comparison —
    // computing it twice independently is the shape that produces a phantom "unsaved changes"
    // state when the two copies drift. Not a `use_memo`: `data` changes when the detail
    // refetches, and a memo would freeze the comparison at the first render.
    let granted_now = known_permissions(&data.permissions);
    let chosen = use_signal({
        let seed = granted_now.clone();
        move || seed
    });

    let identity_dirty = *name_field.read() != user.username || *email_field.read() != user.email;
    let status_dirty = *status_field.read() != user.status;
    let grants_dirty = *chosen.read() != granted_now;
    let dirty = (can_write && (identity_dirty || status_dirty)) || (can_grant && grants_dirty);

    // Up to three calls, run in order, stopping at the first failure — a rejected rename never
    // silently applies a grant change alongside it.
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
            let client = gate.client(api);
            spawn(async move {
                let mut failure = None;
                // A step-up demand is not a failure to report: it stops the chain and opens the
                // prompt, so the two later calls do not run against an elevation the first one
                // has just proved absent.
                let mut demanded = false;
                if let Some(body) = identity {
                    if let Err(e) = client.update_user().id(id.clone()).body(body).send().await {
                        demanded = gate.refused(api::Refusal::of(&e));
                        if !demanded {
                            failure = Some(api::guarded_error(i18n, e));
                        }
                    }
                }
                if failure.is_none() && !demanded {
                    if let Some(body) = status {
                        if let Err(e) = client
                            .set_user_status()
                            .id(id.clone())
                            .body(body)
                            .send()
                            .await
                        {
                            demanded = gate.refused(api::Refusal::of(&e));
                            if !demanded {
                                failure = Some(api::guarded_error(i18n, e));
                            }
                        }
                    }
                }
                if failure.is_none() && !demanded {
                    if let Some(body) = grants {
                        if let Err(e) = client
                            .set_user_permissions()
                            .id(id.clone())
                            .body(body)
                            .send()
                            .await
                        {
                            demanded = gate.refused(api::Refusal::of(&e));
                            if !demanded {
                                failure = Some(api::guarded_error(i18n, e));
                            }
                        }
                    }
                }
                if demanded {
                    // The prompt is on screen; the operator presses Save again once confirmed,
                    // and the fields still hold everything they typed.
                } else if let Some(message) = failure {
                    outcome.set(Some(Err(message)));
                } else {
                    outcome.set(Some(Ok(i18n.t("console.users.saved"))));
                    suspend_reason.set(String::new());
                    detail_reload.bump();
                    reload.bump();
                }
                busy.release();
            });
        }
    };

    // One clone per consumer: `rsx!` moves each into its own closure or child component.
    let (id_header, id_sessions, id_verify, id_sync, id_erase) =
        (id.clone(), id.clone(), id.clone(), id.clone(), id);
    let owner = is_super_user(&data.permissions);
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
                        if owner {
                            span { class: "ik-pill star", style: "font-size:10px;",
                                {i18n.t("console.users.role.owner")}
                            }
                        } else {
                            span { class: if staff { "ik-pill acc" } else { "ik-pill" }, style: "font-size:10px;",
                                if staff {
                                    {i18n.t("console.users.role.staff")}
                                } else {
                                    {i18n.t("console.users.role.reader")}
                                }
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
                                revoke_all(
                                    api,
                                    i18n,
                                    busy,
                                    outcome,
                                    detail_reload,
                                    id_header.clone(),
                                    gate,
                                );
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
            TabBar {
                selected: tab,
                on_select: move |next: Tab| nav.filter(nav.query().with_tab(next.token())),
                flush: true,
            }
        }
        div { style: "padding:0 22px;",
            if is_self {
                p { class: "ik-muted", style: "font-size:12px;margin:12px 0 0;",
                    {i18n.t("console.users.selfNotice")}
                }
            }
            if gate.is_open() {
                StepUpPrompt {
                    enrolled: true,
                    intro: Some(i18n.t("console.stepUp.intro")),
                    on_done: move |()| {
                        gate.close();
                        outcome.set(Some(Ok(i18n.t("stepUp.confirmedRetry"))));
                    },
                }
            }
            OutcomeLine { outcome: outcome.read().clone() }
        }
        match tab {
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
                                            gate,
                                        }
                                    }
                                }
                                div { class: "ik-kv narrow",
                                    span { class: "k", {i18n.t("console.users.status")} }
                                    SegControl {
                                        options: vec![
                                            (
                                                AccountStatus::Active.to_string(),
                                                i18n.t(AccountStatus::Active.label_key()),
                                            ),
                                            (
                                                AccountStatus::Suspended.to_string(),
                                                i18n.t(AccountStatus::Suspended.label_key()),
                                            ),
                                        ],
                                        selected: status_field.read().to_string(),
                                        disabled: !can_write || is_self,
                                        on_select: move |value: String| {
                                            if let Ok(next) = value.parse() {
                                                status_field.set(next);
                                            }
                                        },
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
                                owner,
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
                                                    gate,
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
                            Kpi {
                                label: i18n.t("console.users.stat.tracked"),
                                value: thousands(user.tracked_count),
                                large: true,
                            }
                            Kpi {
                                label: i18n.t("console.users.stat.linked"),
                                value: thousands(user.linked_accounts),
                                large: true,
                            }
                            Kpi {
                                label: i18n.t("console.users.stat.sessions"),
                                value: thousands(user.active_sessions),
                                large: true,
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
                                                gate,
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
                        RecentActions { user_id: user.id.clone() }
                    }
                }
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grant(permission: &str, known: bool) -> GrantRow {
        GrantRow {
            granted_at: "2026-07-29T00:00:00Z".to_owned(),
            granted_by: None,
            known,
            permission: permission.to_owned(),
        }
    }

    /// A grant left over from a build that had a capability this one does not must stay out of
    /// the editable set: it is surfaced separately as an "unknown grant", and folding it in
    /// would re-submit an inert token on the next save as if it were current.
    #[test]
    fn only_recognised_tokens_enter_the_editable_set() {
        let rows = vec![
            grant("users.read", true),
            grant("providers.write", true),
            grant("timetravel.admin", false),
        ];
        let known = known_permissions(&rows);
        assert_eq!(
            known,
            BTreeSet::from([Permission::UsersRead, Permission::ProvidersWrite])
        );
    }

    /// `known: true` is the server's claim, not this build's. A token it cannot parse is
    /// dropped rather than panicking, so a server ahead of this frontend degrades quietly.
    #[test]
    fn a_token_this_build_cannot_parse_is_dropped_even_when_flagged_known() {
        let known = known_permissions(&[grant("not.a.permission", true)]);
        assert!(known.is_empty());
    }
}
