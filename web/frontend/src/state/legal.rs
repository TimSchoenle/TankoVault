//! The legal documents this deployment publishes, through `terrace-legal-dioxus`.
//!
//! [`LegalRoot`] mounts the library's provider once, in the shell, so the footer, the **More**
//! sheet, the register form and `/legal/:slug` share one index fetch per language. The rest of
//! this module is this app's side of the library's four traits: the generated client, the
//! message catalogue, the router and the `ik-*` classes.

use crate::api::{self, Api};
use crate::i18n::{use_i18n, Translator};
use crate::models;
use crate::Route;
use dioxus::prelude::*;
use progenitor_client::ResponseValue;
use terrace_legal_dioxus::{
    LegalProvider, LegalRouting, LegalSkin, LegalText, LegalTransport, LocalFuture, Part, Shared,
    SharedTransport,
};
use terrace_legal_model::{
    DocumentFormat, LegalDocumentView, LegalIndexEntry, LegalKind, LocaleTag,
};

/// Provide the legal index to everything below it. Mount once, in the shell.
///
/// Best-effort and silent on failure, like Discover's facets: an unpublished document produces no
/// link, so a failed fetch producing no links is the same, correct, degradation.
#[component]
pub(crate) fn LegalRoot(children: Element) -> Element {
    let i18n = use_i18n();
    let api = api::use_api();
    // Keyed on the language: the operator's titles are per locale, so switching language has to
    // re-ask rather than keep the previous language's titles.
    let language = use_memo(move || i18n.language());
    // Created once: the provider reads its handles on first render only.
    let handles = use_hook(move || Handles {
        transport: SharedTransport::new(Transport { api, i18n }),
        text: Shared::<dyn LegalText>::new(Words(i18n)),
        routing: Shared::<dyn LegalRouting>::new(Routes),
        skin: Shared::<dyn LegalSkin>::new(Skin),
    });
    rsx! {
        LegalProvider {
            transport: handles.transport,
            text: handles.text,
            routing: handles.routing,
            skin: handles.skin,
            language,
            {children}
        }
    }
}

#[derive(Clone)]
struct Handles {
    transport: SharedTransport,
    text: Shared<dyn LegalText>,
    routing: Shared<dyn LegalRouting>,
    skin: Shared<dyn LegalSkin>,
}

/// The published documents, in the operator's order; empty until the index lands, and when this
/// deployment publishes none. A context read, not a hook, so it may follow an early return.
pub(crate) fn documents() -> Vec<LegalIndexEntry> {
    terrace_legal_dioxus::use_legal_index().unwrap_or_default()
}

/// What to call `entry`: the operator's title, else [`known_title`], else the slug.
pub(crate) fn title(i18n: Translator, entry: &LegalIndexEntry) -> String {
    terrace_legal_dioxus::legal_title(&Words(i18n), entry)
}

/// Where an entry leads: the operator's own page for an external document, else ours.
pub(crate) fn external_url(entry: &LegalIndexEntry) -> Option<&str> {
    match entry.kind {
        LegalKind::External => entry.url.as_deref(),
        _ => None,
    }
}

/// What to call a document with no operator title: this build's name for a slug it knows, else
/// the slug — the honest answer for one it has never heard of, and the reason an operator can
/// publish `dmca` without a code change.
pub(crate) fn known_title(i18n: Translator, slug: &str) -> Option<String> {
    match slug {
        "terms" => Some(i18n.t("footer.terms")),
        "privacy" => Some(i18n.t("footer.privacy")),
        "imprint" => Some(i18n.t("footer.imprint")),
        _ => None,
    }
}

/// The two legal routes over the generated client, converted to the library's wire types.
struct Transport {
    api: Api,
    i18n: Translator,
}

impl LegalTransport for Transport {
    type Error = String;

    fn index(&self, lang: &str) -> LocalFuture<Result<Vec<LegalIndexEntry>, String>> {
        let client = self.api.client();
        let (i18n, lang) = (self.i18n, lang.to_owned());
        Box::pin(async move {
            let entries = client
                .legal_index()
                .lang(lang)
                .send()
                .await
                .map(ResponseValue::into_inner)
                .map_err(|e| api::friendly_error(i18n, e))?;
            Ok(entries.into_iter().map(index_entry).collect())
        })
    }

