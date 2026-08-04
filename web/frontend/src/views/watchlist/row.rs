//! The 54px watchlist row, its overflow menu, and the band heading above it.
//!
//! Status, mute and mark-read write the row optimistically and roll back on failure; only
//! operations that change which rows exist (removal, bulk, sync) still trigger a refetch.

use super::{Board, Bucket, BULK_LIMIT};
use crate::api;
use crate::components::Cover;
use crate::hooks::{Outcome, Reload};
use crate::i18n::{use_i18n, Translator};
use crate::icons::{Ic, Icon};
use crate::models::*;
use crate::util::{chapter_number, rel_time, thousands};
use crate::Route;
use dioxus::prelude::*;
use std::collections::HashSet;
use std::rc::Rc;

/// The handles every row and menu needs, provided once by the view instead of threaded through
/// six props per component.
///
/// A newtype rather than bare `Signal<Outcome>`, so `use_context` can't match some other
/// same-shaped slot by accident.
///
/// Taken **by value** everywhere: it trips `clippy::large_types_passed_by_value` (288 vs 256
/// bytes), but every action moves it into a `spawn`ed future, which no `&RowCtx` reference could
/// outlive — copying it is just four pointer copies, hence the `#[expect]` on every call site.
#[derive(Clone, Copy)]
pub(super) struct RowCtx {
    pub(super) api: api::Api,
    pub(super) i18n: Translator,
    pub(super) reload: Reload,
    pub(super) outcome: Signal<Outcome>,
}

impl RowCtx {
    /// Report a mutation that the server refused. The optimistic write has already been rolled
    /// back by the caller; this is what tells the reader it happened.
    pub(super) fn failed(mut self, error: progenitor_client::Error<ProblemDetails>) {
        self.outcome
            .set(Some(Err(api::friendly_error(self.i18n, error))));
    }
}

/// Open the next unread chapter of `item`.
///
/// A navigation, not a request — distinct from the title's plain link (which a middle-click
/// can open in a tab); this is what `↵` is bound to.
pub(super) fn continue_reading(item: &WatchlistItem) {
    navigator().push(Route::Series {
        id: item.series_id.to_string(),
    });
}

/// How many carrier tiles fit the 132px `Sources` cell before the overflow count.
const SOURCE_TILES: usize = 4;

/// The carrier monograms for one row: preferred first and tinted, the rest neutral, anything
/// past [`SOURCE_TILES`] folded into a `+n`.
///
/// The same 22px `.ik-mono-tile` the series page's chapter rows use, so a source is recognisable
/// as the same thing on both screens.
fn source_tiles(i18n: Translator, sources: &[WatchlistSource]) -> Element {
    let shown = sources.len().min(SOURCE_TILES);
    let overflow = sources.len() - shown;
    rsx! {
        for source in sources.iter().take(shown) {
            span {
                key: "{source.code}",
                class: match (source.preferred, source.state == ProviderState::Active) {
                    (true, true) => "ik-mono-tile pref",
                    (true, false) => "ik-mono-tile pref off",
                    (false, true) => "ik-mono-tile",
                    (false, false) => "ik-mono-tile off",
                },
                title: "{source.name}",
                {monogram(&source.code)}
            }
        }
        if overflow > 0 {
            span {
                class: "ik-mono-tile more",
                title: i18n.plural("watchlist.moreSources", i64::try_from(overflow).unwrap_or(0), &[]),
                "+{overflow}"
            }
        }
    }
}

/// Two characters of a provider slug, uppercased — what fits a 22px tile.
fn monogram(code: &str) -> String {
    code.chars()
        .filter(|c| c.is_alphanumeric())
        .take(2)
        .collect::<String>()
        .to_uppercase()
}

/// Find a row by id — the list is short enough (one page, a few hundred rows) that a scan
/// beats an index kept in sync with every optimistic insert and removal.
fn index_of(board: &Signal<Board>, series_id: SeriesId) -> Option<usize> {
    board
        .read()
        .items
        .iter()
        .position(|i| i.series_id == series_id)
}

