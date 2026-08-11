//! Localisation of every reader-facing string (`i18nrs`, Dioxus binding).
//!
//! Catalogues live in `locales/<code>.json`, baked in via `include_str!` so a missing or
//! malformed one is a build failure, not a blank UI. This module adds placeholder substitution,
//! plural selection and `<html lang>` syncing on top of what `i18nrs` provides.

use dioxus::prelude::*;
use i18nrs::dioxus::{use_i18n as use_i18n_context, I18nProvider};
use i18nrs::{I18n, StorageType};
use std::collections::HashMap;
use std::sync::OnceLock;

/// `localStorage` key the language is persisted under. Shares the `tv-` prefix with
/// [`crate::state::prefs`], and is read by `index.html`'s boot script before the WASM bundle
/// downloads.
const STORAGE_KEY: &str = "tv-lang";

/// The catalogue used when the browser asks for a language we do not ship, and the one every
/// message is authored in.
pub(crate) const DEFAULT_LANGUAGE: &str = "en";

/// The placeholder every message uses instead of spelling the product's name.
///
/// Checked for before interpolating, because [`Translator::t`] is on the render path of every
/// screen and resolving the name means reading a context and cloning a `String`.
const BRAND_PLACEHOLDER: &str = "{brand}";

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
    /// The message at `key`, in the active language, with `{brand}` resolved to what this
    /// deployment calls itself.
    ///
    /// Reading the signal subscribes the calling scope, so switching language re-renders
    /// everything that displayed a message — and reading the branding signal does the same, so a
    /// message naming the product corrects itself the moment `/v1/branding` lands.
    ///
    /// The catalogue never spells the product name out. Roughly forty messages name it — the
    /// desktop app's whole vocabulary does — and every one of them would otherwise be a literal
    /// no configuration could reach.
    pub(crate) fn t(self, key: &str) -> String {
        let message = self.i18n.read().t(key);
        if message.contains(BRAND_PLACEHOLDER) {
            return interpolate(
                &message,
                &[("brand", &crate::state::branding::brand_name())],
            );
        }
        message
    }

    /// [`Translator::t`], with `{name}` placeholders replaced from `args`.
    pub(crate) fn args(self, key: &str, args: &[(&str, &str)]) -> String {
        interpolate(&self.t(key), args)
    }

    /// [`Translator::t`], but `None` when the catalogue has no message at `key`.
    ///
    /// For the few panels that word a vocabulary owned by the *server* — audit actions, merge
    /// signals, decision reasons — where the honest fallback is the raw token. The binding
    /// renders a miss as the sentence `Key '…' not found for language '…'`, which is worse on
    /// screen than the token, so the miss has to be detected before the lookup rather than
    /// recognised in its result.
    pub(crate) fn t_opt(self, key: &str) -> Option<String> {
        has_key(key).then(|| self.t(key))
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

/// The message at `key`, with `{brand}` and the `args` placeholders substituted, resolved
/// **without** the Dioxus context.
///
/// For the one caller that has none: `update::install`'s hand-off runs from `main`, before the
/// app is launched, and it is the moment the reader most needs telling — the window they just
/// opened is about to vanish for a minute while an installer runs. A [`Translator`] is a context
/// lookup, so there is nothing to ask.
///
/// `{brand}` therefore comes from the branding this client last saw from this server
/// ([`crate::state::branding::remembered_name`]) rather than from a signal. Without it the two
/// messages here — the only ones in the app rendered outside a component tree — kept the
/// placeholder verbatim, so the notification that takes over a rebranded deployment's reader for
/// a minute read `{brand} is updating`.
///
/// It resolves the language the way [`I18nRoot`] would for a first run — the system's, if it is
/// shipped — rather than the one the reader last chose, which the desktop build does not persist.
/// Falls back through the default catalogue and finally to the key itself, so a missing message
/// is a visible key rather than an empty notification.
#[cfg(feature = "desktop")]
pub(crate) fn translate_offline(key: &str, args: &[(&str, &str)]) -> String {
    let preferred = preferred_language();
    let message = LOCALES
        .iter()
        .find(|locale| locale.code == preferred)
        .and_then(|locale| lookup(locale.messages, key))
        .or_else(|| {
            LOCALES
                .iter()
                .find(|locale| locale.code == DEFAULT_LANGUAGE)
                .and_then(|locale| lookup(locale.messages, key))
        })
        .unwrap_or_else(|| key.to_owned());
    if !message.contains(BRAND_PLACEHOLDER) {
        return interpolate(&message, args);
    }
    // The caller's arguments first: `interpolate` takes the first match, so a caller that means
    // something else by `brand` still wins.
    let brand = crate::state::branding::remembered_name();
    let mut all = args.to_vec();
    all.push(("brand", &brand));
    interpolate(&message, &all)
}

/// The string at a dot path in a catalogue, or `None` for a missing or non-leaf path.
#[cfg(feature = "desktop")]
fn lookup(messages: &str, key: &str) -> Option<String> {
    let catalogue: serde_json::Value = serde_json::from_str(messages).ok()?;
    let mut node = &catalogue;
    for segment in key.split('.') {
        node = node.get(segment)?;
    }
    node.as_str().map(str::to_owned)
}

/// The default catalogue, parsed once, for [`has_key`].
///
/// Only the default one: `locales_define_the_same_keys` holds every catalogue to the same key
/// set, so presence in the reference answers the question for all of them.
static REFERENCE_CATALOGUE: OnceLock<serde_json::Value> = OnceLock::new();

/// Whether the catalogue defines `key`, addressed as a dot path.
///
/// The membership test behind [`Translator::t_opt`], and what lets a module mapping a closed
/// set of values to catalogue keys assert in test that every value it can produce is worded —
/// coverage `locales_define_the_same_keys` does not provide, since that only proves the
/// catalogues agree with each other.
pub(crate) fn has_key(key: &str) -> bool {
    let catalogue = REFERENCE_CATALOGUE.get_or_init(|| {
        let reference = LOCALES
            .iter()
            .find(|l| l.code == DEFAULT_LANGUAGE)
            .expect("default language is shipped");
        serde_json::from_str(reference.messages).expect("the default catalogue parses")
    });

    let mut node = catalogue;
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

    /// The miss detection behind [`Translator::t_opt`].
    ///
    /// The bug this pins: the panels wording a server-owned vocabulary used to detect a miss by
    /// comparing the translation against the key, but the binding answers a miss with the
    /// sentence `Key '…' not found for language 'en'` and never with the key. The fallback to
    /// the raw token therefore never fired, and every audit row rendered that sentence in place
    /// of its action.
    #[test]
    fn a_key_the_catalogue_lacks_is_reported_missing() {
        assert!(has_key("console.audit.action.scan.trigger"));
        assert!(!has_key("console.audit.action.some.future.action"));
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

    /// A message resolved outside the component tree still names the deployment.
    ///
    /// The bug this pins: [`translate_offline`] interpolated only the caller's arguments, so
    /// `{brand}` survived into the text verbatim. Its two callers are the notifications either
    /// side of an unattended update — the one raised as the app hands itself to an installer, and
    /// the one the build that comes back raises — which is to say the only moment the app takes
    /// over the reader's machine announced itself as `{brand} is updating`. Neither has a
    /// [`Translator`] to ask, so nothing in the rest of the app could have caught it.
    ///
    /// Asserted as "no placeholder survives" rather than against a name, so it holds whatever
    /// the cached branding resolves to on the machine running the test.
    #[cfg(feature = "desktop")]
    #[test]
    fn a_message_resolved_without_the_context_still_names_the_product() {
        for key in [
            "settings.update.notify.applyingTitle",
            "settings.update.notify.applying",
        ] {
            let message = translate_offline(key, &[("version", "2.1.0")]);
            assert!(
                !message.contains(BRAND_PLACEHOLDER),
                "`{key}` reached the reader with the placeholder in it: {message}"
            );
            assert!(!message.is_empty(), "`{key}` resolved to nothing");
        }
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
