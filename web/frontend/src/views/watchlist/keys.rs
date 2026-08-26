//! The list's keyboard contract (§3.5) — the one list the keydown handler dispatches from and
//! the `?` overlay prints.
//!
//! A binding names its chords and its wording on [`Action`] itself, so the reference the reader
//! opens and the handler that answers the key cannot describe different keyboards: a variant
//! added to the dispatch does not compile until both are given.

use super::row::{self, RowCtx};
use super::{Board, BULK_LIMIT};
use crate::components::{FocusTargets, ShortcutGroup, ShortcutRow};
use crate::i18n::Translator;
use crate::models::SeriesId;
use dioxus::prelude::*;
use std::collections::HashSet;

/// The screen state a key press writes to.
///
/// One struct rather than one parameter each: the handler needs five signals beside the row
/// context, which is past what `clippy::too_many_arguments` allows.
#[derive(Clone, Copy)]
pub(super) struct Keyboard {
    pub(super) board: Signal<Board>,
    pub(super) selected: Signal<HashSet<SeriesId>>,
    pub(super) focus: Signal<usize>,
    pub(super) menu_for: Signal<Option<SeriesId>>,
    /// Whether the shortcut reference is up.
    pub(super) help: Signal<bool>,
}

/// What a key press on the list asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Action {
    Down,
    Up,
    Open,
    Select,
    Menu,
    Mute,
    Filter,
    SelectAll,
    Clear,
    Help,
}

impl Action {
    /// Every action, in the order the overlay prints them.
    pub(super) const ALL: &'static [Self] = &[
        Self::Down,
        Self::Up,
        Self::Open,
        Self::Select,
        Self::Menu,
        Self::Mute,
        Self::Filter,
        Self::SelectAll,
        Self::Clear,
        Self::Help,
    ];

    /// The chords that run it, spelled the way the overlay prints them. More than one means
    /// *either* — `J` or `↓` — not both together.
    const fn chords(self) -> &'static [&'static str] {
        match self {
            Self::Down => &["J", "↓"],
            Self::Up => &["K", "↑"],
            Self::Open => &["↵"],
            Self::Select => &["X"],
            Self::Menu => &["S"],
            Self::Mute => &["M"],
            Self::Filter => &["/"],
            Self::SelectAll => &["Ctrl+A", "⌘A"],
            Self::Clear => &["Esc"],
            Self::Help => &["?"],
        }
    }

    /// The catalogue key of the line describing it.
    const fn label_key(self) -> &'static str {
        match self {
            Self::Down => "watchlist.keys.down",
            Self::Up => "watchlist.keys.up",
            Self::Open => "watchlist.keys.open",
            Self::Select => "watchlist.keys.select",
            Self::Menu => "watchlist.keys.menu",
            Self::Mute => "watchlist.keys.mute",
            Self::Filter => "watchlist.keys.filter",
            Self::SelectAll => "watchlist.keys.selectAll",
            Self::Clear => "watchlist.keys.clear",
            Self::Help => "watchlist.keys.help",
        }
    }
}

/// The single-character bindings, as `event.key()` reports them once lower-cased.
///
/// A table rather than match arms so the drift test can walk it: a character the handler answers
/// and no chord prints is exactly the divergence this module exists to prevent.
const CHARACTERS: &[(&str, Action)] = &[
    ("j", Action::Down),
    ("k", Action::Up),
    ("x", Action::Select),
    ("s", Action::Menu),
    ("m", Action::Mute),
    ("/", Action::Filter),
    ("?", Action::Help),
];

/// Which action `event` asks for, or `None` for a key this list leaves alone.
///
/// Every binding but `Ctrl`/`⌘`+`A` is inert under a modifier, so the browser's own `⌘K`/`Ctrl+F`
/// keep working. `Shift` is not one of them: it extends the selection, and `?` *is* a shifted
/// key. Nothing here has to test for a focused text field — the handler hangs off the list
/// element, and the filter box is its sibling rather than its child.
pub(super) fn action_for(event: &Event<KeyboardData>) -> Option<Action> {
    let modifiers = event.modifiers();
    if modifiers.ctrl() || modifiers.alt() || modifiers.meta() {
        if !(modifiers.ctrl() || modifiers.meta()) {
            return None;
        }
        return match event.key() {
            Key::Character(c) if c.eq_ignore_ascii_case("a") => Some(Action::SelectAll),
            _ => None,
        };
    }

    match event.key() {
        Key::ArrowDown => Some(Action::Down),
        Key::ArrowUp => Some(Action::Up),
        Key::Enter => Some(Action::Open),
        Key::Escape => Some(Action::Clear),
        Key::Character(c) => {
            let pressed = c.to_ascii_lowercase();
            CHARACTERS
                .iter()
                .find(|(chord, _)| *chord == pressed.as_str())
                .map(|&(_, action)| action)
        }
        _ => None,
    }
}

