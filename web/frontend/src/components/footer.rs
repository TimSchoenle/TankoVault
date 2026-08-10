//! The page footer (layout handoff §5).
//!
//! Rendered by [`Shell`](super::Shell) after `.ik-content` and inside `.ik-main`, so it sits
//! under every route rather than being repeated per view, and shares the same `.ik-measure` row
//! as the top bar and the page body — the wordmark lines up with the page title and the link
//! groups with the page's right-hand actions.
//!
//! Deliberately light. Cookies & storage folds into the Data Policy; notification settings,
//! providers, shortcuts, release notes, documentation and "report an issue" are each one click
//! away from the rail or Account, and twenty footer links on every screen is noise.

use crate::components::nav::NOTICES_ROUTE;
use crate::i18n::{use_i18n, Translator};
use crate::icons::{Ic, Icon};
use crate::models::LegalKind;
use crate::state::legal::{legal_title, use_legal_index};
use crate::state::use_session;
use crate::{build_info, Route};
use dioxus::prelude::*;

/// The project's licence, and where its text lives.
const LICENCE: &str = "PolyForm Noncommercial 1.0.0";

/// The full five-column footer, or the one-line variant used under the auth card.
#[component]
pub(crate) fn Footer(#[props(default = false)] compact: bool) -> Element {
    let i18n = use_i18n();
    if compact {
        return rsx! {
            footer { class: "ik-footer compact",
                div { class: "ik-measure ik-footer-row",
                    span { class: "ik-muted", style: "font-size:12.5px;", {i18n.t("footer.tagline")} }
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
                        div { class: "ik-wordmark",
                            "Tankō"
                            span { class: "acc", "Vault" }
                        }
                        div { class: "ik-footer-desc", {i18n.t("footer.tagline")} }
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

/// Licences and where the code is. The notices document is a plain `<a>`, not a `Link`: the
/// target is server-rendered, and handing it to the client-side router resolves it to the app
/// shell — which answers `200` with a page that looks like it worked.
///
/// Absolute, not the bare path. The desktop build runs off a local webview origin, so a relative
/// href resolved against *that* — `file:///C:/third-party-notices`, a link into the reader's own
/// drive. Every other absolute link in this column already worked for exactly this reason.
#[component]
fn OpenSourceColumn() -> Element {
    let i18n = use_i18n();
    let origin = crate::platform::origin();
    let notices = format!("{}{NOTICES_ROUTE}", origin.trim_end_matches('/'));
    rsx! {
        nav { class: "ik-footer-col", "aria-label": i18n.t("footer.openSource"),
            div { class: "ik-footer-head", {i18n.t("footer.openSource")} }
            // Withheld rather than broken while the desktop build has no server yet: before the
            // first-run connect screen there is nothing to resolve the document against.
            if !origin.is_empty() {
                a {
                    class: "ik-footer-link",
                    href: "{notices}",
                    target: "_blank",
                    rel: "noopener noreferrer",
                    {i18n.t("nav.notices")}
                    Ic { icon: Icon::OpenInNew, size: 12 }
                }
            }
            span { class: "ik-footer-link", style: "cursor:default;", "{LICENCE}" }
            a {
                class: "ik-footer-link",
                href: build_info::PROJECT_URL,
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
                    href: build_info::RELEASES_URL,
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
#[component]
fn YourDataColumn() -> Element {
    let i18n = use_i18n();
    let session = use_session();
    if !session.is_authenticated() {
        return rsx! {};
    }
    rsx! {
        nav { class: "ik-footer-col", "aria-label": i18n.t("footer.yourData"),
            div { class: "ik-footer-head", {i18n.t("footer.yourData")} }
            Link { to: Route::Account {}, class: "ik-footer-link", {i18n.t("footer.export")} }
            Link { to: Route::Account {}, class: "ik-footer-link", {i18n.t("footer.dataRequests")} }
            Link { to: Route::Account {}, class: "ik-footer-link", {i18n.t("footer.deleteAccount")} }
        }
    }
}

/// `© 2026 Tim Schönle` left, `v0.9.4 · 7f3c1ab · api healthy` right.
///
/// The health pill is best-effort and **omitted** rather than shown red on failure: a footer
/// that reports the API as down on the very page the API just rendered is either wrong or
/// telling the reader something they can already see.
#[component]
fn MetaLine() -> Element {
    let i18n = use_i18n();
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
            span { {i18n.args("footer.copyright", &[("year", "2026")])} }
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
