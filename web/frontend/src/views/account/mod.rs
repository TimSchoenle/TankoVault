//! Account & settings (`DESIGN_SPEC` §7.7) — a settings shell with a sub-nav and one panel per
//! concern. Each panel owns its own data and lives in its own module.

mod appearance;
mod callback;
mod notifications;
mod passkeys;
mod privacy;
mod profile;
mod security;
mod sync;
mod taste;

pub(crate) use callback::AnilistCallback;

use crate::api;
use crate::components::{AuthRequired, TabBar, TabKind};
use crate::i18n::use_i18n;
use crate::state::capabilities::{use_capabilities, CapabilitySet};
use crate::state::use_session;
use crate::wire::types::Feature;
use dioxus::prelude::*;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Panel {
    Profile,
    Appearance,
    Taste,
    Security,
    Sync,
    Notifications,
    Privacy,
}

impl TabKind for Panel {
    fn all() -> &'static [Self] {
        &[
            Self::Profile,
            Self::Appearance,
            Self::Taste,
            Self::Security,
            Self::Sync,
            Self::Notifications,
            Self::Privacy,
        ]
    }

    /// The catalogue key of this panel's tab label (see [`crate::i18n`]).
    fn label_key(self) -> &'static str {
        match self {
            Self::Profile => "account.tab.profile",
            Self::Appearance => "account.tab.appearance",
            Self::Taste => "account.tab.taste",
            Self::Security => "account.tab.security",
            Self::Sync => "account.tab.sync",
            Self::Notifications => "account.tab.notifications",
            Self::Privacy => "account.tab.privacy",
        }
    }
}

impl Panel {
    /// Whether this deployment offers the panel at all.
    fn is_visible(self, caps: &CapabilitySet) -> bool {
        match self {
            Self::Appearance => true,
            Self::Profile => caps.has_feature(Feature::AccountsProfile),
            Self::Taste => caps.has_feature(Feature::CatalogueRecommendations),
            // Either half is enough; passkeys and sessions are independent features.
            Self::Security => {
                caps.has_feature(Feature::AccountsSessions)
                    || caps.has_feature(Feature::AccountsPasskeys)
            }
            Self::Sync => caps.has_feature(Feature::SyncExternal),
            Self::Notifications => caps.has_feature(Feature::NotificationsPreferences),
            Self::Privacy => {
                caps.has_feature(Feature::PrivacySelfExport)
                    || caps.has_feature(Feature::PrivacySelfErasure)
                    || caps.has_feature(Feature::PrivacyRequests)
            }
        }
    }
}

#[component]
pub(crate) fn Account() -> Element {
    let session = use_session();
    let caps = use_capabilities();
    let i18n = use_i18n();
    let api = api::use_api();
    let mut panel = use_signal(|| Panel::Profile);

    if !session.is_authenticated() {
        return rsx! { AuthRequired { title: i18n.t("nav.account") } };
    }

    let name = session
        .username()
        .unwrap_or_else(|| i18n.t("common.readerFallback"));
    // Derived from capabilities, not a stored role — see `CapabilitySet::label_key`.
    let tier = i18n.t(caps.label_key());

    let visible: Vec<Panel> = Panel::all()
        .iter()
        .copied()
        .filter(|p| p.is_visible(&caps))
        .collect();
    // Appearance is always visible, so this can't be empty; avoid unwrapping so that stays
    // a local fact, not a future panic.
    let fallback = visible.first().copied().unwrap_or(Panel::Appearance);
    let current = {
        let selected = *panel.read();
        if visible.contains(&selected) {
            selected
        } else {
            fallback
        }
    };

    let sign_out = move |_| {
        let client = api.client();
        spawn(async move {
            // Clear locally regardless of the response: the refresh cookie may already be gone,
            // and leaving the reader "signed in" after opting out is worse than a quietly
            // failed revocation call.
            let _ = client.logout().send().await;
            session.clear();
            caps.clear();
        });
    };

    rsx! {
        div { class: "ik-page-head",
            h1 { class: "ik-page-title", {i18n.t("nav.account")} }
            button { class: "ik-btn", onclick: sign_out, {i18n.t("account.signOut")} }
        }
        TabBar {
            selected: current,
            on_select: move |next| panel.set(next),
            visible: visible.clone(),
        }
        match current {
            Panel::Profile => rsx! { profile::ProfilePanel { name: name.clone(), tier: tier.clone() } },
            Panel::Appearance => rsx! { appearance::AppearancePanel {} },
            Panel::Taste => rsx! { taste::TastePanel {} },
            Panel::Security => rsx! { security::SecurityPanel {} },
            Panel::Sync => rsx! { sync::SyncPanel {} },
            Panel::Notifications => rsx! { notifications::NotificationsPanel {} },
            Panel::Privacy => rsx! { privacy::PrivacyPanel {} },
        }
    }
}