/// Flip a row's notification flag, writing the row first and restoring it on failure.
#[expect(
    clippy::large_types_passed_by_value,
    reason = "`RowCtx` outlives this call inside a spawned future; see its doc comment"
)]
pub(super) fn toggle_mute(item: &WatchlistItem, mut board: Signal<Board>, ctx: RowCtx) {
    let series_id = item.series_id;
    let status = item.status;
    let Some(index) = index_of(&board, series_id) else {
        return;
    };
    let previous = board.read().items[index].notify;
    board.write().items[index].notify = !previous;

    let client = ctx.api.client();
    spawn(async move {
        let body = WatchlistUpsert {
            status: Some(status),
            notify: Some(!previous),
        };
        if let Err(e) = client
            .put_watchlist()
            .series_id(series_id)
            .body(body)
            .send()
            .await
        {
            // Put it back — a toggle that silently fails is worse than one that visibly refuses.
            if let Some(index) = index_of(&board, series_id) {
                board.write().items[index].notify = previous;
            }
            ctx.failed(e);
        }
    });
}

/// Move a row to another status.
///
/// Updated in place, but **not** removed from a filtered tab it no longer belongs to — rows
/// vanishing mid-triage is disorienting; the next fetch drops it instead.
#[expect(
    clippy::large_types_passed_by_value,
    reason = "`RowCtx` outlives this call inside a spawned future; see its doc comment"
)]
fn set_status(item: &WatchlistItem, next: WatchStatus, mut board: Signal<Board>, ctx: RowCtx) {
    let series_id = item.series_id;
    let notify = item.notify;
    let Some(index) = index_of(&board, series_id) else {
        return;
    };
    let previous = board.read().items[index].status;
    if previous == next {
        return;
    }
    {
        let mut write = board.write();
        write.items[index].status = next;
        if let Some(counts) = write.counts.as_mut() {
            move_count(counts, previous, next);
        }
    }

    let client = ctx.api.client();
    spawn(async move {
        let body = WatchlistUpsert {
            status: Some(next),
            notify: Some(notify),
        };
        if let Err(e) = client
            .put_watchlist()
            .series_id(series_id)
            .body(body)
            .send()
            .await
        {
            if let Some(index) = index_of(&board, series_id) {
                let mut write = board.write();
                write.items[index].status = previous;
                if let Some(counts) = write.counts.as_mut() {
                    move_count(counts, next, previous);
                }
            }
            ctx.failed(e);
        }
    });
}

/// Move one entry between two status buckets. `all` is untouched — the entry did not leave the
/// watchlist, it changed shelf.
fn move_count(counts: &mut WatchlistCounts, from: WatchStatus, to: WatchStatus) {
    *bucket_mut(counts, from) -= 1;
    *bucket_mut(counts, to) += 1;
}

fn bucket_mut(counts: &mut WatchlistCounts, status: WatchStatus) -> &mut i64 {
    match status {
        WatchStatus::Reading => &mut counts.reading,
        WatchStatus::Planned => &mut counts.planned,
        WatchStatus::Paused => &mut counts.paused,
        WatchStatus::Completed => &mut counts.completed,
        WatchStatus::Dropped => &mut counts.dropped,
    }
}

/// Mark every chapter of one series read, via the bulk endpoint with a single id — a second
/// code path for "everything" is how the two definitions would drift.
#[expect(
    clippy::large_types_passed_by_value,
    reason = "`RowCtx` outlives this call inside a spawned future; see its doc comment"
)]
fn mark_all_read(item: &WatchlistItem, mut board: Signal<Board>, ctx: RowCtx) {
    let series_id = item.series_id;
    let Some(index) = index_of(&board, series_id) else {
        return;
    };
    let previous = board.read().items[index].clone();
    {
        let mut write = board.write();
        write.items[index].unread = 0;
        write.items[index].last_read_number = previous.latest_chapter_number;
    }

    let client = ctx.api.client();
    spawn(async move {
        if let Err(e) = client
            .bulk_mark_read()
            .body(WatchlistBulkIds {
                series_ids: vec![series_id],
            })
            .send()
            .await
        {
            if let Some(index) = index_of(&board, series_id) {
                board.write().items[index] = previous;
            }
            ctx.failed(e);
        }
    });
}

