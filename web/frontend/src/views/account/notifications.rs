//! Notification preferences (§9.4) — what to be told about, for which series, and where.
//!
//! Every switch here reaches the notifier. The panel used to write three keys into a free-form
//! JSON document that nothing on the server ever read, so turning "new chapters" off changed
//! nothing at all; the document is a typed, validated contract now, and the fan-out consults it
//! before it writes a row.

use crate::api;
use crate::components::{OutcomeLine, PanelCard, Section, SkeletonBlock};
use crate::hooks::use_outcome;
use crate::i18n::use_i18n;
use crate::icons::Icon;
use crate::models::NotificationPrefs;
use dioxus::prelude::*;
use inkstone_ui::ToggleButton;

/// One switch: its catalogue key, how to read it, and how to flip it.
///
/// A table rather than a `match` per row so the sections below are declarations — adding a switch
/// is one line, and no section can silently render a control that writes to the wrong field.
struct Switch {
    label_key: &'static str,
    hint_key: Option<&'static str>,
    read: fn(&NotificationPrefs) -> bool,
    write: fn(&mut NotificationPrefs, bool),
}

const CHANNELS: &[Switch] = &[
    Switch {
        label_key: "account.notifications.channel.inApp",
        hint_key: Some("account.notifications.channel.inAppHint"),
        read: |p| p.channels.in_app,
        write: |p, v| p.channels.in_app = v,
    },
    Switch {
        label_key: "account.notifications.channel.live",
        hint_key: Some("account.notifications.channel.liveHint"),
        read: |p| p.channels.live,
        write: |p, v| p.channels.live = v,
    },
];

const KINDS: &[Switch] = &[
    Switch {
        label_key: "account.notifications.kind.newChapter",
        hint_key: None,
        read: |p| p.kinds.new_chapter,
        write: |p, v| p.kinds.new_chapter = v,
    },
    Switch {
        label_key: "account.notifications.kind.sourceAdded",
        hint_key: None,
        read: |p| p.kinds.source_added,
        write: |p, v| p.kinds.source_added = v,
    },
    Switch {
        label_key: "account.notifications.kind.seriesCompleted",
        hint_key: None,
        read: |p| p.kinds.series_completed,
        write: |p, v| p.kinds.series_completed = v,
    },
    Switch {
        label_key: "account.notifications.kind.syncConflict",
        hint_key: None,
        read: |p| p.kinds.sync_conflict,
        write: |p, v| p.kinds.sync_conflict = v,
    },
    Switch {
        label_key: "account.notifications.kind.announcement",
        hint_key: None,
        read: |p| p.kinds.announcement,
        write: |p, v| p.kinds.announcement = v,
    },
];

/// The watchlist statuses, in the order the watchlist itself lists them.
const STATUSES: &[Switch] = &[
    Switch {
        label_key: "enum.watchStatus.reading",
        hint_key: None,
        read: |p| p.watch_status.reading,
        write: |p, v| p.watch_status.reading = v,
    },
    Switch {
        label_key: "enum.watchStatus.planned",
        hint_key: None,
        read: |p| p.watch_status.planned,
        write: |p, v| p.watch_status.planned = v,
    },
    Switch {
        label_key: "enum.watchStatus.paused",
        hint_key: None,
        read: |p| p.watch_status.paused,
        write: |p, v| p.watch_status.paused = v,
    },
    Switch {
        label_key: "enum.watchStatus.completed",
        hint_key: None,
        read: |p| p.watch_status.completed,
        write: |p, v| p.watch_status.completed = v,
    },
    Switch {
        label_key: "enum.watchStatus.dropped",
        hint_key: None,
        read: |p| p.watch_status.dropped,
        write: |p, v| p.watch_status.dropped = v,
    },
];

const DELIVERY: &[Switch] = &[
    Switch {
        label_key: "account.notifications.groupUnread",
        hint_key: Some("account.notifications.groupUnreadHint"),
        read: |p| p.group_unread,
        write: |p, v| p.group_unread = v,
    },
    Switch {
        label_key: "account.notifications.quietHours",
        hint_key: Some("account.notifications.quietHoursHint"),
        read: |p| p.quiet_hours.enabled,
        write: |p, v| p.quiet_hours.enabled = v,
    },
];

#[component]
pub(crate) fn NotificationsPanel() -> Element {
    let i18n = use_i18n();
    let api = api::use_api();
    let mut outcome = use_outcome();
    let mut prefs = use_signal(|| Option::<NotificationPrefs>::None);

    use_effect(move || {
        let client = api.client();
        spawn(async move {
            match client.notification_prefs().send().await {
                Ok(response) => prefs.set(Some(response.into_inner())),
                // Don't silently present the defaults as saved state on a failed load — that
                // invites toggling against a phantom baseline.
                Err(e) => outcome.set(Some(Err(api::friendly_error(i18n, e)))),
            }
        });
    });

    let Some(current) = prefs.read().clone() else {
        return rsx! {
            PanelCard { icon: Icon::Notify, title: i18n.t("account.notifications.title"),
                SkeletonBlock { height: 160 }
            }
        };
    };

    let mut toggle = move |switch: &'static Switch, on: bool| {
        let Some(mut next) = prefs.peek().clone() else {
            return;
        };
        (switch.write)(&mut next, !on);
        // Optimistic: flip locally so the control responds immediately, then reconcile.
        prefs.set(Some(next.clone()));
        outcome.set(None);
        let client = api.client();
        spawn(async move {
            match client.put_notification_prefs().body(next).send().await {
                Ok(_) => outcome.set(Some(Ok(i18n.t("account.notifications.saved")))),
                Err(e) => outcome.set(Some(Err(api::friendly_error(i18n, e)))),
            }
        });
    };

    let group = move |label_key: &'static str, switches: &'static [Switch]| {
        let current = current.clone();
        rsx! {
            Section { label: i18n.t(label_key),
                for switch in switches {
                    {
                        let on = (switch.read)(&current);
                        rsx! {
                            div { class: "ik-row", key: "{switch.label_key}",
                                div { class: "grow",
                                    div { {i18n.t(switch.label_key)} }
                                    if let Some(hint) = switch.hint_key {
                                        div { class: "ik-muted", style: "font-size:12px;", {i18n.t(hint)} }
                                    }
                                }
                                ToggleButton {
                                    on,
                                    on_toggle: move |_| toggle(switch, on),
                                    if on {
                                        {i18n.t("common.on")}
                                    } else {
                                        {i18n.t("common.off")}
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    };

    rsx! {
        PanelCard { icon: Icon::Notify, title: i18n.t("account.notifications.title"),
            {group("account.notifications.section.kinds", KINDS)}
            {group("account.notifications.section.series", STATUSES)}
            {group("account.notifications.section.channels", CHANNELS)}
            {group("account.notifications.section.delivery", DELIVERY)}
            OutcomeLine { outcome: outcome.read().clone() }
        }
    }
}
