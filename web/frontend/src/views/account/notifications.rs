//! Notification preferences (§9.4) — a set of on/off toggles persisted verbatim as the open
//! `notification_prefs` JSON document.

use super::PanelCard;
use crate::api;
use crate::components::{OutcomeLine, SkeletonBlock};
use crate::hooks::use_outcome;
use crate::i18n::use_i18n;
use crate::icons::Icon;
use dioxus::prelude::*;
use serde_json::Value;

/// The toggles the panel exposes: the key in the open prefs document, and the catalogue key
/// wording it. Stored as booleans; an absent key means enabled, so a reader who has never
/// opened this panel gets everything.
const KEYS: [(&str, &str); 3] = [
    ("new_chapters", "account.notifications.newChapters"),
    ("email", "account.notifications.email"),
    ("digest", "account.notifications.digest"),
];

#[component]
pub(crate) fn NotificationsPanel() -> Element {
    let i18n = use_i18n();
    let api = api::use_api();
    let mut outcome = use_outcome();
    let mut prefs = use_signal(|| Option::<Value>::None);

    use_effect(move || {
        let client = api.client();
        spawn(async move {
            let loaded = match client.notification_prefs().send().await {
                Ok(response) => response.into_inner(),
                // A failed load must not silently present "everything on" as the reader's
                // saved state — that would invite them to toggle against a phantom baseline.
                Err(e) => {
                    outcome.set(Some(Err(api::friendly_error(i18n, e))));
                    Value::Object(serde_json::Map::new())
                }
            };
            prefs.set(Some(loaded));
        });
    });

    let Some(current) = prefs.read().clone() else {
        return rsx! {
            PanelCard { icon: Icon::Notify, title: i18n.t("account.notifications.title"),
                SkeletonBlock { height: 80 }
            }
        };
    };

    let mut toggle = move |key: &'static str, on: bool| {
        let mut next = prefs
            .peek()
            .clone()
            .unwrap_or(Value::Object(serde_json::Map::new()));
        if !next.is_object() {
            next = Value::Object(serde_json::Map::new());
        }
        if let Some(object) = next.as_object_mut() {
            object.insert(key.to_owned(), Value::Bool(!on));
        }
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

    rsx! {
        PanelCard { icon: Icon::Notify, title: i18n.t("account.notifications.title"),
            for (key , label_key) in KEYS {
                {
                    let on = current.get(key).and_then(Value::as_bool).unwrap_or(true);
                    rsx! {
                        div { class: "ik-row", key: "{key}",
                            span { class: "grow", {i18n.t(label_key)} }
                            button {
                                class: if on { "ik-btn primary" } else { "ik-btn" },
                                "aria-pressed": on,
                                onclick: move |_| toggle(key, on),
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
            OutcomeLine { outcome: outcome.read().clone() }
        }
    }
}
