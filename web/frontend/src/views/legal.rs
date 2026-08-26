//! `/legal/:slug` — one operator-published document, at the prose measure.
//!
//! The body is **operator input**, not developer input, so it is never turned into an HTML
//! string: `crate::markdown` maps it onto `rsx!` nodes and an operator's file has nothing to
//! inject into.

use crate::api;
use crate::components::{async_view, SkeletonBlock};
use crate::i18n::use_i18n;
use crate::icons::{Ic, Icon};
use crate::markdown::markdown;
use crate::models::LegalDocumentView;
use crate::state::legal::legal_title;
use crate::title::PageTitle;
use crate::Route;
use dioxus::prelude::*;
use progenitor_client::ResponseValue;

#[component]
pub(crate) fn Legal(slug: String) -> Element {
    let i18n = use_i18n();
    let api = api::use_api();
    let published = use_context::<PageTitle>();
    let language = i18n.language();

    let document = use_resource(use_reactive!(|(slug, language)| {
        let client = api.client();
        async move {
            client
                .legal_document()
                .slug(&slug)
                .lang(&language)
                .send()
                .await
                .map(ResponseValue::into_inner)
                .map_err(|e| api::friendly_error(i18n, e))
        }
    }));

    rsx! {
        {
            async_view(
                &document,
                use_reload_noop(),
                || rsx! { SkeletonBlock { height: 420 } },
                move |doc: &LegalDocumentView| {
                    // The document's own name is something the route cannot spell: it is the
                    // operator's title, in the locale the server chose.
                    published
                        .set(
                            Route::Legal { slug: doc.slug.clone() },
                            legal_title(i18n, &doc.slug, doc.title.as_deref()),
                        );
                    rsx! { Document { doc: doc.clone() } }
                },
            )
        }
    }
}

/// `async_view` takes a reload handle for its error state's retry. This page has no mutation to
/// invalidate, so it gets a fresh one rather than the shell's.
fn use_reload_noop() -> crate::hooks::Reload {
    crate::hooks::use_reload()
}

/// The rendered document: title, the last-updated and locale notes, then the prose.
#[component]
fn Document(doc: LegalDocumentView) -> Element {
    let i18n = use_i18n();
    let requested = i18n.language();
    rsx! {
        div { class: "ik-legal",
            div { class: "ik-flex", style: "gap:9px;margin-bottom:2px;",
                Ic { icon: Icon::Gavel, size: 18 }
                span { class: "ik-kicker", {i18n.t("footer.legal")} }
            }
            h1 { class: "ik-page-title", style: "margin-bottom:6px;",
                {legal_title(i18n, &doc.slug, doc.title.as_deref())}
            }
            div { class: "ik-legal-meta",
                if let Some(updated) = doc.updated.as_ref().filter(|u| !u.trim().is_empty()) {
                    span { {i18n.args("legal.updated", &[("date", updated)])} }
                }
                // Only when the two differ: saying "shown in English" on the English page is
                // noise, but a German reader looking at English text needs to know it is the
                // only version there is, not the operator's German.
                if doc.locale != requested {
                    span { class: "ik-legal-locale",
                        Ic { icon: Icon::Language, size: 13 }
                        {i18n.args("legal.localeNote", &[("locale", &doc.locale.to_uppercase())])}
                    }
                }
            }
            div { class: "ik-prose", {markdown(&doc.body)} }
        }
    }
}
