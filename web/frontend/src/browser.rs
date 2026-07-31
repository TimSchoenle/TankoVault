//! The app's direct browser-API surface: `localStorage`, the root element's attributes, and
//! full-page navigation.
//!
//! ## Why this module exists
//!
//! Every function here replaces a `document::eval("…js…")` call. Dioxus implements `eval` on
//! the web target as `new Function(code)` (`dioxus-web`'s `WebEvaluator::create`), which a
//! Content-Security-Policy without `'unsafe-eval'` blocks outright — and because the
//! `wasm-bindgen` import is not marked `catch`, the thrown `EvalError` is not returned to Rust
//! but aborts the WASM instance. The app read its appearance preferences on boot, so the very
//! first eval killed it: a white page and `RuntimeError: unreachable executed`.
//!
//! The alternative was to widen the served policy with `'unsafe-eval'`
//! (`services/frontend/src/main.rs`), which would hand any injected script the one primitive
//! the policy exists to deny — for the sake of eight calls that are three lines of `web-sys`
//! each. Going through the typed bindings is also strictly better on its own terms: no string
//! interpolation to escape, no JSON round-trip, no promise per read, and the compiler checks
//! the call instead of the browser discovering the typo at runtime.
//!
//! ## Contract
//!
//! Nothing here fails loudly. A browser can refuse `localStorage` outright (private mode,
//! blocked third-party storage) and every caller's fallback — the attribute the boot script
//! already applied, or the stylesheet default — is a correct answer. A preference that cannot
//! be persisted is not a reason to interrupt the reader.

use wasm_bindgen::JsCast as _;

/// Read `key` from `localStorage`, or `None` if it is unset or storage is unavailable.
pub(crate) fn local_get(key: &str) -> Option<String> {
    storage()?.get_item(key).ok().flatten()
}

/// Persist `value` under `key`. A refusal is silent; see the module contract.
pub(crate) fn local_set(key: &str, value: &str) {
    if let Some(storage) = storage() {
        let _ = storage.set_item(key, value);
    }
}

/// Forget `key`, so whatever default the caller falls back to takes over again.
pub(crate) fn local_remove(key: &str) {
    if let Some(storage) = storage() {
        let _ = storage.remove_item(key);
    }
}

/// `window.localStorage`, if this browser exposes it to the document.
///
/// `local_storage()` returns `Err` rather than `Ok(None)` when storage is blocked by policy,
/// which is not a distinction any caller here acts on.
fn storage() -> Option<web_sys::Storage> {
    web_sys::window()?.local_storage().ok().flatten()
}

/// Read an attribute of `<html>` — the one the boot script in `index.html` applied before
/// first paint, which is the fallback when `localStorage` has no answer.
pub(crate) fn root_attribute(name: &str) -> Option<String> {
    root()?.get_attribute(name)
}

/// Set an attribute on `<html>`, which is what the `:root` rules in the stylesheet select on.
pub(crate) fn set_root_attribute(name: &str, value: &str) {
    if let Some(root) = root() {
        let _ = root.set_attribute(name, value);
    }
}

/// Remove an attribute from `<html>`, handing the choice back to the `:root` defaults.
pub(crate) fn remove_root_attribute(name: &str) {
    if let Some(root) = root() {
        let _ = root.remove_attribute(name);
    }
}

/// The `<html>` element.
fn root() -> Option<web_sys::Element> {
    web_sys::window()?.document()?.document_element()
}

/// Mirror the active language onto `<html lang>`.
///
/// Set as an attribute rather than through `HtmlElement::set_lang` so the call needs no
/// downcast: the two are the same reflected property, and `Element` is already in hand.
pub(crate) fn set_document_language(tag: &str) {
    set_root_attribute("lang", tag);
}

/// Leave the SPA for `url` — a real navigation, not a router push.
///
/// Used only where the destination is another origin (an OAuth consent screen), which the
/// client-side router cannot route to by definition.
pub(crate) fn navigate_to(url: &str) {
    if let Some(window) = web_sys::window() {
        let _ = window.location().set_href(url);
    }
}

/// Focus the element with the given id and select whatever it already contains.
///
/// A no-op unless the element is on screen and is a text field — the caller is a shortcut
/// button for the top bar's search box, and a screen that does not render one is not an error.
pub(crate) fn focus_and_select(id: &str) {
    let Some(field) = web_sys::window()
        .and_then(|window| window.document())
        .and_then(|document| document.get_element_by_id(id))
        .and_then(|element| element.dyn_into::<web_sys::HtmlInputElement>().ok())
    else {
        return;
    };
    let _ = field.focus();
    field.select();
}