    fn document(&self, slug: &str, lang: &str) -> LocalFuture<Result<LegalDocumentView, String>> {
        let client = self.api.client();
        let (i18n, slug, lang) = (self.i18n, slug.to_owned(), lang.to_owned());
        Box::pin(async move {
            let view = client
                .legal_document()
                .slug(slug)
                .lang(lang)
                .send()
                .await
                .map(ResponseValue::into_inner)
                .map_err(|e| api::friendly_error(i18n, e))?;
            document_view(view).map_err(|e| e.to_string())
        })
    }
}

fn index_entry(wire: models::LegalIndexEntry) -> LegalIndexEntry {
    let kind = match wire.kind {
        models::LegalKind::Inline => LegalKind::Inline,
        models::LegalKind::External => LegalKind::External,
    };
    // A locale this build cannot parse is one it cannot request either, so it is dropped rather
    // than failing the whole index.
    let locales = wire
        .locales
        .iter()
        .filter_map(|code| code.parse::<LocaleTag>().ok())
        .collect();
    LegalIndexEntry::new(wire.slug, kind)
        .with_title(wire.title)
        .with_updated(wire.updated)
        .with_url(wire.url)
        .with_locales(locales)
}

fn document_view(
    wire: models::LegalDocumentView,
) -> Result<LegalDocumentView, terrace_legal_model::ParseLocaleError> {
    let locale = wire.locale.parse::<LocaleTag>()?;
    Ok(LegalDocumentView::new(wire.slug, locale, wire.body)
        .with_title(wire.title)
        .with_updated(wire.updated)
        .with_format(DocumentFormat::other(wire.format)))
}

/// The catalogue's words for the library's components.
struct Words(Translator);

impl LegalText for Words {
    fn known_title(&self, slug: &str) -> Option<String> {
        known_title(self.0, slug)
    }

    fn heading(&self) -> String {
        self.0.t("footer.legal")
    }

    fn updated(&self, date: &str) -> String {
        self.0.args("legal.updated", &[("date", date)])
    }

    fn shown_in(&self, locale: &LocaleTag) -> String {
        self.0.args(
            "legal.localeNote",
            &[("locale", &locale.as_str().to_uppercase())],
        )
    }
}

/// A hosted document's page is `/legal/:slug`.
struct Routes;

impl LegalRouting for Routes {
    fn document_link(&self, slug: &str, label: String, class: &'static str) -> Element {
        rsx! {
            Link { to: Route::Legal { slug: slug.to_owned() }, class, "{label}" }
        }
    }
}

/// The `ik-*` class for each part the library renders. The names live here, in a file the
/// Tailwind build scans, rather than in the library's source.
struct Skin;

impl LegalSkin for Skin {
    fn class(&self, part: Part) -> &'static str {
        match part {
            Part::FooterLink => "ik-footer-link",
            Part::SheetLink => "ik-sheet-row",
            Part::Link => "ik-link",
            Part::Page => "ik-legal-doc",
            Part::Meta => "ik-legal-meta",
            Part::LocaleNote => "ik-legal-meta ik-legal-locale",
            Part::Prose => "ik-prose",
            _ => "",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The generated client and the library each own a copy of the wire shape; the conversion is
    /// the one place they meet, so a field it forgets vanishes from every legal surface.
    #[test]
    fn a_wire_entry_converts_field_for_field() {
        let wire = models::LegalIndexEntry {
            slug: "terms".to_owned(),
            title: Some("Terms".to_owned()),
            updated: Some("2026-08-04".to_owned()),
            kind: models::LegalKind::Inline,
            url: None,
            locales: vec![
                "de".to_owned(),
                "de-AT".to_owned(),
                "not a locale".to_owned(),
            ],
        };
        let entry = index_entry(wire);
        assert_eq!(entry.slug, "terms");
        assert_eq!(entry.title.as_deref(), Some("Terms"));
        assert_eq!(entry.updated.as_deref(), Some("2026-08-04"));
        assert_eq!(entry.kind, LegalKind::Inline);
        let locales: Vec<&str> = entry.locales.iter().map(LocaleTag::as_str).collect();
        assert_eq!(locales, ["de", "de-AT"]);
    }

    #[test]
    fn a_wire_document_keeps_its_served_locale_and_format() {
        let wire = models::LegalDocumentView {
            slug: "terms".to_owned(),
            locale: "en".to_owned(),
            title: None,
            updated: None,
            format: "markdown".to_owned(),
            body: "# Terms".to_owned(),
        };
        let view = document_view(wire).expect("a valid locale");
        assert_eq!(view.locale.as_str(), "en");
        assert!(view.format.is_markdown());
        assert_eq!(view.body, "# Terms");
    }
}
