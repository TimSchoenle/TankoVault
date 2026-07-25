//! Account & settings (`DESIGN_SPEC` §7.7) — a settings shell with a sub-nav and one panel per
//! concern. Each panel owns its own data and lives in its own module.

mod appearance;
mod callback;
mod notifications;
mod profile;
mod security;
mod sync;

pub(crate) use callback::AnilistCallback;

use crate::api;
use crate::components::SignInGate;
use crate::state::use_session;
use dioxus::prelude::*;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Panel {
    Profile,
    Appearance,
    Security,
    Sync,
    Notifications,
}

impl Panel {
    const ALL: [Panel; 5] = [
        Self::Profile,
        Self::Appearance,
        Self::Security,
        Self::Sync,
        Self::Notifications,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::Profile => "Profile",
            Self::Appearance => "Appearance",
            Self::Security => "Security & sessions",
            Self::Sync => "Sync & integrations",
            Self::Notifications => "Notification prefs",
        }
    }
}

#[component]
pub(crate) fn Account() -> Element {
    let session = use_session();
    let api = api::use_api();
    let mut panel = use_signal(|| Panel::Profile);

    if !session.is_authenticated() {
        return rsx! {
            h1 { class: "ik-page-title", "Account" }
            SignInGate {}
        };
    }

    let name = session.username().unwrap_or_else(|| "reader".to_owned());
    let role = session.role.read().label();
    let current = *panel.read();

    let sign_out = move |_| {
        let client = api.client();
        spawn(async move {
            // Clear locally regardless of the response: the refresh cookie may already be
            // gone, and leaving the reader "signed in" after they asked not to be would be
            // worse than a revocation call that quietly failed.
            let _ = client.logout().send().await;
            session.clear();
        });
    };

    rsx! {
        div { class: "ik-page-head",
            h1 { class: "ik-page-title", "Account" }
            button { class: "ik-btn", onclick: sign_out, "Sign out" }
        }
        div { class: "ik-tabs",
            for entry in Panel::ALL {
                button {
                    key: "{entry.label()}",
                    class: if current == entry { "ik-tab active" } else { "ik-tab" },
                    onclick: move |_| panel.set(entry),
                    "{entry.label()}"
                }
            }
        }
        match current {
            Panel::Profile => rsx! { profile::ProfilePanel { name: name.clone(), role } },
            Panel::Appearance => rsx! { appearance::AppearancePanel {} },
            Panel::Security => rsx! { security::SecurityPanel {} },
            Panel::Sync => rsx! { sync::SyncPanel {} },
            Panel::Notifications => rsx! { notifications::NotificationsPanel {} },
        }
    }
}

/// The shared card chrome every panel sits in: an icon + title header and a body.
#[component]
fn PanelCard(icon: crate::icons::Icon, title: &'static str, children: Element) -> Element {
    rsx! {
        div { class: "ik-sidebar-card", style: "max-width:560px;",
            div { class: "ik-flex", style: "margin-bottom:12px;",
                crate::icons::Ic { icon, size: 18 }
                strong { "{title}" }
            }
            {children}
        }
    }
}
