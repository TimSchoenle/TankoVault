//! The brand lockup, drawn from configuration rather than spelled out.
//!
//! One component for the two places it appears — the rail's lockup and the footer's — because
//! the two-tone split is a property of the configured identity, and a second copy would be the
//! one that keeps this project's accent half after a rebrand.

use crate::state::branding::use_branding;
use dioxus::prelude::*;

/// The wordmark: the lead half in the body colour, the accent half in the accent colour.
///
/// An identity with no accent half renders as one word. That is the correct rendering for an
/// operator's own name, not a degraded one — there is no rule that would split an arbitrary name
/// the way "Tankō/Vault" is split, and guessing produces a lockup nobody chose.
#[component]
pub(crate) fn Wordmark(class: String) -> Element {
    let branding = use_branding();
    let branding = branding.read();
    rsx! {
        div { class: "{class}",
            "{branding.wordmark_lead}"
            if let Some(accent) = branding.wordmark_accent.as_deref() {
                span { class: "acc", "{accent}" }
            }
        }
    }
}
