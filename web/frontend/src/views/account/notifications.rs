//! Notification preferences (§9.4) — a set of on/off toggles persisted verbatim as the open
//! `notification_prefs` JSON document.

use super::PanelCard;
use crate::api;
use crate::components::{OutcomeLine, SkeletonBlock};
use crate::hooks::use_outcome;
use crate::icons::Icon;
use dioxus::prelude::*;
use serde_json::Value;

/// The toggles the panel exposes. Stored as booleans in the open prefs document; an absent
/// key means enabled, so a reader who has never opened this panel gets everything.
const KEYS: [(&str, &str); 3] = [
    ("new_chapters", "New chapters in your watchlist"),
    ("email", "Email notifications"),
    ("digest", "Weekly digest"),
];

#[component]
pub(crate) fn NotificationsPanel() -> Element {
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
                    outcome.set(Some(Err(api::friendly_error(e))));
                    Value::Object(serde_json::Map::new())
                }
            };
            prefs.set(Some(loaded));
        });
    });

    let Some(current) = prefs.read().clone() else {
        return rsx! {
            PanelCard { icon: Icon::Notify, title: "Notification preferences",
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
                Ok(_) => outcome.set(Some(Ok("Preferences saved.".to_owned()))),
                Err(e) => outcome.set(Some(Err(api::friendly_error(e)))),
            }
        });
    };

    rsx! {
        PanelCard { icon: Icon::Notify, title: "Notification preferences",
            for (key , label) in KEYS {
                {
                    let on = current.get(key).and_then(Value::as_bool).unwrap_or(true);
                    rsx! {
                        div { class: "ik-row", key: "{key}",
                            span { class: "grow", "{label}" }
                            button {
                                class: if on { "ik-btn primary" } else { "ik-btn" },
                                "aria-pressed": on,
                                onclick: move |_| toggle(key, on),
                                if on { "On" } else { "Off" }
                            }
                        }
                    }
                }
            }
            OutcomeLine { outcome: outcome.read().clone() }
        }
    }
}
