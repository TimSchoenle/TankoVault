//! The legal documents this deployment publishes, fetched once per session.
//!
//! Three surfaces read the same list — the footer's Legal column, the **More** sheet, and the
//! register form's "you accept …" line — so it is fetched once by [`use_legal_sync`] in the
//! shell and read from context everywhere else. Fetching per surface would issue three requests
//! on every page load for a list that only changes when an operator restarts the API.

use crate::api;
use crate::models::LegalIndexEntry;
use dioxus::prelude::*;

/// The published documents, empty until the index lands — and permanently empty when this
/// deployment publishes none, which is the common case.
#[derive(Clone, Copy)]
pub(crate) struct LegalIndex(pub(crate) Signal<Vec<LegalIndexEntry>>);

impl LegalIndex {
    pub(crate) fn new() -> Self {
        Self(Signal::new(Vec::new()))
    }
}

/// Fetch the index into context. Mount once, in the shell.
///
/// Best-effort and silent on failure, like Discover's facets: the whole point of driving the
/// footer from this list is that an unpublished document produces no link, so a failed fetch
/// producing no links is the same, correct, degradation.
pub(crate) fn use_legal_sync() {
    let api = api::use_api();
    let mut index = use_context::<LegalIndex>();
    let i18n = crate::i18n::use_i18n();
    // Keyed on the language: the operator's own titles are per-locale, so switching language
    // has to re-ask rather than keep the previous language's titles.
    let language = i18n.language();

    let _ = use_resource(use_reactive!(|language| {
        let client = api.client();
        async move {
            let entries = client
                .legal_index()
                .lang(language)
                .send()
                .await
                .map(|response| response.into_inner())
                .unwrap_or_default();
            index.0.set(entries);
        }
    }));
}

/// The published documents. A context lookup, not a hook, so it may be called after an early
/// return and more than once in the same component.
pub(crate) fn use_legal_index() -> Signal<Vec<LegalIndexEntry>> {
    use_context::<LegalIndex>().0
}

/// What to call a document: the operator's own title, else this build's name for a slug it
/// knows, else the slug — which is the honest answer for one it has never heard of, and the
/// reason an operator can publish `dmca` without a code change.
pub(crate) fn legal_title(
    i18n: crate::i18n::Translator,
    slug: &str,
    configured: Option<&str>,
) -> String {
    if let Some(title) = configured.map(str::trim).filter(|t| !t.is_empty()) {
        return title.to_owned();
    }
    match slug {
        "terms" => i18n.t("footer.terms"),
        "privacy" => i18n.t("footer.privacy"),
        "imprint" => i18n.t("footer.imprint"),
        other => other.to_owned(),
    }
}

/// The entry for `slug`, if this deployment publishes it.
///
/// Registration's acceptance sentence uses this: it links `terms` and `privacy` **only if
/// configured**, and omits the sentence entirely when they are not, rather than pointing
/// nowhere.
pub(crate) fn published(slug: &str) -> Option<LegalIndexEntry> {
    use_legal_index()
        .read()
        .iter()
        .find(|entry| entry.slug == slug)
        .cloned()
}
