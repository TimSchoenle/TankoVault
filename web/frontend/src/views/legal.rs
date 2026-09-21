//! `/legal/:slug` — one operator-published document, at the prose measure.
//!
//! The body is **operator input**, not developer input, so it is never turned into an HTML
//! string: `terrace-legal-dioxus` renders it through `terrace-legal-markdown`, which maps it onto
//! elements and gives an operator's file nothing to inject into.

use crate::components::{ErrorBox, SkeletonBlock};
use crate::i18n::use_i18n;
use crate::icons::{Ic, Icon};
use crate::title::PageTitle;
use crate::Route;
use dioxus::prelude::*;
use terrace_legal_dioxus::LegalDocumentPage;

#[component]
pub(crate) fn Legal(slug: String) -> Element {
    let i18n = use_i18n();
    let published = use_context::<PageTitle>();
    // Bumped by the error state's retry. Keying the page on it remounts it, and a fresh mount
    // fetches again.
    let mut attempt = use_signal(|| 0_u32);
    let route = Route::Legal { slug: slug.clone() };

    rsx! {
        div { class: "ik-legal",
            div { class: "ik-flex", style: "gap:9px;margin-bottom:2px;",
                Ic { icon: Icon::Gavel, size: 18 }
                span { class: "ik-kicker", {i18n.t("footer.legal")} }
            }
            LegalDocumentPage {
                key: "{slug}-{attempt}",
                slug: slug.clone(),
                loading: rsx! { SkeletonBlock { height: 420 } },
                error: move |message: String| rsx! {
                    ErrorBox { message, on_retry: move |()| attempt += 1 }
                },
                // The document's own name is something the route cannot spell: it is the
                // operator's title, in the locale the server chose.
                on_title: move |title: String| published.set(route.clone(), title),
            }
        }
    }
}
