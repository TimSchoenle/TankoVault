//! Owns Console · Users: the account directory and a full inspector per account.
//!
//! Off the shared auto-refresh tick: a background refetch mid-edit would discard whatever the
//! operator is typing. Reloads only after its own writes, or a manual reload.
//!
//! Per-device session rows and per-account export are absent: no endpoint exists for either.
//! TODO(api): `GET /v1/admin/users/:id/sessions`, `GET /v1/admin/users/:id/export`.

mod activity;
mod editor;
mod grants;
mod row;
mod sync;
mod tabs;
mod verify;

use crate::api;
use crate::components::{
    async_view, CompactPager, ListSearch, NoSelection, OutcomeLine, SkeletonBlock, StepUpGuard,
    TabBar, TabKind, Window,
};
use crate::hooks::{use_reload, Reload};
use crate::i18n::use_i18n;
use crate::icons::{Ic, Icon};
use crate::models::AccountStatusExt as _;
use crate::util::{initial, iso_date, rel_time, thousands};
use crate::views::console::{landing_selection, use_console_nav, ConsoleQuery};
use crate::wire::types::{AccountStatus, DirectoryRow, GrantRow, Permission, UserDetailResponse};
use activity::RecentActions;
use dioxus::prelude::*;
use editor::{Editor, Rights};
use inkstone_ui::{Button, Pill, Size, Tone};
use progenitor_client::ResponseValue;
use row::UserRow;
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

    /// An unrecognised token opens the default tab instead of refusing the link.
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

    /// An unrecognised token shows every account, not silently hiding most of the
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
/// Unknown tokens are surfaced separately as "unknown grant", not dropped, but must stay
/// out of this set; including one would re-submit an inert token as current on save.
fn known_permissions(grants: &[GrantRow]) -> BTreeSet<Permission> {
    grants
        .iter()
        .filter(|g| g.known)
        .filter_map(|g| serde_json::from_value::<Permission>(serde_json::json!(g.permission)).ok())
        .collect()
}

/// Whether these grants are the deployment owner's.
///
/// Asked of the grant list, not of a count, because the super user holds exactly one
/// grant: counting says "an operator with one capability", which is the opposite of the truth.
/// The token is also absent from the permission catalogue by design, so the checklist below
/// renders it nowhere: hence the explicit notice this drives.
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
    // directory. Pagination arithmetic must use `page_len`, not `rows.len()`: `rows.len()`
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
    // stale link opens a lit row instead of an inspector with nothing selected beside it.
    let current = view
        .sel
        .clone()
        .filter(|id| rows.iter().any(|row| &row.id == id))
        .or_else(|| rows.first().map(|r| r.id.clone()));

    // …and the fallback goes into the URL, so the address names the account on screen,
    // not whichever one this page of the directory happens to start with. It replaces,
    // not pushes (the operator did not choose this row and must not have to back out of it),
    // and the next query is built out here, not inside the effect, because reading
    // `nav.query()` in there would subscribe the effect to the memo the write changes.
    let landing = landing_selection(view.sel.as_deref(), current.clone())
        .map(|sel| view.with_selection(Some(sel)));
    use_effect(use_reactive!(|landing| {
        if let Some(next) = landing {
            nav.filter(next);
        }
    }));

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
                    Button {
                        size: Size::Xs,
                        style: "margin-left:auto;",
                        on_click: move |_| reload.bump(),
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
    let editor = Editor::use_new(&data, reload, detail_reload);
    let Editor {
        i18n,
        busy,
        outcome,
        gate,
        ..
    } = editor;
    let nav = use_console_nav();
    let tab = Tab::parse(nav.query().tab_token());
    let user = data.user.clone();
    let rights = Rights::use_for(&user);
    let changes = editor.changes(&data, rights);

    let save = {
        let user = user.clone();
        move |_| {
            // Cloned per click: the gate keeps the action to replay it, so what it holds cannot
            // be something moved out of the handler.
            let user = user.clone();
            gate.attempt(move || editor.save(&user, changes));
        }
    };

    let owner = is_super_user(&data.permissions);
    let staff = !data.permissions.is_empty();
    let joined = iso_date(Some(&user.created_at)).to_owned();
    let last_seen = rel_time(i18n, user.last_login_at.as_deref());
    // The id is a uuid; its leading group is enough to recognise an account by, and the whole
    // thing would crowd the meta line.
    let short_id = user.id.get(0..8).unwrap_or(&user.id).to_owned();
    let revoke_id = user.id.clone();

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
                            Pill {
                                class: "star",
                                {i18n.t("console.users.role.owner")}
                            }
                        } else {
                            span { class: if staff { "ik-pill acc" } else { "ik-pill" },
                                if staff {
                                    {i18n.t("console.users.role.staff")}
                                } else {
                                    {i18n.t("console.users.role.reader")}
                                }
                            }
                        }
                        if user.status == AccountStatus::Suspended {
                            span { class: user.status.pill_class(),
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
                    if rights.sessions {
                        Button {
                            size: Size::Sm,
                            disabled: busy.is_busy() || user.active_sessions == 0,
                            on_click: move |_| {
                                let id = revoke_id.clone();
                                gate.attempt(move || editor.revoke_sessions(id.clone()));
                            },
                            {i18n.t("console.users.revokeSessions")}
                        }
                    }
                    if (rights.write || rights.grant) && !rights.is_self {
                        Button {
                            size: Size::Sm,
                            tone: Tone::Primary,
                            disabled: busy.is_busy() || !changes.any(),
                            on_click: save,
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
            if rights.is_self {
                p { class: "ik-muted", style: "font-size:12px;margin:12px 0 0;",
                    {i18n.t("console.users.selfNotice")}
                }
            }
            StepUpGuard { gate, intro: Some(i18n.t("console.stepUp.intro")) }
            OutcomeLine { outcome: outcome.read().clone() }
        }
        match tab {
            Tab::Identity => tabs::identity(&editor, &data, rights),
            Tab::Sessions => tabs::sessions(&editor, &user, rights),
            Tab::Library => tabs::library(i18n, &user),
            Tab::Privacy => tabs::privacy(&editor, &user, rights, on_erased),
            Tab::Activity => rsx! {
                div { class: "ik-cons-inspbody",
                    div { class: "ik-cons-col", style: "max-width:620px;",
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
    /// dropped instead of panicking, so a server ahead of this frontend degrades quietly.
    #[test]
    fn a_token_this_build_cannot_parse_is_dropped_even_when_flagged_known() {
        let known = known_permissions(&[grant("not.a.permission", true)]);
        assert!(known.is_empty());
    }
}
