//! What this account has actually done, read out of the privileged-action trail.

use crate::api;
use crate::components::{async_view, Section, SkeletonBlock};
use crate::hooks::use_reload;
use crate::i18n::use_i18n;
use crate::state::capabilities::use_capabilities;
use crate::util::rel_time;
use crate::wire::types::Permission;
use dioxus::prelude::*;
use progenitor_client::ResponseValue;

/// How many of this account's actions the panel shows.
const RECENT: u32 = 10;

/// What this account has actually done, out of the privileged-action trail.
///
/// Filtered by `?actor=` on the server. It used to fetch the global forty most recent records
/// and keep the ones whose `actor` string matched this username — so an operator who had not
/// acted inside the last forty actions *system-wide* rendered "no recent actions". The panel
/// was silently wrong rather than merely shallow, and on a deployment with a nightly sweep it
/// was wrong for nearly everyone.
#[component]
pub(super) fn RecentActions(user_id: String) -> Element {
    let api = api::use_api();
    let i18n = use_i18n();
    let caps = use_capabilities();
    let reload = use_reload();

    if !caps.can(Permission::AuditRead) {
        return rsx! {};
    }

    let entries = use_resource(move || {
        reload.track();
        let actor = user_id.clone();
        let client = api.client();
        async move {
            client
                .audit_log()
                .actor(actor)
                .limit(RECENT)
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
                    |page| {
                        if page.items.is_empty() {
                            return rsx! {
                                p { class: "ik-muted", style: "font-size:12px;margin:0;",
                                    {i18n.t("console.users.noRecentActions")}
                                }
                            };
                        }
                        rsx! {
                            div { class: "ik-timeline",
                                for entry in page.items.clone() {
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