/// Untrack a series — this one does remove the row, since the entry no longer exists.
#[expect(
    clippy::large_types_passed_by_value,
    reason = "`RowCtx` outlives this call inside a spawned future; see its doc comment"
)]
fn remove(item: &WatchlistItem, mut board: Signal<Board>, ctx: RowCtx) {
    let series_id = item.series_id;
    let Some(index) = index_of(&board, series_id) else {
        return;
    };
    let previous = board.read().items[index].clone();
    {
        let mut write = board.write();
        write.items.remove(index);
        write.total -= 1;
        if let Some(counts) = write.counts.as_mut() {
            *bucket_mut(counts, previous.status) -= 1;
            counts.all -= 1;
        }
    }

    let client = ctx.api.client();
    spawn(async move {
        if let Err(e) = client.delete_watchlist().series_id(series_id).send().await {
            let mut write = board.write();
            let at = index.min(write.items.len());
            write.total += 1;
            if let Some(counts) = write.counts.as_mut() {
                *bucket_mut(counts, previous.status) += 1;
                counts.all += 1;
            }
            write.items.insert(at, previous);
            drop(write);
            ctx.failed(e);
        }
    });
}

/// A band heading, sticky under the toolbar: which band, how much is in it, and the one action
/// that clears it.
#[component]
pub(super) fn GroupHeader(
    band: Bucket,
    title_count: i64,
    chapter_count: i64,
    /// The loaded rows in this band. Empty bands never render a header.
    ids: Vec<SeriesId>,
    /// Whether every row of the band is loaded — otherwise `Mark group read` would silently
    /// mark a fraction and report success.
    complete: bool,
) -> Element {
    let i18n = use_i18n();
    let ctx = use_context::<RowCtx>();
    let actionable = complete && !ids.is_empty() && ids.len() <= BULK_LIMIT;
    let hint = if actionable {
        i18n.t("watchlist.markGroupRead")
    } else if ids.len() > BULK_LIMIT {
        i18n.args(
            "watchlist.markGroupTooBig",
            &[("limit", &BULK_LIMIT.to_string())],
        )
    } else {
        i18n.t("watchlist.markGroupIncomplete")
    };

    rsx! {
        div { class: "ik-wl-band", role: "row",
            span { class: "ik-wl-band-name", {i18n.t(band.label_key())} }
            span { class: "ik-wl-band-stats",
                {
                    i18n.args(
                        "watchlist.groupStats",
                        &[
                            ("titles", &thousands(title_count)),
                            ("chapters", &thousands(chapter_count)),
                        ],
                    )
                }
            }
            button {
                class: "ik-wl-band-act",
                r#type: "button",
                disabled: !actionable,
                title: "{hint}",
                onclick: move |_| {
                    let client = ctx.api.client();
                    let ids = ids.clone();
                    spawn(async move {
                        match client
                            .bulk_mark_read()
                            .body(WatchlistBulkIds { series_ids: ids })
                            .send()
                            .await
                        {
                            // Changes unread counts across rows and band aggregates — genuinely
                            // warrants a refetch, not a local edit.
                            Ok(_) => ctx.reload.bump(),
                            Err(e) => ctx.failed(e),
                        }
                    });
                },
                {i18n.t("watchlist.markGroupRead")}
            }
        }
    }
}

