//! The registered-user directory: identity, RBAC role and tracked-series count.
//! Read-only — role management has no endpoint yet.

use crate::api;
use crate::state::use_session;
use crate::util::thousands;
use crate::views::console::RefreshTick;
use dioxus::prelude::*;
use progenitor_client::ResponseValue;

/// Users tab (`DESIGN_SPEC` §7.8.7): the registered-user directory from `GET /v1/admin/users`
/// (§9.5) — identity, RBAC role, and how many series each user tracks — plus the aggregate
/// count. Read-only (role management has no endpoint yet).
#[component]
pub(super) fn UsersPanel(tick: RefreshTick) -> Element {
    let api = api::use_api();
    let session = use_session();
    let res = use_resource(move || {
        tick.track();
        let client = api.client();
        async move {
            if session.is_authenticated() {
                Some(
                    client
                        .list_users()
                        .send()
                        .await
                        .map(ResponseValue::into_inner)
                        .map_err(api::friendly_error),
                )
            } else {
                None
            }
        }
    });

    let body = match &*res.read_unchecked() {
        None | Some(None) => rsx! { div { class: "ik-skeleton", style: "height:120px;" } },
        Some(Some(Err(e))) => rsx! {
            p { class: "ik-muted", style: "font-size:13px;", "Could not load users: {e}" }
        },
        Some(Some(Ok(list))) if list.is_empty() => rsx! {
            div { class: "ik-empty", "No users registered yet." }
        },
        Some(Some(Ok(list))) => {
            let count = list.len();
            let rows = list.clone();
            rsx! {
                div { class: "ik-kpis", style: "margin-bottom:14px;",
                    div { class: "ik-kpi",
                        div { class: "ik-kpi-label", "Registered users" }
                        div { class: "ik-kpi-value", "{thousands(count as i64)}" }
                    }
                }
                div { class: "ik-tablewrap",
                    table { class: "ik-table ik-table-compact",
                        thead {
                            tr {
                                th { "User" }
                                th { "Email" }
                                th { "Role" }
                                th { style: "text-align:right;", "Tracked" }
                                th { "Joined" }
                            }
                        }
                        tbody {
                            for u in rows {
                                UserRowView { key: "{u.id}", user: Signal::new(u) }
                            }
                        }
                    }
                }
            }
        }
    };

    rsx! {
        section { style: "margin-bottom:18px;",
            h3 { "Users" }
            {body}
        }
    }
}

#[component]
pub(super) fn UserRowView(user: Signal<crate::models::UserRow>) -> Element {
    let u = user.read();
    let joined = u.created_at.get(0..10).unwrap_or("").to_owned();
    let role_class = match u.role.as_str() {
        "admin" => "ik-pill acc",
        "operator" => "ik-pill jade",
        _ => "ik-pill",
    };
    rsx! {
        tr {
            td { "{u.username}" }
            td { class: "ik-mono ik-muted", style: "font-size:12px;", "{u.email}" }
            td { span { class: "{role_class}", "{u.role}" } }
            td { class: "ik-mono", style: "text-align:right;", "{thousands(u.tracked_count)}" }
            td { class: "ik-mono ik-muted", style: "font-size:12px;", "{joined}" }
        }
    }
}
