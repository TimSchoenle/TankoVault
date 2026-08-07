//! Localisation of every reader-facing string (`i18nrs`, Dioxus binding).
//!
//! Catalogues live in `locales/<code>.json`, baked in via `include_str!` so a missing or
//! malformed one is a build failure, not a blank UI. This module adds placeholder substitution,
//! plural selection and `<html lang>` syncing on top of what `i18nrs` provides.

use dioxus::prelude::*;
use i18nrs::dioxus::{use_i18n as use_i18n_context, I18nProvider};
use i18nrs::{I18n, StorageType};
use std::collections::HashMap;

/// `localStorage` key the language is persisted under. Shares the `tv-` prefix with
/// [`crate::state::prefs`], and is read by `index.html`'s boot script before the WASM bundle
/// downloads.
const STORAGE_KEY: &str = "tv-lang";

/// The catalogue used when the browser asks for a language we do not ship, and the one every
/// message is authored in.
pub(crate) const DEFAULT_LANGUAGE: &str = "en";

/// A shipped language: its BCP-47 primary subtag, the name it calls itself, and its catalogue.
pub(crate) struct Locale {
    /// BCP-47 primary subtag (`"en"`), also the `localStorage` value and `<html lang>`.
    pub(crate) code: &'static str,
    /// The language's name *in that language*, for the picker — a reader looking for their own
    /// language recognises "Deutsch", not "German".
    pub(crate) endonym: &'static str,
    /// The raw catalogue JSON, parsed once by the provider.
    messages: &'static str,
}

/// Every language the app ships, in picker order.
pub(crate) const LOCALES: &[Locale] = &[
    Locale {
        code: "en",
        endonym: "English",
        messages: include_str!("../locales/en.json"),
    },
    Locale {
        code: "de",
        endonym: "Deutsch",
        messages: include_str!("../locales/de.json"),
    },
];

/// The catalogues in the shape `i18nrs` wants.
fn translations() -> HashMap<&'static str, &'static str> {
    LOCALES
        .iter()
        .map(|locale| (locale.code, locale.messages))
        .collect()
}

/// Provide the translation context to the whole app.
///
/// Mount this above the router: every screen consumes the context, and a language change has
/// to re-render all of them.
#[component]
pub(crate) fn I18nRoot(children: Element) -> Element {
    rsx! {
        I18nProvider {
            translations: translations(),
            storage_type: StorageType::LocalStorage,
            storage_name: STORAGE_KEY.to_string(),
            default_language: preferred_language(),
            children: rsx! {
                HtmlLang {}
                {children}
            },
        }
    }
}

/// The language to start in when nothing has been stored yet: the system's own preference if
/// we ship it, else [`DEFAULT_LANGUAGE`].
///
/// Only the primary subtag is matched — a reader whose system reports `de-AT` gets `de`
/// rather than being bounced to English over a regional suffix we do not distinguish.
fn preferred_language() -> String {
    crate::platform::preferred_language()
        .and_then(|tag| {
            let primary = tag
                .split(['-', '_'])
                .next()
                .unwrap_or_default()
                .to_ascii_lowercase();
            LOCALES.iter().find(|locale| locale.code == primary)
        })
        .map_or_else(
            || DEFAULT_LANGUAGE.to_owned(),
            |locale| locale.code.to_owned(),
        )
}

/// Mirror the active language onto the document root.
///
/// `i18nrs` maintains `dir` but not `lang`, and `lang` is what drives screen-reader voice
/// selection, hyphenation and locale-aware font fallback. Runs on mount and on every change.
#[component]
fn HtmlLang() -> Element {
    let i18n = use_i18n();
    use_effect(move || {
        // Written through the typed platform binding, so the tag is a value
        // rather than a fragment of a script — there is no string literal to break out of,
        // and nothing for the served CSP to have to permit.
        crate::platform::set_document_language(&i18n.language());
    });
    rsx! {}
}

/// A `Copy` translation handle: `let i18n = use_i18n();` then `i18n.t("nav.home")`.
///
/// `Copy` so it can be captured by event handlers and spawned futures — which is where API
/// failures are turned into sentences — without cloning at every call site.
#[derive(Clone, Copy)]
pub(crate) struct Translator {
    i18n: Signal<I18n>,
    set_language: EventHandler<String>,
}

impl Translator {
    /// The message at `key`, in the active language.
    ///
    /// Reading the signal subscribes the calling scope, so switching language re-renders
    /// everything that displayed a message.
    pub(crate) fn t(self, key: &str) -> String {
        self.i18n.read().t(key)
    }

    /// [`Translator::t`], with `{name}` placeholders replaced from `args`.
    pub(crate) fn args(self, key: &str, args: &[(&str, &str)]) -> String {
        interpolate(&self.t(key), args)
    }

    /// A count-sensitive message: `key.one` for exactly one, `key.other` for anything else,
    /// with `{count}` and `args` substituted into whichever form is picked.
    ///
    /// Deliberately a two-form rule, not full CLDR plural categories — every shipped language
    /// uses one/other, and CLDR's tables would cost more bundle bytes than the entire
    /// catalogue. The assertion below forces revisiting this before adding a language with a
    /// dual/few/many form.
    pub(crate) fn plural(self, key: &str, count: i64, args: &[(&str, &str)]) -> String {
        debug_assert!(
            LOCALES
                .iter()
                .all(|locale| matches!(locale.code, "en" | "de")),
            "a language outside the one/other plural split needs a real plural rule here",
        );
        let form = if count == 1 { "one" } else { "other" };
        let mut all = vec![("count", count.to_string())];
        all.extend(
            args.iter()
                .map(|(name, value)| (*name, (*value).to_owned())),
        );
        let borrowed: Vec<(&str, &str)> = all
            .iter()
            .map(|(name, value)| (*name, value.as_str()))
            .collect();
        self.args(&format!("{key}.{form}"), &borrowed)
    }

