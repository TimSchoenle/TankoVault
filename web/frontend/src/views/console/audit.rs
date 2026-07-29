//! The privileged-action audit trail (design §16): recent operator actions, newest first.

use crate::api;
use crate::components::async_block_list;
use crate::i18n::use_i18n;
use crate::models::*;
use crate::util::rel_time;
use crate::views::console::RefreshTick;
use dioxus::prelude::*;
use progenitor_client::ResponseValue;

/// Privileged-action audit trail (design §16): recent operator actions, newest first.
#[component]
pub(super) fn AuditPanel(tick: RefreshTick) -> Element {
    let api = api::use_api();
    let i18n = use_i18n();
    let res = use_resource(move || {
        tick.track();
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

    let empty = i18n.t("console.audit.empty");
    rsx! {
        section { style: "margin-bottom:18px;",
            h3 { {i18n.t("console.audit.title")} }
            {
                async_block_list(
                    &res,
                    tick.reload(),
                    80,
                    &empty,
                    |rows| {
                        let rows = rows.to_vec();
                        rsx! {
                            div { class: "ik-tablewrap",
                                table { class: "ik-table ik-table-compact",
                                    thead {
                                        tr {
                                            th { {i18n.t("console.audit.col.when")} }
                                            th { {i18n.t("console.audit.col.actor")} }
                                            th { {i18n.t("console.audit.col.action")} }
                                            th { {i18n.t("console.audit.col.target")} }
                                        }
                                    }
                                    tbody {
                                        for a in rows {
                                            AuditRow { key: "{a.id}", entry: Signal::new(a) }
                                        }
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

#[component]
pub(super) fn AuditRow(entry: Signal<AuditEntry>) -> Element {
    let i18n = use_i18n();
    let a = entry.read();
    let actor = a
        .actor
        .clone()
        .unwrap_or_else(|| i18n.t("console.audit.system"));
    let target = a.target.clone().unwrap_or_else(|| i18n.t("time.unknown"));
    rsx! {
        tr {
            td { class: "ik-muted ik-mono", style: "font-size:12px;white-space:nowrap;",
                "{rel_time(i18n, Some(a.created_at.as_str()))}"
            }
            td { "{actor}" }
            td { span { class: "ik-pill", "{a.action}" } }
            td { class: "ik-mono ik-muted", style: "font-size:12px;word-break:break-all;", "{target}" }
        }
    }
}
