//! The inspector's tab bodies, one per tab.

use dioxus::prelude::*;
use inkstone_ui::{Button, Size};

use super::editor::{Editor, Rights};
use super::grants::PermissionGrants;
use super::is_super_user;
use super::sync::ExternalSync;
use super::verify::VerifyEmailAction;
use crate::components::{Kpi, Section, SegControl, TypeToConfirm};
use crate::i18n::Translator;
use crate::icons::{Ic, Icon};
use crate::models::AccountStatusExt as _;
use crate::util::{iso_date, thousands};
use crate::wire::types::{AccountStatus, UserDetail, UserDetailResponse};

/// Identity: the name, address, verification and status fields, the grants and the tracker links.
pub(super) fn identity(editor: &Editor, data: &UserDetailResponse, rights: Rights) -> Element {
    let Editor {
        i18n,
        gate,
        reload,
        detail_reload,
        mut name,
        mut email,
        mut status,
        mut suspend_reason,
        chosen,
        ..
    } = *editor;
    let user = &data.user;
    let joined = iso_date(Some(&user.created_at)).to_owned();
    let owner = is_super_user(&data.permissions);
    rsx! {
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
                                disabled: !rights.write || rights.is_self,
                                value: "{name}",
                                oninput: move |e| name.set(e.value()),
                            }
                        }
                        div { class: "ik-kv narrow",
                            label { class: "k", r#for: "tv-u-email", {i18n.t("auth.field.email")} }
                            input {
                                id: "tv-u-email",
                                class: "ik-input",
                                style: "font-size:12.5px;padding:9px 11px;",
                                r#type: "email",
                                disabled: !rights.write || rights.is_self,
                                value: "{email}",
                                oninput: move |e| email.set(e.value()),
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
                                    user_id: user.id.clone(),
                                    enabled: rights.write && !rights.is_self,
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
                                selected: status.read().to_string(),
                                disabled: !rights.write || rights.is_self,
                                on_select: move |value: String| {
                                    if let Ok(next) = value.parse() {
                                        status.set(next);
                                    }
                                },
                            }
                        }
                        if *status.read() == AccountStatus::Suspended && *status.read() != user.status {
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
                if rights.grant {
                    PermissionGrants {
                        grants: data.permissions.clone(),
                        chosen,
                        editable: !rights.is_self,
                        owner,
                    }
                }
            }
            div { class: "ik-cons-col",
                ExternalSync {
                    user_id: user.id.clone(),
                    username: user.username.clone(),
                    editable: rights.sync,
                    gate,
                }
            }
        }
    }
}

/// Sessions: the live session count and signing the account out everywhere.
pub(super) fn sessions(editor: &Editor, user: &UserDetail, rights: Rights) -> Element {
    let editor = *editor;
    let Editor {
        i18n, busy, gate, ..
    } = editor;
    let user_id = user.id.clone();
    rsx! {
        div { class: "ik-cons-inspbody",
            div { class: "ik-cons-col", style: "max-width:620px;",
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
                            if rights.sessions {
                                Button {
                                    size: Size::Xs,
                                    style: "margin-left:auto;",
                                    disabled: busy.is_busy() || user.active_sessions == 0,
                                    on_click: move |_| {
                                        let id = user_id.clone();
                                        gate.attempt(move || editor.revoke_sessions(id.clone()));
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
    }
}

/// Library: what the account tracks and links.
pub(super) fn library(i18n: Translator, user: &UserDetail) -> Element {
    rsx! {
        div { class: "ik-cons-inspbody",
            div { class: "ik-cons-col",
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
    }
}

/// Privacy: open privacy requests and erasing the account.
pub(super) fn privacy(
    editor: &Editor,
    user: &UserDetail,
    rights: Rights,
    on_erased: EventHandler<()>,
) -> Element {
    let editor = *editor;
    let Editor {
        i18n,
        busy,
        gate,
        mut erase_reason,
        ..
    } = editor;
    let user = user.clone();
    rsx! {
        div { class: "ik-cons-inspbody",
            div { class: "ik-cons-col", style: "max-width:620px;",
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
                    if rights.delete && !rights.is_self {
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
                                    let (id, name, reason) = (
                                        user.id.clone(),
                                        user.username.clone(),
                                        erase_reason.peek().trim().to_owned(),
                                    );
                                    gate.attempt(move || {
                                        editor.erase(id.clone(), name.clone(), reason.clone(), on_erased);
                                    });
                                },
                            }
                        }
                    }
                }
            }
        }
    }
}
