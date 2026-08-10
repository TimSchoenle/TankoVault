//! What this deployment calls itself, fetched once per session from `GET /v1/branding`.
//!
//! Four surfaces read it — the rail's lockup, the footer's lockup, licence and copyright line,
//! the document title, and every catalogue message carrying `{brand}` — so it is fetched once by
//! [`use_branding_sync`] in the shell and read from context everywhere else.
//!
//! It starts at the **shipped** identity rather than blank, and that is load-bearing: the rail
//! and the footer render before any response lands, and a wordmark that flashes empty is worse
//! than one that is briefly this project's. The desktop build depends on the same thing more
//! heavily — before its first-run connect screen there is no server to ask at all.

use crate::api;
use crate::models::BrandingView;
use dioxus::prelude::*;

/// The deployment's identity, resolved.
///
/// A local struct rather than the wire type: the wire type's `copyright` is three fields that
/// every call site would have to recombine, and the compiled-in fallback needs a `Default` the
/// generated type does not have.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct Branding {
    /// The product name in prose. Substituted for `{brand}` in every catalogue message.
    pub(crate) name: String,
    /// The lockup's body-coloured half.
    pub(crate) wordmark_lead: String,
    /// The lockup's accent half, when the identity has one.
    pub(crate) wordmark_accent: Option<String>,
    /// An operator's own tagline, which replaces the translated one.
    pub(crate) tagline: Option<String>,
    /// Who holds the copyright, and for which year — kept apart rather than pre-composed, so the
    /// catalogue decides the order and the symbol.
    pub(crate) copyright_holder: String,
    /// See [`Self::copyright_holder`].
    pub(crate) copyright_year: String,
    /// The whole notice verbatim, when the operator supplied one. Renders instead of the
    /// catalogue's line.
    pub(crate) copyright_notice: Option<String>,
    /// The licence label.
    pub(crate) licence: String,
    /// Where the licence label links, when the operator publishes the text.
    pub(crate) licence_url: Option<String>,
    /// The project's own page.
    pub(crate) project_url: String,
    /// Where a reader downloads the native client.
    pub(crate) releases_url: String,
}

impl Default for Branding {
    fn default() -> Self {
        Self {
            name: crate::build_info::PRODUCT_NAME.to_owned(),
            wordmark_lead: crate::build_info::WORDMARK_LEAD.to_owned(),
            wordmark_accent: Some(crate::build_info::WORDMARK_ACCENT.to_owned()),
            tagline: None,
            // Left empty rather than guessed: the holder and the year are the server's to
            // resolve — the year is the *current* one, which nothing here knows — and a footer
            // briefly claiming the wrong holder is worse than one whose meta line is briefly a
            // line shorter. `MetaLine` omits the notice while the holder is empty.
            copyright_holder: String::new(),
            copyright_year: String::new(),
            copyright_notice: None,
            licence: crate::build_info::LICENCE.to_owned(),
            licence_url: None,
            project_url: crate::build_info::PROJECT_URL.to_owned(),
            releases_url: crate::build_info::RELEASES_URL.to_owned(),
        }
    }
}

impl From<BrandingView> for Branding {
    fn from(view: BrandingView) -> Self {
        Self {
            name: view.name,
            wordmark_lead: view.wordmark.lead,
            wordmark_accent: view.wordmark.accent,
            tagline: view.tagline,
            copyright_holder: view.copyright.holder,
            copyright_year: view.copyright.year,
            copyright_notice: view.copyright.notice,
            licence: view.licence.name,
            licence_url: view.licence.url,
            project_url: view.project_url,
            releases_url: view.releases_url,
        }
    }
}

/// The deployment's identity, at the shipped default until the fetch lands.
#[derive(Clone, Copy)]
pub(crate) struct BrandingState(pub(crate) Signal<Branding>);

impl BrandingState {
    pub(crate) fn new() -> Self {
        Self(Signal::new(Branding::default()))
    }
}

/// Fetch the branding into context. Mount once, in the shell.
///
/// Best-effort and silent on failure, like the legal index: a deployment that cannot answer this
/// keeps the shipped identity, which is exactly what an unconfigured `[branding]` section would
/// have published anyway.
pub(crate) fn use_branding_sync() {
    let api = api::use_api();
    let mut branding = use_context::<BrandingState>();

    // Bound and unread, for the same reason `use_legal_sync`'s is: it writes into context
    // rather than being read back, and `let _ =` would drop the future.
    let _fetch = use_resource(move || {
        let client = api.client();
        async move {
            if let Ok(view) = client.branding().send().await {
                branding.0.set(Branding::from(view.into_inner()));
            }
        }
    });
}

/// The deployment's identity. A context lookup, not a hook, so it may be called after an early
/// return and more than once in the same component.
pub(crate) fn use_branding() -> Signal<Branding> {
    use_context::<BrandingState>().0
}

/// The product name, for callers that cannot assume the context exists.
///
/// [`crate::i18n::Translator`] is the one that cannot: it is constructed from a context lookup in
/// every component, including any mounted above the provider, and a message rendering as
/// `{brand}` because a lookup panicked would be the worst of both.
pub(crate) fn brand_name() -> String {
    try_consume_context::<BrandingState>().map_or_else(
        || crate::build_info::PRODUCT_NAME.to_owned(),
        |state| state.0.read().name.clone(),
    )
}