/// One tracked title.
#[component]
pub(super) fn WatchRow(
    item: WatchlistItem,
    index: usize,
    focused: bool,
    selected: Signal<HashSet<SeriesId>>,
    focus: Signal<usize>,
    menu_for: Signal<Option<SeriesId>>,
    board: Signal<Board>,
) -> Element {
    let i18n = use_i18n();
    let series_id = item.series_id;
    let is_selected = selected.read().contains(&series_id);
    let caught_up = item.unread == 0;

    let read = item.last_read_number.unwrap_or(0.0);
    let total = item.total_chapters;
    let percent = if total > 0 {
        #[expect(
            clippy::cast_precision_loss,
            reason = "a chapter count that loses f64 precision is 2^53 chapters; the bar is 176px"
        )]
        ((read / total as f64) * 100.0).clamp(0.0, 100.0)
    } else {
        0.0
    };
    // Grey (muted) / jade (done) / accent (default) — no room for a second badge in 176px.
    let bar_class = if !item.notify {
        "ik-wl-bar muted"
    } else if percent >= 100.0 {
        "ik-wl-bar done"
    } else {
        "ik-wl-bar"
    };

    let mut class = String::from("ik-wl-row");
    if is_selected {
        class.push_str(" selected");
    }
    if focused {
        class.push_str(" focused");
    }
    if caught_up {
        class.push_str(" caught-up");
    }

    let submeta = if item.source_degraded {
        i18n.t("watchlist.sourceOffline")
    } else {
        let name = item
            .preferred_source_name
            .clone()
            .unwrap_or_else(|| i18n.t("watchlist.noSource"));
        if item.source_count > 1 {
            i18n.args(
                "watchlist.sourceMeta",
                &[("source", &name), ("count", &item.source_count.to_string())],
            )
        } else {
            name
        }
    };

    let row_item = item.clone();
    let menu_item = item.clone();

    rsx! {
        div {
            id: "wl-row-{series_id}",
            class: "{class}",
            role: "row",
            "aria-selected": if is_selected { "true" } else { "false" },
            onclick: move |_| focus.set(index),
            // Real `<input type="checkbox">`, not a styled div — native `Space` toggle and
            // screen-reader announcement.
            span { role: "gridcell",
                input {
                    r#type: "checkbox",
                    class: "ik-wl-check",
                    checked: is_selected,
                    "aria-label": i18n.args("watchlist.selectRow", &[("title", &item.series_title)]),
                    onchange: move |_| {
                        let mut selection = selected.write();
                        if !selection.remove(&series_id) && selection.len() < BULK_LIMIT {
                            selection.insert(series_id);
                        }
                    },
                }
            }

            span { class: "ik-wl-title", role: "gridcell",
                div { class: "ik-wl-thumb",
                    Cover { url: item.cover_url.clone(), title: item.series_title.clone() }
                }
                div { style: "min-width:0;",
                    div { class: "ik-wl-name",
                        Link { to: Route::Series { id: series_id.to_string() }, "{item.series_title}" }
                        if item.source_degraded {
                            span {
                                class: "ik-wl-warn",
                                title: i18n.t("watchlist.sourceOffline"),
                                Ic { icon: Icon::Warning, size: 13 }
                            }
                        }
                        if !item.notify {
                            span { class: "ik-pill", {i18n.t("watchlist.muted")} }
                        }
                    }
                    div {
                        class: if item.source_degraded { "ik-wl-sub warn" } else { "ik-wl-sub" },
                        "{submeta}"
                    }
                }
            }

            // Only rendered above 1500px (`.ik-wl-next` is display:none below the step), where
            // the column answers "what would Continue actually open?" without a hover.
            span { class: "ik-wl-next", role: "gridcell",
                if let Some(next) = crate::models::next_unread(&item) {
                    div { class: "ik-wl-next-ch",
                        span { class: "num",
                            {i18n.args("watchlist.chapterNo", &[("number", &chapter_number(next.number))])}
                        }
                        if let Some(title) = next.title.as_ref().filter(|t| !t.trim().is_empty()) {
                            "{title}"
                        }
                    }
                    if item.unread > 1 {
                        div { class: "ik-wl-next-more",
                            {i18n.plural("watchlist.moreUnread", item.unread - 1, &[])}
                        }
                    }
                } else {
                    span { class: "ik-faint", style: "font-size:12px;", {i18n.t("watchlist.upToDate")} }
                }
            }

            span { class: "ik-wl-progress", role: "gridcell",
                div { class: "ik-mono ik-wl-count",
                    "{chapter_number(read)} / {thousands(total)}"
                }
                div { class: "{bar_class}", span { style: "width:{percent}%;" } }
            }

            span { class: "ik-wl-unread", role: "gridcell",
                if item.unread > 0 {
                    span { class: "ik-pill acc", "{thousands(item.unread)}" }
                } else {
                    span { class: "ik-faint", "—" }
                }
            }

            span { class: "ik-wl-released", role: "gridcell",
                div { {rel_time(i18n, item.latest_chapter_at.as_deref())} }
                if let Some(number) = item.latest_chapter_number {
                    div { class: "ik-mono ik-faint",
                        {i18n.args("watchlist.chapterNo", &[("number", &chapter_number(number))])}
                    }
                }
            }

            span { class: "ik-wl-sources", role: "gridcell",
                {source_tiles(i18n, &item.sources)}
            }

            span { class: "ik-wl-actions", role: "gridcell",
                if caught_up {
                    span { class: "ik-faint", style: "font-size:12px;", {i18n.t("watchlist.upToDate")} }
                } else {
                    button {
                        class: if focused { "ik-wl-continue on" } else { "ik-wl-continue" },
                        r#type: "button",
                        onclick: move |event| {
                            event.stop_propagation();
                            continue_reading(&row_item);
                        },
                        {i18n.t("watchlist.continue")}
                    }
                }
                button {
                    class: "ik-wl-more",
                    r#type: "button",
                    "aria-haspopup": "menu",
                    "aria-expanded": if *menu_for.read() == Some(series_id) { "true" } else { "false" },
                    "aria-label": i18n.args("watchlist.rowMenu", &[("title", &item.series_title)]),
                    onclick: move |event| {
                        event.stop_propagation();
                        let mut menu_for = menu_for;
                        let open = *menu_for.peek() == Some(series_id);
                        menu_for.set(if open { None } else { Some(series_id) });
                        focus.set(index);
                    },
                    Ic { icon: Icon::MoreHoriz, size: 16 }
                }
                if *menu_for.read() == Some(series_id) {
                    RowMenu { item: menu_item, menu_for, board }
                }
            }
        }
    }
}

