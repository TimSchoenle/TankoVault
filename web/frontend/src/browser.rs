//! The app's direct browser-API surface — `localStorage`, `<html>` attributes, full-page
//! navigation — typed through `web-sys` instead of `document::eval`.
//!
//! Dioxus's web `eval` runs as `new Function(code)`: a CSP without `'unsafe-eval'` blocks it,
//! and because the failure isn't caught, it aborts the WASM instance rather than returning an
//! error. This app hit exactly that on boot, reading appearance prefs: white page, dead instance.
//!
//! Storage can fail silently (private mode, blocked third-party storage); every caller's
//! fallback — a boot-script attribute or the stylesheet default — is a correct answer, not
//! a reason to interrupt the reader.

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

/// `window.localStorage`, if this browser exposes it to the document; a policy block and
/// "unset" both collapse to `None` here.
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

/// Mirror the active language onto `<html lang>`, set directly to avoid an `HtmlElement` downcast.
pub(crate) fn set_document_language(tag: &str) {
    set_root_attribute("lang", tag);
}

/// Leave the SPA for `url` — a real navigation, not a router push, for destinations outside
/// this origin (e.g. an OAuth consent screen).
pub(crate) fn navigate_to(url: &str) {
    if let Some(window) = web_sys::window() {
        let _ = window.location().set_href(url);
    }
}

/// Focus the element with the given id and select its contents; a no-op unless it's an
/// on-screen text field (not every screen renders the search box this serves).
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
