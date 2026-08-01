//! Appearance preferences (`DESIGN_SPEC` §8).
//!
//! Each knob is a `data-*` attribute on `<html>`, mirrored into a `tv-*` `localStorage` key so
//! it survives a reload. The *initial* application runs from an inline script in `index.html`,
//! before first paint, since a WASM app can't set the attribute soon enough to avoid a flash of
//! the wrong theme — this module only handles changes made while the app is running.

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
        let stored = crate::browser::local_get(self.key)
            .or_else(|| crate::browser::root_attribute(self.attr))
            .unwrap_or_else(|| self.default.to_owned());
        signal.set(stored);
    }

    /// Apply and persist a choice. A non-default value sets the attribute and stores the key;
    /// selecting the default clears both, so the `:root` rules take over again (except for
    /// [`Knob::explicit_default`] knobs, which always write).
    pub(crate) fn apply(self, mut signal: Signal<String>, value: &str) {
        signal.set(value.to_owned());
        if value == self.default && !self.explicit_default {
            crate::browser::remove_root_attribute(self.attr);
            crate::browser::local_remove(self.key);
        } else {
            crate::browser::set_root_attribute(self.attr, value);
            crate::browser::local_set(self.key, value);
        }
    }
}
