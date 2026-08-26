//! Account & settings (`DESIGN_SPEC` §7.7) — a settings shell with a sub-nav and one panel per
//! concern. Each panel owns its own data and lives in its own module.
//!
//! The open panel is a route segment, not a signal: `/account/security` is an address, so a
//! reload, the back button and a footer link all land where they say they do.

mod appearance;
mod callback;
mod content;
mod desktop;
mod mfa;
mod notifications;
mod passkeys;
mod privacy;
mod profile;
mod security;
mod sources;
mod sync;
mod taste;

pub(crate) use callback::AnilistCallback;

use crate::api;
use crate::app::Route;
use crate::components::{AuthRequired, SkeletonBlock, TabBar, TabKind};
use crate::i18n::use_i18n;
use crate::state::capabilities::{use_capabilities, CapabilitySet};
use crate::state::use_session;
use crate::wire::types::Feature;
use dioxus::prelude::*;
use inkstone_ui::Button;
use std::fmt;
use std::str::FromStr;

/// One settings panel.
///
/// Public because it is a route segment: [`AccountPanel::slug`] *is* the URL, so
/// `/account/privacy` is an address rather than a parameter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AccountPanel {
    Profile,
    Appearance,
    Sources,
    Content,
    Taste,
    Security,
    Sync,
    Notifications,
    DesktopApp,
    Privacy,
}

impl TabKind for AccountPanel {
    fn all() -> &'static [Self] {
        &[
            Self::Profile,
            Self::Appearance,
            Self::Sources,
            Self::Content,
            Self::Taste,
            Self::Security,
            Self::Sync,
            Self::Notifications,
            Self::DesktopApp,
            Self::Privacy,
        ]
    }

    /// The catalogue key of this panel's tab label (see [`crate::i18n`]).
    fn label_key(self) -> &'static str {
        match self {
            Self::Profile => "account.tab.profile",
            Self::Appearance => "account.tab.appearance",
            Self::Sources => "account.tab.sources",
            Self::Content => "account.tab.content",
            Self::Taste => "account.tab.taste",
            Self::Security => "account.tab.security",
            Self::Sync => "account.tab.sync",
            Self::Notifications => "account.tab.notifications",
            Self::DesktopApp => "account.tab.desktop",
            Self::Privacy => "account.tab.privacy",
        }
    }
}

impl AccountPanel {
    /// This panel's URL segment.
    pub(crate) fn slug(self) -> &'static str {
        match self {
            Self::Profile => "profile",
            Self::Appearance => "appearance",
            Self::Sources => "sources",
            Self::Content => "content",
            Self::Taste => "taste",
            Self::Security => "security",
            Self::Sync => "sync",
            Self::Notifications => "notifications",
            Self::DesktopApp => "desktop-app",
            Self::Privacy => "privacy",
        }
    }

    /// Whether this deployment offers the panel at all.
    fn is_visible(self, caps: &CapabilitySet) -> bool {
        match self {
            // Neither has a flag behind it: appearance is device-local, and the source order is
            // ungated on the server because it only shapes outbound links — a reader who cannot
            // change it is worse off than one who can.
            Self::Appearance | Self::Sources => true,
            // The one panel gated on the *build* rather than on the deployment: it advertises
            // the native client, and the reader of the native client already has it.
            Self::DesktopApp => cfg!(feature = "web"),
            // Deliberately *not* gated on `CatalogueAdultContent`. The panel explains that the
            // deployment has it switched off; hiding it instead would mean an operator turning
            // the flag on silently activates opt-ins nobody has been able to review.
            Self::Content => caps.has_feature(Feature::CatalogueBrowse),
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

impl fmt::Display for AccountPanel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.slug())
    }
}

/// An unrecognised panel slug. The route table turns this into a redirect to `/account` rather
/// than a 404: a link to a panel this build has dropped should still land on the settings.
#[derive(Debug)]
pub(crate) struct UnknownPanel;

impl fmt::Display for UnknownPanel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("no account panel by that name")
    }
}

impl FromStr for AccountPanel {
    type Err = UnknownPanel;

    fn from_str(slug: &str) -> Result<Self, Self::Err> {
        Self::all()
            .iter()
            .copied()
            .find(|panel| panel.slug() == slug)
            .ok_or(UnknownPanel)
    }
}

/// The panel a bare `/account` opens: the first one this deployment offers, which is Profile
/// wherever profiles are switched on.
fn landing_panel(caps: &CapabilitySet) -> AccountPanel {
    AccountPanel::all()
        .iter()
        .copied()
        .find(|panel| panel.is_visible(caps))
        // Appearance is always visible, so this cannot be reached; avoid unwrapping so that
        // stays a local fact, not a future panic.
        .unwrap_or(AccountPanel::Appearance)
}

/// `/account` — the way in, not a place. Resolves the panel this reader can actually open and
/// rewrites the address bar to it, so no settings panel is unlinkable.
#[component]
pub(crate) fn Account() -> Element {
    let session = use_session();
    let caps = use_capabilities();
    let i18n = use_i18n();
    let navigator = navigator();

    use_effect(move || {
        if caps.is_ready() {
            navigator.replace(Route::AccountSection {
                panel: landing_panel(&caps),
            });
        }
    });

    if !session.is_authenticated() {
        return rsx! { AuthRequired { title: i18n.t("nav.account") } };
    }

    // Held back until the capability fetch lands: which panels exist depends on it, and opening
    // Profile only to swap it out a moment later reads as a glitch.
    rsx! {
        h1 { class: "ik-page-title", {i18n.t("nav.account")} }
        SkeletonBlock { height: 220 }
    }
}