    /// The active language's [`Locale::code`].
    pub(crate) fn language(self) -> String {
        self.i18n.read().get_current_language().to_owned()
    }

    /// Switch language. The provider persists the choice and re-renders every consumer.
    pub(crate) fn set_language(self, code: &str) {
        self.set_language.call(code.to_owned());
    }
}

/// The translation handle for any component under [`I18nRoot`].
///
/// Not a hook — it is a context lookup — so it may be called after an early return, and more
/// than once in the same component, without disturbing hook order.
pub(crate) fn use_i18n() -> Translator {
    let context = use_i18n_context();
    Translator {
        i18n: context.i18n,
        set_language: context.set_language,
    }
}

/// Replace each `{name}` in `template` with its value from `args`.
///
/// An unknown placeholder is emitted verbatim rather than dropped: a visible `{count}` in the
/// UI points straight at the call site that forgot an argument, whereas a silent gap reads as
/// a plausible sentence and ships.
fn interpolate(template: &str, args: &[(&str, &str)]) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        let Some(close) = after.find('}') else {
            // An unterminated brace is literal text, not a broken placeholder.
            rest = &rest[open..];
            break;
        };
        let name = &after[..close];
        if let Some((_, value)) = args.iter().find(|(candidate, _)| *candidate == name) {
            out.push_str(value);
        } else {
            out.push('{');
            out.push_str(name);
            out.push('}');
        }
        rest = &after[close + 1..];
    }
    out.push_str(rest);
    out
}

/// Whether the default catalogue defines `key`, addressed as a dot path.
///
/// Test-only: at runtime a missing key already surfaces as `Key '…' not found`. This lets a
/// module that maps a closed set of values to catalogue keys assert every value it can produce
/// is actually worded — coverage `locales_define_the_same_keys` doesn't provide, since that only
/// proves the catalogues agree with each other.
#[cfg(test)]
pub(crate) fn has_key(key: &str) -> bool {
    let reference = LOCALES
        .iter()
        .find(|l| l.code == DEFAULT_LANGUAGE)
        .expect("default language is shipped");
    let catalogue: serde_json::Value =
        serde_json::from_str(reference.messages).expect("the default catalogue parses");

    let mut node = &catalogue;
    for segment in key.split('.') {
        match node.get(segment) {
            Some(next) => node = next,
            None => return false,
        }
    }
    node.is_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    #[test]
    fn has_key_finds_a_leaf_and_rejects_a_branch() {
        assert!(has_key("nav.watchlist"), "a real message must be found");
        assert!(
            !has_key("nav"),
            "an interior node is not a message; treating one as present would let a module \
             claim a whole section as its label and render `[object Object]`-shaped nonsense"
        );
        assert!(!has_key("nav.definitelyNotAKey"));
    }

    #[test]
    fn substitutes_named_placeholders() {
        assert_eq!(
            interpolate(
                "{count} new chapters for {title}",
                &[("count", "3"), ("title", "Blame!")]
            ),
            "3 new chapters for Blame!"
        );
    }

    #[test]
    fn leaves_unknown_placeholders_visible() {
        assert_eq!(interpolate("{count} left", &[]), "{count} left");
    }

    #[test]
    fn passes_literal_braces_through() {
        assert_eq!(interpolate("100% {", &[("a", "b")]), "100% {");
        assert_eq!(interpolate("no holes", &[("a", "b")]), "no holes");
    }

    #[test]
    fn every_locale_parses() {
        for locale in LOCALES {
            serde_json::from_str::<Value>(locale.messages)
                .unwrap_or_else(|e| panic!("{}.json is not valid JSON: {e}", locale.code));
        }
    }

    #[test]
    fn the_default_language_is_shipped() {
        assert!(LOCALES.iter().any(|l| l.code == DEFAULT_LANGUAGE));
    }

    /// `i18nrs` falls back to an arbitrary catalogue for a missing key, so a key that exists in
    /// only some locales renders unpredictably — including as the literal string
    /// `Key '…' not found`. Structural equality across catalogues rules that out.
    #[test]
    fn locales_define_the_same_keys() {
        let reference = LOCALES
            .iter()
            .find(|l| l.code == DEFAULT_LANGUAGE)
            .expect("default language is shipped");
        let expected = key_paths(&serde_json::from_str(reference.messages).unwrap());

        for locale in LOCALES.iter().filter(|l| l.code != reference.code) {
            let actual = key_paths(&serde_json::from_str(locale.messages).unwrap());
            let missing: Vec<_> = expected.difference(&actual).collect();
            let extra: Vec<_> = actual.difference(&expected).collect();
            assert!(
                missing.is_empty() && extra.is_empty(),
                "{}.json diverges from {}.json — missing: {missing:?}, unexpected: {extra:?}",
                locale.code,
                reference.code,
            );
        }
    }

    /// Every dot path that resolves to a leaf, so a key demoted to an object in one locale is
    /// reported as a difference rather than silently matching.
    fn key_paths(value: &Value) -> std::collections::BTreeSet<String> {
        fn walk(value: &Value, prefix: &str, out: &mut std::collections::BTreeSet<String>) {
            match value {
                Value::Object(fields) => {
                    for (name, child) in fields {
                        let path = if prefix.is_empty() {
                            name.clone()
                        } else {
                            format!("{prefix}.{name}")
                        };
                        walk(child, &path, out);
                    }
                }
                _ => {
                    out.insert(prefix.to_owned());
                }
            }
        }
        let mut out = std::collections::BTreeSet::new();
        walk(value, "", &mut out);
        out
    }
}
