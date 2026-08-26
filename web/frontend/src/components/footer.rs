//! The page footer (layout handoff §5).
//!
//! Rendered by [`Shell`](super::Shell) after `.ik-content` and inside `.ik-main`, so it sits
//! under every route rather than being repeated per view, and shares the same `.ik-measure` row
//! as the top bar and the page body — the wordmark lines up with the page title and the link
//! groups with the page's right-hand actions.
//!
//! Deliberately light. Cookies & storage folds into the Data Policy; notification settings,
//! providers, release notes, documentation and "report an issue" are each one click away from
//! the rail or Account, and twenty footer links on every screen is noise. Keyboard shortcuts are
//! not here either: `?` on a screen that binds any opens the reference over it
//! ([`ShortcutsOverlay`](super::ShortcutsOverlay)).

use crate::components::Wordmark;
use crate::i18n::{use_i18n, Translator};
use crate::icons::{Ic, Icon};
use crate::models::LegalKind;
use crate::state::branding::use_branding;
use crate::state::legal::{legal_title, use_legal_index};
use crate::state::use_session;
use crate::views::AccountPanel;
use crate::{build_info, Route};
use dioxus::prelude::*;

/// The full five-column footer, or the one-line variant used under the auth card.
#[component]
pub(crate) fn Footer(#[props(default = false)] compact: bool) -> Element {
    let i18n = use_i18n();
    let branding = use_branding();
    if compact {
        return rsx! {
            footer { class: "ik-footer compact",
                div { class: "ik-measure ik-footer-row",
                    span { class: "ik-muted", style: "font-size:12.5px;", {tagline(i18n, &branding.read())} }
                    div { class: "ik-footer-groups", style: "gap:18px;",
                        {legal_links(i18n)}
                    }
                }
            }
        };
    }

    rsx! {
        footer { class: "ik-footer",
            div { class: "ik-measure",
                div { class: "ik-footer-row",
                    div { class: "ik-footer-brand",
                        Wordmark { class: "ik-wordmark" }
                        div { class: "ik-footer-desc", {tagline(i18n, &branding.read())} }
                    }
                    div { class: "ik-footer-groups",
                        LegalColumn {}
                        OpenSourceColumn {}
                        YourDataColumn {}
                    }
                }
                MetaLine {}
            }
        }
    }
}

/// The Legal column, **generated from `GET /v1/legal`**.
///
/// An operator who configures no Imprint gets no Imprint link rather than a dead one, and an
/// operator who adds `dmca` gets it for free. A failed call renders nothing at all — silent,
/// like Discover's facet degradation: a footer column that says "could not load" is worse than
/// a footer without the column.
#[component]
fn LegalColumn() -> Element {
    let i18n = use_i18n();
    let entries = use_legal_index();
    if entries.read().is_empty() {
        return rsx! {};
    }
    rsx! {
        nav { class: "ik-footer-col", "aria-label": i18n.t("footer.legal"),
            div { class: "ik-footer-head", {i18n.t("footer.legal")} }
            {legal_links(i18n)}
        }
    }
}

/// The copyright notice: the operator's verbatim one, else the catalogue's line filled in with
/// the holder and year the server resolved, else nothing at all.
fn copyright(i18n: Translator, branding: &crate::state::branding::Branding) -> String {
    if let Some(notice) = branding.copyright_notice.as_deref() {
        return notice.to_owned();
    }
    if branding.copyright_holder.is_empty() {
        return String::new();
    }
    i18n.args(
        "footer.copyright",
        &[
            ("year", &branding.copyright_year),
            ("holder", &branding.copyright_holder),
        ],
    )
}

/// The line under the wordmark: the operator's own, or the catalogue's translated one.
///
/// An operator who sets a tagline gets it in every language. That is the trade they asked for —
/// a deployment whose product is not the one the catalogue describes is better served by one
/// untranslated true sentence than by a translated false one.
fn tagline(i18n: Translator, branding: &crate::state::branding::Branding) -> String {
    branding
        .tagline
        .clone()
        .unwrap_or_else(|| i18n.t("footer.tagline"))
}

/// One link per configured document, in the shape the index described it.
///
/// Shared with the compact footer and the register form's acceptance line, so an externally
/// hosted document is a plain `<a>` in all three rather than a route that would 404.
pub(crate) fn legal_links(i18n: Translator) -> Element {
    let entries = use_legal_index();
    let entries = entries.read().clone();
    rsx! {
        for entry in entries {
            match entry.kind {
                LegalKind::External => {
                    let href = entry.url.clone().unwrap_or_default();
                    rsx! {
                        a {
                            key: "{entry.slug}",
                            class: "ik-footer-link",
                            href: "{href}",
                            target: "_blank",
                            rel: "noopener noreferrer",
                            {legal_title(i18n, &entry.slug, entry.title.as_deref())}
                            Ic { icon: Icon::OpenInNew, size: 12 }
                        }
                    }
                }
                LegalKind::Inline => rsx! {
                    Link {
                        key: "{entry.slug}",
                        to: Route::Legal { slug: entry.slug.clone() },
                        class: "ik-footer-link",
                        {legal_title(i18n, &entry.slug, entry.title.as_deref())}
                    }
                },
            }
        }
    }
}