/// `/account/:panel` — the settings shell itself.
#[component]
pub(crate) fn AccountSection(panel: AccountPanel) -> Element {
    let session = use_session();
    let caps = use_capabilities();
    let i18n = use_i18n();
    let api = api::use_api();
    let navigator = navigator();

    // The route is the read side of the panel choice; a memo rather than a signal, because a
    // signal here would be a second copy of it that the back button cannot reach.
    let routed = use_memo(use_reactive!(|panel| panel));

    // The panel named in the URL can lose its feature under the reader's feet. Rewrite the
    // address rather than leave it naming a panel that renders something else.
    use_effect(move || {
        let open = *routed.read();
        if caps.is_ready() && !open.is_visible(&caps) {
            navigator.replace(Route::AccountSection {
                panel: landing_panel(&caps),
            });
        }
    });

    if !session.is_authenticated() {
        return rsx! { AuthRequired { title: i18n.t("nav.account") } };
    }

    let name = session
        .username()
        .unwrap_or_else(|| i18n.t("common.readerFallback"));
    // Derived from capabilities, not a stored role — see `CapabilitySet::label_key`.
    let tier = i18n.t(caps.label_key());

    let visible: Vec<AccountPanel> = AccountPanel::all()
        .iter()
        .copied()
        .filter(|p| p.is_visible(&caps))
        .collect();
    // The effect above is already rewriting the address; rendering the routed panel meanwhile
    // would mount a screen this deployment does not offer.
    let current = if visible.contains(&panel) {
        panel
    } else {
        landing_panel(&caps)
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
            Button {
                on_click: sign_out,
                {i18n.t("account.signOut")}
            }
        }
        // A sub-page nav, not a chip cloud. Ten panels wrapped into two or three ragged rows at
        // every width below a wide desktop, and the second row's left edge under the first read
        // as a list that had spilled rather than as the screen's own navigation. `scroll` keeps
        // it to one row on the page's own column, between the head above it and the panel below.
        //
        // No `<nav>` around it: the kit puts the label on the tablist, and a landmark whose only
        // child is a named widget is announced twice.
        TabBar {
            selected: current,
            // Pushes, so the back button walks the panels the reader actually opened.
            on_select: move |next: AccountPanel| {
                navigator.push(Route::AccountSection { panel: next });
            },
            visible: visible.clone(),
            label: i18n.t("nav.account"),
            scroll: true,
        }
        match current {
            AccountPanel::Profile => rsx! { profile::ProfilePanel { name: name.clone(), tier: tier.clone() } },
            AccountPanel::Appearance => rsx! { appearance::AppearancePanel {} },
            AccountPanel::Sources => rsx! { sources::SourcesPanel {} },
            AccountPanel::Content => rsx! { content::ContentPanel {} },
            AccountPanel::Taste => rsx! { taste::TastePanel {} },
            AccountPanel::Security => rsx! { security::SecurityPanel {} },
            AccountPanel::Sync => rsx! { sync::SyncPanel {} },
            AccountPanel::Notifications => rsx! { notifications::NotificationsPanel {} },
            AccountPanel::DesktopApp => rsx! { desktop::DesktopAppPanel {} },
            AccountPanel::Privacy => rsx! { privacy::PrivacyPanel {} },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every panel's slug is its URL, so a slug that does not parse back to itself is a panel no
    /// link can reach — which is the whole defect routing the panel exists to close. Ten panels
    /// shared one address (`/account`) until they became segments.
    #[test]
    fn every_panel_slug_parses_back_to_itself() {
        for panel in AccountPanel::all().iter().copied() {
            let slug = panel.slug();
            assert_eq!(
                slug.parse::<AccountPanel>().ok(),
                Some(panel),
                "`{slug}` does not round-trip through the route segment"
            );
            assert_eq!(panel.to_string(), slug);
        }
    }

    /// Two panels sharing a slug would make one of them unreachable, and the shadowed one would
    /// simply never open — silently, because the parse still succeeds.
    #[test]
    fn panel_slugs_are_unique() {
        let mut slugs: Vec<&str> = AccountPanel::all().iter().map(|p| p.slug()).collect();
        slugs.sort_unstable();
        let count = slugs.len();
        slugs.dedup();
        assert_eq!(slugs.len(), count);
    }

    /// An unknown segment must fail to parse, so the route table's `/account/:_panel` redirect
    /// takes over. Parsing it as a default panel instead would make every typo silently open
    /// Profile at an address that does not name it.
    #[test]
    fn an_unknown_slug_does_not_parse() {
        assert!("nonsense".parse::<AccountPanel>().is_err());
        assert!(String::new().parse::<AccountPanel>().is_err());
        // The AniList callback shares the `/account/` prefix and must never resolve as a panel.
        assert!("anilist-callback".parse::<AccountPanel>().is_err());
    }

    /// `landing_panel` walks the strip in order, so the default segment is whatever comes first
    /// — Profile, as the sub-nav shows it.
    #[test]
    fn profile_is_the_first_panel_in_the_strip() {
        assert_eq!(AccountPanel::all().first(), Some(&AccountPanel::Profile));
    }
}
