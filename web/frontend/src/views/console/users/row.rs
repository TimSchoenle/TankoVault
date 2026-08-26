//! One row of the user directory list pane.

use crate::i18n::use_i18n;
use crate::models::AccountStatusExt as _;
use crate::util::{initial, thousands};
use crate::wire::types::{AccountStatus, DirectoryRow};
use dioxus::prelude::*;
use inkstone_ui::{Pill, Tone};

/// One directory row: who they are, what they hold, and the state their account is in.
#[component]
pub(super) fn UserRow(row: DirectoryRow, selected: bool, on_pick: EventHandler<String>) -> Element {
    let i18n = use_i18n();
    let id = row.id.clone();
    // The super user holds exactly one grant, so a count-based test renders the deployment owner
    // as an ordinary operator with a single capability. The flag is what distinguishes them.
    let owner = row.is_super_user;
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
                    span { class: "ik-mono", style: "display:block;font-size:12.5px;color:var(--muted);margin-top:2px;overflow:hidden;text-overflow:ellipsis;white-space:nowrap;",
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
                    if owner {
                        Pill { class: "star",
                            {i18n.t("console.users.role.owner")}
                        }
                    } else if staff && !suspended {
                        Pill { tone: Tone::Accent,
                            {i18n.t("console.users.role.staff")}
                        }
                    }
                    span { class: row.status.pill_class(),
                        {i18n.t(row.status.label_key())}
                    }
                }
            }
        }
    }
}