/// Licences and where the code is.
///
/// The notices are an in-app `Link` now that `/licenses` renders them. It used to be a plain
/// `<a>` at the absolute `/third-party-notices`, for two reasons that both went away with the
/// screen: the target was server-rendered, so the client-side router would have resolved it to
/// the app shell; and the desktop build runs off a local webview origin, where a relative href
/// resolves against `file:///`. A route has neither problem, and the screen it opens links the
/// plain-text document itself — including on desktop before a server is chosen, where it now
/// says there is nothing to show rather than being withheld with no explanation.
#[component]
fn OpenSourceColumn() -> Element {
    let i18n = use_i18n();
    let branding = use_branding();
    let branding = branding.read();
    rsx! {
        nav { class: "ik-footer-col", "aria-label": i18n.t("footer.openSource"),
            div { class: "ik-footer-head", {i18n.t("footer.openSource")} }
            Link { to: Route::Licenses {}, class: "ik-footer-link", {i18n.t("nav.notices")} }
            // Plain text unless the operator publishes the licence somewhere: a self-hosted
            // deployment usually has nowhere to point, and a link into nothing is worse than a
            // label that does not pretend to be one.
            if let Some(url) = branding.licence_url.as_deref() {
                a {
                    class: "ik-footer-link",
                    href: "{url}",
                    target: "_blank",
                    rel: "noopener noreferrer",
                    "{branding.licence}"
                    Ic { icon: Icon::OpenInNew, size: 12 }
                }
            } else {
                span { class: "ik-footer-link", style: "cursor:default;", "{branding.licence}" }
            }
            a {
                class: "ik-footer-link",
                href: "{branding.project_url}",
                target: "_blank",
                rel: "noopener noreferrer",
                {i18n.t("footer.source")}
                Ic { icon: Icon::OpenInNew, size: 12 }
            }
            // Web only: an installed client advertising its own download is noise, and the copy
            // reading this one *is* the download.
            if cfg!(feature = "web") {
                a {
                    class: "ik-footer-link",
                    href: "{branding.releases_url}",
                    target: "_blank",
                    rel: "noopener noreferrer",
                    {i18n.t("footer.desktopApp")}
                    Ic { icon: Icon::OpenInNew, size: 12 }
                }
            }
        }
    }
}

/// The reader's own data. Every entry links the Account panel that owns it — the footer never
/// repeats a destructive action, so "Delete account" here is a route, not a button.
///
/// All three land on Privacy because that panel owns all three controls; before the panel was a
/// route segment they landed on Profile instead, which is three labels promising three
/// destinations and delivering a fourth.
#[component]
fn YourDataColumn() -> Element {
    let i18n = use_i18n();
    let session = use_session();
    if !session.is_authenticated() {
        return rsx! {};
    }
    let privacy = Route::AccountSection {
        panel: AccountPanel::Privacy,
    };
    rsx! {
        nav { class: "ik-footer-col", "aria-label": i18n.t("footer.yourData"),
            div { class: "ik-footer-head", {i18n.t("footer.yourData")} }
            Link { to: privacy.clone(), class: "ik-footer-link", {i18n.t("footer.export")} }
            Link { to: privacy.clone(), class: "ik-footer-link", {i18n.t("footer.dataRequests")} }
            Link { to: privacy, class: "ik-footer-link", {i18n.t("footer.deleteAccount")} }
        }
    }
}

/// The deployment's copyright notice left, `v0.9.4 · 7f3c1ab · api healthy` right.
///
/// The health pill is best-effort and **omitted** rather than shown red on failure: a footer
/// that reports the API as down on the very page the API just rendered is either wrong or
/// telling the reader something they can already see.
///
/// The notice is omitted the same way while it is unknown — until `/v1/branding` lands there is
/// no holder to name, and naming this project's under someone else's deployment would be a
/// false claim rather than a placeholder.
#[component]
fn MetaLine() -> Element {
    let i18n = use_i18n();
    let branding = use_branding();
    let api = crate::api::use_api();
    // One call per session, not per navigation: the resource is created here and `Shell` keeps
    // the footer mounted across routes.
    let health = use_resource(move || {
        let base = api.base_url();
        async move {
            reqwest::get(format!("{base}/health"))
                .await
                .is_ok_and(|response| response.status().is_success())
        }
    });
    let healthy = matches!(&*health.read(), Some(true));

    rsx! {
        div { class: "ik-footer-meta",
            span { {copyright(i18n, &branding.read())} }
            span { class: "right",
                span { "v{build_info::VERSION}" }
                if let Some(commit) = build_info::commit() {
                    span { "·" }
                    span { "{commit}" }
                }
                if healthy {
                    span { "·" }
                    span { class: "ok", {i18n.t("footer.apiHealthy")} }
                }
            }
        }
    }
}