/// One entry in the row menu. A closed list rather than free-form markup so the arrow-key
/// cursor and the rendered buttons cannot disagree about how many items there are.
#[derive(Clone, Copy, PartialEq, Eq)]
enum MenuItem {
    Move(WatchStatus),
    MarkAllRead,
    ToggleMute,
    ChangeSource,
    Remove,
}

impl MenuItem {
    /// The menu, in order. `Move to` offers the three shelves a title leaves `Reading` for;
    /// `Completed` isn't among them — that's what `Mark all read` is for, not a status picker.
    fn all() -> [MenuItem; 6] {
        [
            Self::Move(WatchStatus::Planned),
            Self::Move(WatchStatus::Paused),
            Self::Move(WatchStatus::Dropped),
            Self::MarkAllRead,
            Self::ToggleMute,
            Self::ChangeSource,
        ]
    }

    /// Rendered after a separator, in the accent colour, because it is the one entry here that
    /// destroys something.
    fn destructive() -> MenuItem {
        Self::Remove
    }
}

/// Run one menu entry.
///
/// Re-reads the row by id rather than closing over `WatchlistItem` — a non-`Copy` capture can't
/// be shared by six `onclick` closures, and re-reading also acts on current state, not a stale
/// snapshot.
#[expect(
    clippy::large_types_passed_by_value,
    reason = "`RowCtx` outlives this call inside a spawned future; see its doc comment"
)]
fn act(
    entry: MenuItem,
    series_id: SeriesId,
    board: Signal<Board>,
    ctx: RowCtx,
    mut menu_for: Signal<Option<SeriesId>>,
) {
    menu_for.set(None);
    let Some(item) = board
        .read()
        .items
        .iter()
        .find(|i| i.series_id == series_id)
        .cloned()
    else {
        return;
    };
    match entry {
        MenuItem::Move(status) => set_status(&item, status, board, ctx),
        MenuItem::MarkAllRead => mark_all_read(&item, board, ctx),
        MenuItem::ToggleMute => toggle_mute(&item, board, ctx),
        // No per-user source override yet, so this opens the series page instead — better than
        // a menu entry that does nothing.
        MenuItem::ChangeSource => {
            navigator().push(Route::Series {
                id: series_id.to_string(),
            });
        }
        MenuItem::Remove => remove(&item, board, ctx),
    }
}

