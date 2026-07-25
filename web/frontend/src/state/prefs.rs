//! Appearance preferences (`DESIGN_SPEC` §8).
//!
//! Each knob is a `data-*` attribute on `<html>` that swaps a block of CSS custom properties
//! (see the `[data-theme]` / `[data-accent]` / `[data-density]` / `[data-cover]` rules in
//! `input.css`), mirrored into a `tv-*` `localStorage` key so it survives a reload.
//!
//! The *initial* application deliberately does not happen here. It runs from an inline script
//! in `index.html`, before the first paint — a WASM app cannot set the attribute until the
//! bundle has downloaded, instantiated and rendered, which is long enough to flash the wrong
//! theme at the reader. This module only handles changes made while the app is running.

use dioxus::prelude::*;

/// One appearance knob: which attribute it drives, which key persists it, and the value that
/// means "leave it to the stylesheet".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Knob {
    /// The `data-*` attribute set on `<html>`.
    pub(crate) attr: &'static str,
    /// The `localStorage` key the choice is mirrored into.
    pub(crate) key: &'static str,
    /// The value already encoded by the `:root` defaults.
    pub(crate) default: &'static str,
    /// Whether the default must still be written explicitly.
    ///
    /// True for the theme only: both `dark` and `light` are real, meaningful values, and the
    /// boot script needs a stored `dark` to distinguish "the reader chose dark" from "no
    /// choice yet, follow the OS". Every other knob clears itself back to the stylesheet
    /// default instead of pinning it.
    pub(crate) explicit_default: bool,
}

pub(crate) const THEME: Knob = Knob {
    attr: "data-theme",
    key: "tv-theme",
    default: "dark",
    explicit_default: true,
};
pub(crate) const ACCENT: Knob = Knob {
    attr: "data-accent",
    key: "tv-accent",
    default: "vermilion",
    explicit_default: false,
};
pub(crate) const DENSITY: Knob = Knob {
    attr: "data-density",
    key: "tv-density",
    default: "standard",
    explicit_default: false,
};
pub(crate) const COVER: Knob = Knob {
    attr: "data-cover",
    key: "tv-cover",
    default: "ink",
    explicit_default: false,
};

impl Knob {
    /// Read the knob's persisted value into `signal` (`localStorage`, else the attribute the
    /// boot script already applied, else the default).
    pub(crate) fn load(self, mut signal: Signal<String>) {
        spawn(async move {
            let script = format!(
                "return localStorage.getItem('{key}') \
                 || document.documentElement.getAttribute('{attr}') \
                 || '{default}';",
                key = self.key,
                attr = self.attr,
                default = self.default,
            );
            if let Ok(value) = document::eval(&script).await {
                if let Some(value) = value.as_str() {
                    signal.set(value.trim_matches('"').to_owned());
                }
            }
        });
    }

    /// Apply and persist a choice. A non-default value sets the attribute and stores the key;
    /// selecting the default clears both, so the `:root` rules take over again (except for
    /// [`Knob::explicit_default`] knobs, which always write).
    pub(crate) fn apply(self, mut signal: Signal<String>, value: &str) {
        signal.set(value.to_owned());
        let script = if value == self.default && !self.explicit_default {
            format!(
                "document.documentElement.removeAttribute('{attr}');\
                 localStorage.removeItem('{key}');",
                attr = self.attr,
                key = self.key,
            )
        } else {
            // Serialise the value so a knob label can never break out of the string literal.
            let value = serde_json::to_string(value).unwrap_or_else(|_| "\"\"".to_owned());
            format!(
                "document.documentElement.setAttribute('{attr}',{value});\
                 localStorage.setItem('{key}',{value});",
                attr = self.attr,
                key = self.key,
            )
        };
        let _ = document::eval(&script);
    }
}
