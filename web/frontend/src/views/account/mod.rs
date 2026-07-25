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
use crate::i18n::use_i18n;
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

    /// The catalogue key of this panel's tab label (see [`crate::i18n`]).
    fn label_key(self) -> &'static str {
        match self {
            Self::Profile => "account.tab.profile",
            Self::Appearance => "account.tab.appearance",
            Self::Security => "account.tab.security",
            Self::Sync => "account.tab.sync",
            Self::Notifications => "account.tab.notifications",
        }
    }
}

#[component]
pub(crate) fn Account() -> Element {
    let session = use_session();
    let i18n = use_i18n();
    let api = api::use_api();
    let mut panel = use_signal(|| Panel::Profile);

    if !session.is_authenticated() {
        return rsx! {
            h1 { class: "ik-page-title", {i18n.t("nav.account")} }
            SignInGate {}
        };
    }

    let name = session
        .username()
        .unwrap_or_else(|| i18n.t("common.readerFallback"));
    let role = i18n.t(session.role.read().label_key());
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
            h1 { class: "ik-page-title", {i18n.t("nav.account")} }
            button { class: "ik-btn", onclick: sign_out, {i18n.t("account.signOut")} }
        }
        div { class: "ik-tabs",
            for entry in Panel::ALL {
                button {
                    key: "{entry.label_key()}",
                    class: if current == entry { "ik-tab active" } else { "ik-tab" },
                    onclick: move |_| panel.set(entry),
                    {i18n.t(entry.label_key())}
                }
            }
        }
        match current {
            Panel::Profile => rsx! { profile::ProfilePanel { name: name.clone(), role: role.clone() } },
            Panel::Appearance => rsx! { appearance::AppearancePanel {} },
            Panel::Security => rsx! { security::SecurityPanel {} },
            Panel::Sync => rsx! { sync::SyncPanel {} },
            Panel::Notifications => rsx! { notifications::NotificationsPanel {} },
        }
    }
}

/// The shared card chrome every panel sits in: an icon + title header and a body.
///
/// `title` arrives already resolved — a panel has its [`crate::i18n::Translator`] to hand and
/// this keeps the chrome free of any opinion about where the words came from.
#[component]
fn PanelCard(icon: crate::icons::Icon, title: String, children: Element) -> Element {
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
