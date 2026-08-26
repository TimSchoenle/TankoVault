//! Reusable Inkstone UI components (design §17.3/§17.4).

mod bottombar;
mod confirm;
mod cover;
mod data;
mod feedback;
mod focus;
mod footer;
mod form;
mod grid;
mod layout;
mod nav;
mod pagination;
mod recommend;
// The desktop build draws its own window header, and carries the one settings surface that has
// to work when the server does not. Neither has a web counterpart.
#[cfg(feature = "desktop")]
mod settings;
mod shell;
mod shortcuts;
mod step_up;
mod tabs;
#[cfg(feature = "desktop")]
mod titlebar;
mod topbar;
// An app that outlives its own window. There is no browser counterpart, and nothing here is
// reachable from the web build.
#[cfg(feature = "desktop")]
mod tray;
// What a release changed, on the start that first runs it. Desktop only: a web SPA is updated by
// reloading it, and there is nothing to have missed.
#[cfg(feature = "desktop")]
mod whats_new;
mod wordmark;

pub(crate) use bottombar::BottomTabs;
pub(crate) use confirm::{InlineConfirm, TypeToConfirm};
pub(crate) use cover::{CardMeta, Cover, CoverCard};
pub(crate) use data::{HealthPill, Kpi};
pub(crate) use feedback::{
    async_block, async_block_list, async_list, async_view, AuthRequired, EmptyBox, ErrorBox,
    ErrorLine, OutcomeLine, SkeletonBlock, SkeletonGrid, SkeletonRows,
};
pub(crate) use focus::{focus_and_select, use_focus_targets, FocusTargets};
pub(crate) use footer::Footer;
pub(crate) use form::{Field, ListSearch, SegControl, SliderRow};
pub(crate) use grid::{unmeasured, use_grid_fill, use_grid_fit, GridFit, GridFitProbe};
pub(crate) use layout::{ListFooter, NoSelection, PanelCard, Section};
pub(crate) use pagination::{CompactPager, Pagination, Window};
pub(crate) use recommend::RecCard;
#[cfg(feature = "desktop")]
pub(crate) use settings::SettingsSheet;
pub(crate) use shell::Shell;
pub(crate) use shortcuts::{ShortcutGroup, ShortcutRow, ShortcutsOverlay};
pub(crate) use step_up::{use_step_up_gate, StepUpGate, StepUpGuard};
pub(crate) use tabs::{TabBar, TabKind};
#[cfg(feature = "desktop")]
pub(crate) use titlebar::TitleBar;
#[cfg(feature = "desktop")]
pub(crate) use tray::{CloseToTray, TrayHost};
#[cfg(feature = "desktop")]
pub(crate) use whats_new::WhatsNew;
pub(crate) use wordmark::Wordmark;

use dioxus::prelude::*;

/// Global unread-notification count, provided at the app root and updated by the SSE stream.
#[derive(Clone, Copy)]
pub(crate) struct UnreadBadge(pub Signal<i64>);

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    /// No screen may spell a button's classes itself.
    ///
    /// This is the rule the `inkstone_ui::Button` extraction exists to make enforceable, and it
    /// is a test rather than a convention because the failure it prevents is invisible: `class:
    /// "ik-btn danger"` compiles, renders, and silently drew an ordinary neutral button, because
    /// no `.ik-btn.danger` rule was ever written. Four modifiers (`danger`, `ghost`,
    /// `vermilion`, `active`) were dead this way across five call sites, one of them a queue
    /// drain. `Tone` and `Size` can only name classes the stylesheet defines; a raw string
    /// cannot, so raw strings are banned. Use `Button`, `ToggleButton`, `IconButton`, or
    /// `inkstone_ui::button_class` where the element has to be an `<a>`.
    #[test]
    fn no_screen_writes_button_classes_by_hand() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut offenders = Vec::new();
        walk(&root, &mut |path, text| {
            for (number, line) in text.lines().enumerate() {
                if line.contains("class: \"ik-btn") {
                    offenders.push(format!(
                        "{}:{}",
                        path.strip_prefix(&root).unwrap_or(path).display(),
                        number + 1
                    ));
                }
            }
        });
        assert!(
            offenders.is_empty(),
            "button classes written by hand at {offenders:?} — use `Button`/`button_class`"
        );
    }

    fn walk(dir: &Path, visit: &mut impl FnMut(&Path, &str)) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, visit);
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                if let Ok(text) = fs::read_to_string(&path) {
                    visit(&path, &text);
                }
            }
        }
    }
}