/// Dispatch one key press against the list.
///
/// `J`/`K` alongside the arrow keys (hands stay on the home row while triaging).
#[expect(
    clippy::large_types_passed_by_value,
    reason = "`RowCtx` reaches a spawned future through the actions this dispatches to; see \
              its doc comment in `row.rs`"
)]
pub(super) fn on_key(
    event: &Event<KeyboardData>,
    keys: Keyboard,
    ctx: RowCtx,
    focus_targets: FocusTargets,
) {
    let Some(action) = action_for(event) else {
        return;
    };
    let Keyboard {
        board,
        mut selected,
        mut focus,
        mut menu_for,
        mut help,
    } = keys;

    // Both answer with nothing on screen, so they run ahead of the guard below rather than being
    // inert on the empty list the reference is most likely to be opened from.
    match action {
        Action::SelectAll => {
            event.prevent_default();
            let ids: HashSet<SeriesId> = board
                .read()
                .items
                .iter()
                .take(BULK_LIMIT)
                .map(|i| i.series_id)
                .collect();
            selected.set(ids);
            return;
        }
        Action::Help => {
            event.prevent_default();
            help.set(true);
            return;
        }
        _ => {}
    }

    let items = board.read().items.clone();
    if items.is_empty() {
        return;
    }
    let last = items.len() - 1;
    let current = (*focus.read()).min(last);
    let extend = event.modifiers().shift();

    let mut step = |to: usize, selected: &mut Signal<HashSet<SeriesId>>| {
        if extend {
            // Shift-stepping selects the landing row, sweeping a range without tracking an anchor.
            selected.write().insert(items[to].series_id);
        }
        focus.set(to);
    };

    match action {
        Action::Down => {
            event.prevent_default();
            step(current.saturating_add(1).min(last), &mut selected);
        }
        Action::Up => {
            event.prevent_default();
            step(current.saturating_sub(1), &mut selected);
        }
        Action::Open => {
            event.prevent_default();
            row::continue_reading(&items[current]);
        }
        Action::Select => {
            event.prevent_default();
            let id = items[current].series_id;
            let mut selection = selected.write();
            if !selection.remove(&id) && selection.len() < BULK_LIMIT {
                selection.insert(id);
            }
        }
        Action::Menu => {
            event.prevent_default();
            menu_for.set(Some(items[current].series_id));
        }
        Action::Mute => {
            event.prevent_default();
            row::toggle_mute(&items[current], board, ctx);
        }
        Action::Filter => {
            event.prevent_default();
            crate::components::focus_and_select(focus_targets.filter);
        }
        Action::Clear => {
            if menu_for.peek().is_some() {
                menu_for.set(None);
            } else {
                selected.write().clear();
            }
        }
        // Answered above, before the list was required to hold a row.
        Action::SelectAll | Action::Help => {}
    }
}

/// This screen's bindings, worded for the `?` overlay.
pub(super) fn shortcut_group(i18n: Translator) -> ShortcutGroup {
    ShortcutGroup {
        screen: i18n.t("nav.watchlist"),
        rows: Action::ALL
            .iter()
            .map(|action| ShortcutRow {
                chords: action.chords().iter().map(|&c| c.to_owned()).collect(),
                description: i18n.t(action.label_key()),
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Nine bindings shipped with nowhere to learn them, and the obvious fix — a hand-written
    /// table of key names in the overlay — is a second copy of the contract that goes stale the
    /// first time a key moves. It cannot: the overlay prints [`Action::ALL`], and a character the
    /// handler dispatches that no action prints fails here.
    #[test]
    fn every_dispatched_character_is_printed() {
        for (character, action) in CHARACTERS {
            assert!(
                Action::ALL.contains(action),
                "`{character}` dispatches to {action:?}, which the overlay never lists"
            );
            assert!(
                action
                    .chords()
                    .iter()
                    .any(|chord| chord.eq_ignore_ascii_case(character)),
                "`{character}` dispatches to {action:?}, whose printed chords do not include it"
            );
        }
    }

    /// A binding with no wording renders its own catalogue key in the middle of the reference,
    /// and one with no chord renders a blank cell beside a sentence.
    #[test]
    fn every_action_is_worded_and_printable() {
        let mut seen: Vec<Action> = Vec::new();
        for action in Action::ALL.iter().copied() {
            let key = action.label_key();
            assert!(
                crate::i18n::has_key(key),
                "{action:?}: `{key}` is not in the catalogue"
            );
            assert!(!action.chords().is_empty(), "{action:?} prints no chord");
            assert!(!seen.contains(&action), "{action:?} is listed twice");
            seen.push(action);
        }
    }
}
