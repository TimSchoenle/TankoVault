//! What this deployment calls itself, fetched once per session from `GET /v1/branding`.
//!
//! Four surfaces read it — the rail's lockup, the footer's lockup, licence and copyright line,
//! the document title, and every catalogue message carrying `{brand}` — so it is fetched once by
//! [`use_branding_sync`] in the shell and read from context everywhere else.
//!
//! It starts at the **last identity this client saw from this server**, and falls back to the
//! shipped one: the rail and the footer render before any response lands, and a wordmark that
//! flashes empty is worse than one that is briefly this project's — but one that flashes *this
//! project's* on a rebranded deployment is worse than either. The desktop build depends on it
//! more heavily still, because two of its surfaces run with no server to ask and no component
//! tree to read from: the notification raised as an update hands off to an installer, and the
//! one raised by the build that comes back. Both are [`remembered_name`]'s callers.
//!
//! The cache is keyed by the origin it came from, so a desktop client repointed at another
//! server shows the shipped identity for one fetch rather than the previous deployment's.

use crate::api;
use crate::models::BrandingView;
use dioxus::prelude::*;
use serde::{Deserialize, Serialize};

/// The cached identity, and the server it was read from.
const CACHE_KEY: &str = "tv-branding";

/// The deployment's identity, resolved.
///
/// A local struct rather than the wire type: the wire type's `copyright` is three fields that
/// every call site would have to recombine, and the compiled-in fallback needs a `Default` the
/// generated type does not have.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
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

/// The identity as it was last seen, and the server that published it.
#[derive(Serialize, Deserialize)]
struct Cached {
    origin: String,
    branding: Branding,
}

/// The last identity this client saw from the server it is currently pointed at.
///
/// The origin check is what makes the cache safe to seed the first frame from: without it a
/// desktop client moved to a second deployment would greet the reader under the first one's
/// name, which is worse than the shipped fallback because it is *plausible*.
pub(crate) fn remembered() -> Branding {
    let Some(text) = crate::platform::store_get(CACHE_KEY) else {
        return Branding::default();
    };
    serde_json::from_str::<Cached>(&text)
        .ok()
        .filter(|cached| cached.origin == crate::platform::origin())
        .map_or_else(Branding::default, |cached| cached.branding)
}

/// The product name as it was last seen, for the callers that have no component tree.
///
/// Those are the two update notifications: one is raised from `main` before the window exists,
/// the other on the first render of the build an installer produced, both far too early for
/// `/v1/branding` to have answered. Before this they resolved `{brand}` to nothing at all and to
/// the shipped name respectively, so a rebranded deployment's reader was told that this project
/// was updating — the one moment the app takes over their machine, announced over a name they
/// have never seen.
pub(crate) fn remembered_name() -> String {
    remembered().name
}

/// Keep `branding` for the next start.
fn remember(branding: &Branding) {
    let cached = Cached {
        origin: crate::platform::origin(),
        branding: branding.clone(),
    };
    if let Ok(text) = serde_json::to_string(&cached) {
        crate::platform::store_set(CACHE_KEY, &text);
    }
}

/// The deployment's identity, at the remembered one until the fetch lands.
#[derive(Clone, Copy)]
pub(crate) struct BrandingState(pub(crate) Signal<Branding>);

impl BrandingState {
    pub(crate) fn new() -> Self {
        Self(Signal::new(remembered()))
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
                let resolved = Branding::from(view.into_inner());
                remember(&resolved);
                branding.0.set(resolved);
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
    try_consume_context::<BrandingState>()
        .map_or_else(remembered_name, |state| state.0.read().name.clone())
}
