//! Appearance preferences (`DESIGN_SPEC` §8) and the operator console's persisted knobs.
//!
//! Each appearance knob is a `data-*` attribute on the document root, mirrored into a `tv-*`
//! settings key so it survives a restart. On the web build the *initial* application runs from
//! an inline script in `index.html`, before first paint, since a WASM app can't set the
//! attribute soon enough to avoid a flash of the wrong theme; the desktop build has no such
//! script and applies them from [`hydrate_appearance`] on the first render instead.
//!
//! The console's knobs (below) are `localStorage` and *not* the URL on purpose: they are the
//! operator's, not the link's. A colleague opening a pasted console URL should see their own
//! density and their own pinned tiles, not the sender's.

use crate::views::ConsoleEntity;
use dioxus::prelude::*;
use std::collections::BTreeSet;
use std::str::FromStr as _;

/// Every appearance knob, in the order they are applied. Iterated by [`hydrate_appearance`];
/// the individual constants are what the appearance screen binds its controls to.
const KNOBS: [Knob; 4] = [THEME, ACCENT, DENSITY, COVER];

/// Apply each stored appearance choice to the document root.
///
/// Runs once at the app root. On web this re-asserts what the boot script already wrote, which
/// is why it is cheap and not a source of flash; on desktop it is the only thing that applies
/// them at all.
pub(crate) fn hydrate_appearance() {
    for knob in KNOBS {
        let stored = crate::platform::store_get(knob.key)
            .or_else(|| crate::platform::root_attribute(knob.attr));
        if let Some(value) = stored {
            crate::platform::set_root_attribute(knob.attr, &value);
        }
    }
}

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
        let stored = crate::platform::store_get(self.key)
            .or_else(|| crate::platform::root_attribute(self.attr))
            .unwrap_or_else(|| self.default.to_owned());
        signal.set(stored);
    }

    /// Apply and persist a choice. A non-default value sets the attribute and stores the key;
    /// selecting the default clears both, so the `:root` rules take over again (except for
    /// [`Knob::explicit_default`] knobs, which always write).
    pub(crate) fn apply(self, mut signal: Signal<String>, value: &str) {
        signal.set(value.to_owned());
        if value == self.default && !self.explicit_default {
            crate::platform::remove_root_attribute(self.attr);
            crate::platform::store_remove(self.key);
        } else {
            crate::platform::set_root_attribute(self.attr, value);
            crate::platform::store_set(self.key, value);
        }
    }
}

/// The entity a bare `/console` reopens.
const CONSOLE_ENTITY: &str = "tv-console-entity";
/// Whether the console's live push is running or detached.
const CONSOLE_LIVE: &str = "tv-console-live";
/// Whether the console's tables are drawn compact.
const CONSOLE_COMPACT: &str = "tv-console-compact";
/// The columns the operator has hidden, as a comma-separated list of column tokens.
const CONSOLE_HIDDEN_COLUMNS: &str = "tv-console-hidden-cols";

/// The console entity to reopen, if one was stored and this build still has it.
pub(crate) fn console_entity() -> Option<ConsoleEntity> {
    crate::platform::store_get(CONSOLE_ENTITY).and_then(|slug| ConsoleEntity::from_str(&slug).ok())
}

/// Remember the console entity as the one to reopen.
pub(crate) fn set_console_entity(entity: ConsoleEntity) {
    crate::platform::store_set(CONSOLE_ENTITY, entity.slug());
}

/// Whether the console's live push should run. Defaults to on — a console that opens detached
/// shows stale numbers with no sign that they are stale.
pub(crate) fn console_live() -> bool {
    crate::platform::store_get(CONSOLE_LIVE).is_none_or(|stored| stored != "0")
}

pub(crate) fn set_console_live(live: bool) {
    if live {
        crate::platform::store_remove(CONSOLE_LIVE);
    } else {
        crate::platform::store_set(CONSOLE_LIVE, "0");
    }
}

/// Whether the console's tables are drawn compact. Defaults to on, matching what the class
/// strings hardcoded before this was a choice.
pub(crate) fn console_compact() -> bool {
    crate::platform::store_get(CONSOLE_COMPACT).is_none_or(|stored| stored != "0")
}

pub(crate) fn set_console_compact(compact: bool) {
    if compact {
        crate::platform::store_remove(CONSOLE_COMPACT);
    } else {
        crate::platform::store_set(CONSOLE_COMPACT, "0");
    }
}

/// The console table columns the operator has hidden.
pub(crate) fn console_hidden_columns() -> BTreeSet<String> {
    crate::platform::store_get(CONSOLE_HIDDEN_COLUMNS)
        .unwrap_or_default()
        .split(',')
        .filter(|token| !token.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

pub(crate) fn set_console_hidden_columns(hidden: &BTreeSet<String>) {
    if hidden.is_empty() {
        crate::platform::store_remove(CONSOLE_HIDDEN_COLUMNS);
    } else {
        let joined = hidden.iter().cloned().collect::<Vec<_>>().join(",");
        crate::platform::store_set(CONSOLE_HIDDEN_COLUMNS, &joined);
    }
}