/// The row overflow menu: a popover, focus-managed, `Esc` closes, arrows move.
#[component]
fn RowMenu(
    item: WatchlistItem,
    menu_for: Signal<Option<SeriesId>>,
    board: Signal<Board>,
) -> Element {
    let i18n = use_i18n();
    let ctx = use_context::<RowCtx>();
    let mut cursor = use_signal(|| 0usize);
    let mut handles = use_signal(Vec::<Option<Rc<MountedData>>>::new);
    let entries: Vec<MenuItem> = MenuItem::all()
        .into_iter()
        .chain(std::iter::once(MenuItem::destructive()))
        .collect();
    let count = entries.len();

    let mut move_cursor = move |to: usize| {
        cursor.set(to);
        if let Some(Some(handle)) = handles.read().get(to).cloned() {
            spawn(async move {
                // Best-effort — a refused focus call (detached node mid-close) isn't worth surfacing.
                let _ = handle.set_focus(true).await;
            });
        }
    };

    let series_id = item.series_id;
    let muted = !item.notify;
    let label = move |entry: MenuItem| match entry {
        MenuItem::Move(status) => i18n.args(
            "watchlist.moveTo",
            &[("status", &i18n.t(status.label_key()))],
        ),
        MenuItem::MarkAllRead => i18n.t("watchlist.markAllRead"),
        MenuItem::ToggleMute => i18n.t(if muted {
            "watchlist.unmute"
        } else {
            "watchlist.mute"
        }),
        MenuItem::ChangeSource => i18n.t("watchlist.changeSource"),
        MenuItem::Remove => i18n.t("watchlist.removeFromWatchlist"),
    };

    rsx! {
        // Transparent backdrop, not a document-level listener — closes on outside click without
        // reaching outside this subtree, and stops the click reaching the row underneath.
        div {
            class: "ik-wl-backdrop",
            onclick: move |event| {
                event.stop_propagation();
                let mut menu_for = menu_for;
                menu_for.set(None);
            },
        }
        div {
            class: "ik-wl-menu",
            role: "menu",
            onclick: move |event| event.stop_propagation(),
            onkeydown: move |event| {
                match event.key() {
                    Key::ArrowDown => {
                        event.prevent_default();
                        move_cursor((*cursor.peek() + 1) % count);
                    }
                    Key::ArrowUp => {
                        event.prevent_default();
                        move_cursor((*cursor.peek() + count - 1) % count);
                    }
                    Key::Escape => {
                        event.prevent_default();
                        let mut menu_for = menu_for;
                        menu_for.set(None);
                    }
                    _ => {}
                }
            },
            // Separator is emitted by the item that follows it, not as a loop sibling — `rsx!`
            // only allows a `key` on a block's first node, so a conditional divider ahead of it
            // would leave the button unkeyed and the menu re-created wholesale every render.
            for (index , entry) in entries.into_iter().enumerate() {
                div {
                    key: "{index}",
                    class: if entry == MenuItem::Remove { "ik-wl-menu-group sep" } else { "ik-wl-menu-group" },
                button {
                    class: if entry == MenuItem::Remove { "ik-wl-menu-item danger" } else { "ik-wl-menu-item" },
                    r#type: "button",
                    role: "menuitem",
                    tabindex: if index == *cursor.read() { "0" } else { "-1" },
                    onmounted: move |event| {
                        let mut handles = handles.write();
                        if handles.len() <= index {
                            handles.resize(index + 1, None);
                        }
                        handles[index] = Some(event.data());
                        // First entry takes focus on open, so `S` then arrow keys is one
                        // continuous gesture.
                        if index == 0 {
                            let handle = event.data();
                            spawn(async move {
                                let _ = handle.set_focus(true).await;
                            });
                        }
                    },
                    onclick: move |_| act(entry, series_id, board, ctx, menu_for),
                    {label(entry)}
                }
                }
            }
        }
    }
}
