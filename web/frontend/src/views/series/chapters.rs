//! The chapter list — the series page's primary surface.
//!
//! Every read source is collapsed behind **one merged open control** per chapter: the main half
//! opens the highest-ranked source that actually carries that chapter, the caret half opens the
//! per-chapter source menu. There is deliberately no per-source button anywhere; the monogram
//! column says *who* has the chapter, and the single button says *where it opens*.

use super::model::{
    chapter_key, group_chapters, ChapterGroup, ChapterKey, MergedChapter, RankedSource,
};
use crate::api;
use crate::components::OutcomeLine;
use crate::hooks::{use_busy, use_outcome, Outcome, Reload};
use crate::i18n::use_i18n;
use crate::icons::{Ic, Icon};
use crate::models::{ChapterRead, SeriesId, SeriesSourceId};
use crate::util::{chapter_number, is_fresh, monogram, rel_time};
use dioxus::prelude::*;

/// How many chapter groups the list shows before the "load more" footer.
const PAGE: usize = 25;

/// Which chapters the list is showing.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Filter {
    All,
    Unread,
}

#[component]
pub(super) fn ChapterSection(
    series_id: SeriesId,
    /// Every chapter, newest first, already merged across sources.
    chapters: Vec<MergedChapter>,
    /// The series' sources in resolution order.
    sources: Vec<RankedSource>,
    pinned: Signal<Option<SeriesSourceId>>,
    /// Bumped after a read-state write so the merged list refetches.
    reload: Reload,
) -> Element {
    let i18n = use_i18n();
    let mut filter = use_signal(|| Filter::All);
    let mut hide_parts = use_signal(|| false);
    let mut newest_first = use_signal(|| true);
    let mut shown = use_signal(|| PAGE);
    // At most one source menu is open at a time, keyed by the chapter it belongs to.
    let open_menu = use_signal(|| Option::<ChapterKey>::None);
    // One shared slot for read-toggle failures: the rows are a table, so an error rendered
    // per row would reflow the list under the pointer. Held here, above the list, it stays
    // where the reader is looking after a click.
    let mark_error = use_outcome();

    // Counts describe the whole series, never the current filter: a chip that renamed itself
    // once you clicked it would make the two numbers impossible to compare.
    let total = chapters.iter().filter(|c| !c.is_part()).count();
    let read = chapters
        .iter()
        .filter(|c| !c.is_part() && c.read == Some(true))
        .count();
    let unread = chapters.iter().filter(|c| c.read == Some(false)).count();
    let tracked = chapters.iter().any(|c| c.read.is_some());
    let next_up = super::model::next_unread(&chapters).map(|c| chapter_key(c.number));

    let percent = if total == 0 {
        0.0
    } else {
        // Both counts are list lengths, far inside `f64`'s exact integer range.
        #[allow(clippy::cast_precision_loss)]
        {
            (read as f64 / total as f64) * 100.0
        }
    };

    let mut groups = group_chapters(&chapters);
    if *filter.read() == Filter::Unread {
        groups.retain(|group| {
            group
                .full
                .iter()
                .chain(group.parts.iter())
                .any(|c| c.read == Some(false))
        });
    }
    if !*newest_first.read() {
        groups.reverse();
    }
    let group_total = groups.len();
    let visible = (*shown.read()).min(group_total);
    let hidden = group_total - visible;

    // The footer names the range still folded away, in the direction the list is sorted.
    let remaining_range = groups.get(visible..).and_then(|rest| {
        let first = rest.first()?.lead()?.number;
        let last = rest.last()?.lead()?.number;
        Some((chapter_number(first), chapter_number(last)))
    });
    let load_count = PAGE.min(hidden).to_string();

    rsx! {
        div { class: "ik-ch-head",
            div {
                div { class: "ik-sec-lbl", {i18n.t("series.chapters")} }
                h2 {
                    {
                        i18n.args(
                            "series.countLine",
                            &[("total", &total.to_string()), ("read", &read.to_string())],
                        )
                    }
                }
            }
            OpensOn { sources: sources.clone(), pinned }
        }

        if tracked {
            div { class: "ik-progress thin", style: "margin:12px 0 14px;",
                span { style: "width:{percent}%;" }
            }
        }

        div { class: "ik-flex", style: "margin-bottom:12px;flex-wrap:wrap;gap:8px;",
            button {
                class: if *filter.read() == Filter::All { "ik-chip active" } else { "ik-chip" },
                onclick: move |_| filter.set(Filter::All),
                {i18n.args("series.filterAll", &[("count", &total.to_string())])}
            }
            if tracked {
                button {
                    class: if *filter.read() == Filter::Unread { "ik-chip active" } else { "ik-chip" },
                    onclick: move |_| filter.set(Filter::Unread),
                    {i18n.args("series.filterUnread", &[("count", &unread.to_string())])}
                }
            }
            button {
                class: if *hide_parts.read() { "ik-chip active" } else { "ik-chip" },
                onclick: move |_| {
                    let next = !*hide_parts.read();
                    hide_parts.set(next);
                },
                {i18n.t("series.hideParts")}
            }
            button {
                class: "ik-btn bare ik-mono",
                style: "margin-left:auto;font-size:11.5px;",
                onclick: move |_| {
                    let next = !*newest_first.read();
                    newest_first.set(next);
                },
                if *newest_first.read() {
                    {i18n.t("series.newestFirst")}
                } else {
                    {i18n.t("series.oldestFirst")}
                }
                Ic { icon: Icon::ChevronDown, size: 13 }
            }
        }

        OutcomeLine { outcome: mark_error.read().clone() }

        div { class: "ik-chtable",
            for (index , group) in groups.into_iter().take(visible).enumerate() {
                GroupRows {
                    key: "{index}",
                    group,
                    series_id,
                    sources: sources.clone(),
                    pinned,
                    open_menu,
                    next_up,
                    hide_parts: *hide_parts.read(),
                    reload,
                    mark_error,
                }
            }
            if let Some((from, to)) = remaining_range {
                button {
                    class: "ik-chfoot",
                    onclick: move |_| {
                        let next = *shown.read() + PAGE;
                        shown.set(next);
                    },
                    span { style: "font-weight:500;font-size:12.5px;color:var(--muted);",
                        {i18n.args("series.olderChapters", &[("from", &from), ("to", &to)])}
                    }
                    span { class: "more",
                        {i18n.args("series.loadMore", &[("count", &load_count)])}
                    }
                }
            }
        }
        p { class: "ik-chnote", {i18n.t("series.carriersNote")} }
    }
}

/// The resolved open target, stated once above the list: which source the buttons will use and
/// which one backs it up. Read-only here — the picker lives in each chapter's source menu.
#[component]
fn OpensOn(sources: Vec<RankedSource>, pinned: Signal<Option<SeriesSourceId>>) -> Element {
    let i18n = use_i18n();
    let Some(lead) = sources.first() else {
        return rsx! {};
    };
    let backup = sources.get(1).map(|s| s.source.provider_name.clone());
    let is_pinned = *pinned.read() == Some(lead.source.id);

    rsx! {
        div { class: "ik-opens",
            div { class: "ik-mono", style: "font-size:11.5px;color:var(--faint);",
                {i18n.t("series.opensOn")}
            }
            div { class: "ik-flex", style: "gap:7px;margin-top:3px;justify-content:flex-end;",
                span { style: "font-weight:600;font-size:13.5px;", "{lead.source.provider_name}" }
                if let Some(backup) = backup {
                    span { class: "ik-mono", style: "font-size:11.5px;color:var(--faint);",
                        "→ {backup}"
                    }
                }
                if is_pinned {
                    span { class: "ik-mono", style: "font-size:11.5px;color:var(--acc3);",
                        {i18n.t("series.pinned")}
                    }
                }
            }
        }
    }
}

/// One chapter group: the whole chapter, then its part releases behind a toggle.
#[component]
fn GroupRows(
    group: ChapterGroup,
    series_id: SeriesId,
    sources: Vec<RankedSource>,
    pinned: Signal<Option<SeriesSourceId>>,
    open_menu: Signal<Option<ChapterKey>>,
    next_up: Option<ChapterKey>,
    hide_parts: bool,
    reload: Reload,
    /// Shared slot for a failed read-toggle, owned by [`ChapterSection`].
    mark_error: Signal<Outcome>,
) -> Element {
    let i18n = use_i18n();
    let mut expanded = use_signal(|| false);
    let has_full = group.full.is_some();
    let parts = group.parts.clone();

    // Ascending range for the toggle label: `parts` is newest-first.
    let lo = parts.last().map_or(0.0, |c| c.number);
    let hi = parts.first().map_or(0.0, |c| c.number);
    let count = i64::try_from(parts.len()).unwrap_or(i64::MAX);
    let toggle_label = i18n.plural(
        "series.partReleases",
        count,
        &[("from", &chapter_number(lo)), ("to", &chapter_number(hi))],
    );
    // A lone group of parts is the reading frontier — collapsing it would hide the newest
    // chapter behind a disclosure.
    let show_parts = !hide_parts && (!has_full || *expanded.read());

    rsx! {
        if let Some(chapter) = group.full.clone() {
            ChapterRow {
                chapter,
                series_id,
                sources: sources.clone(),
                pinned,
                open_menu,
                next_up,
                is_part: false,
                reload,
                mark_error,
            }
        }
        if has_full && !parts.is_empty() && !hide_parts {
            button {
                class: "ik-chapter-toggle",
                r#type: "button",
                "aria-expanded": if *expanded.read() { "true" } else { "false" },
                onclick: move |_| {
                    let next = !*expanded.read();
                    expanded.set(next);
                },
                Ic {
                    icon: Icon::ChevronRight,
                    size: 14,
                    class: if *expanded.read() { "ik-chevron open" } else { "ik-chevron" },
                }
                span { "{toggle_label}" }
            }
        }
        if show_parts {
            for chapter in parts {
                ChapterRow {
                    key: "{chapter.number}",
                    chapter,
                    series_id,
                    sources: sources.clone(),
                    pinned,
                    open_menu,
                    next_up,
                    is_part: true,
                    reload,
                    mark_error,
                }
            }
        }
    }
}

#[component]
fn ChapterRow(
    chapter: MergedChapter,
    series_id: SeriesId,
    sources: Vec<RankedSource>,
    pinned: Signal<Option<SeriesSourceId>>,
    open_menu: Signal<Option<ChapterKey>>,
    next_up: Option<ChapterKey>,
    is_part: bool,
    reload: Reload,
    /// Shared slot for a failed read-toggle, owned by [`ChapterSection`].
    mark_error: Signal<Outcome>,
) -> Element {
    let api = api::use_api();
    let i18n = use_i18n();
    let busy = use_busy();
    let mut mark_error = mark_error;

    let key = chapter_key(chapter.number);
    let is_next = next_up == Some(key);
    let number = chapter.number;
    let label_number = chapter_number(number);
    let title = chapter.title.clone().unwrap_or_else(|| {
        let catalogue_key = if is_part {
            "series.partNumbered"
        } else {
            "series.chapterNumbered"
        };
        i18n.args(catalogue_key, &[("number", &label_number)])
    });

    let resolved = chapter.resolved().clone();
    let fresh = is_fresh(resolved.published_at.as_deref());
    let when = rel_time(i18n, resolved.published_at.as_deref());
    let is_read = chapter.read == Some(true);
    let can_track = chapter.read.is_some();
    let mark_label = if is_read {
        i18n.t("common.markUnread")
    } else {
        i18n.t("common.markRead")
    };
    let state_label = if is_read {
        i18n.t("series.read")
    } else {
        i18n.t("series.unread")
    };

    // Per-chapter read toggle, kept on the read indicator itself so the row gains no column.
    // The endpoint applies the two-scalar rule server-side, so marking a part release advances
    // the part frontier and never corrupts whole-chapter progress; external services that have
    // no notion of parts only ever receive the whole-chapter frontier.
    //
    // A failure is named rather than swallowed: the row's only feedback is the refetched read
    // state, so a discarded error is indistinguishable from a button that does nothing.
    let failed_label = i18n.args("series.markFailed", &[("number", &label_number)]);
    let toggle_read = move |_| {
        if !busy.claim() {
            return;
        }
        mark_error.set(None);
        let failed_label = failed_label.clone();
        let client = api.client();
        spawn(async move {
            match client
                .put_chapter_progress()
                .series_id(series_id)
                .number(number)
                .body(ChapterRead { read: !is_read })
                .send()
                .await
            {
                Ok(_) => reload.bump(),
                Err(e) => {
                    let reason = api::friendly_error(i18n, e);
                    mark_error.set(Some(Err(format!("{failed_label} {reason}"))));
                }
            }
            busy.release();
        });
    };

    let row_class = match (is_next, is_part) {
        (true, _) => "ik-chrow next",
        (false, true) => "ik-chrow part",
        (false, false) => "ik-chrow",
    };

    rsx! {
        div { class: "{row_class}",
            // States read/unread; the button in the action cell is what changes it.
            if can_track {
                span { class: "mark", "aria-label": "{state_label}",
                    if is_read {
                        Ic { icon: Icon::Check, size: 13 }
                    } else {
                        span { class: "dot" }
                    }
                }
            } else {
                span {}
            }
            span { class: "num", {i18n.args("series.chapterShort", &[("number", &label_number)])} }
            div { class: "cell",
                div { class: "ttl", "{title}" }
                if is_next {
                    div { class: "sub", {i18n.t("series.nextUp")} }
                } else if is_part {
                    div { class: "sub", {i18n.t("series.part")} }
                }
            }
            div { class: "carriers",
                Carriers { chapter: chapter.clone() }
            }
            span { class: if fresh { "fresh new" } else { "fresh" }, "{when}" }
            div { class: "act",
                if can_track {
                    button {
                        class: "ik-btn xs toggle-read",
                        disabled: busy.is_busy(),
                        onclick: toggle_read,
                        "{mark_label}"
                    }
                }
                OpenControl {
                    chapter,
                    sources,
                    pinned,
                    open_menu,
                    filled: is_next,
                    compact: true,
                    label: i18n.t("common.open"),
                }
            }
        }
    }
}

/// Monogram tiles for the sources carrying this chapter, resolved source first. Anything past
/// the third collapses into a `+n` tile so the column keeps its width.
#[component]
fn Carriers(chapter: MergedChapter) -> Element {
    const VISIBLE: usize = 3;
    let i18n = use_i18n();
    let extra = chapter.carriers.len().saturating_sub(VISIBLE);
    let preferred = chapter.resolved().source_id;
    let more_title = i18n.args("series.moreSources", &[("count", &extra.to_string())]);

    rsx! {
        for carrier in chapter.carriers.iter().take(VISIBLE) {
            span {
                key: "{carrier.source_id}",
                class: if carrier.source_id == preferred { "ik-mono-tile pref" } else { "ik-mono-tile" },
                title: "{carrier.provider_name}",
                {monogram(&carrier.provider_name)}
            }
        }
        if extra > 0 {
            span { class: "ik-mono-tile more", title: "{more_title}", "+{extra}" }
        }
    }
}

/// The merged open control: one button, two halves, no per-source alternatives.
#[component]
pub(super) fn OpenControl(
    chapter: MergedChapter,
    sources: Vec<RankedSource>,
    pinned: Signal<Option<SeriesSourceId>>,
    open_menu: Signal<Option<ChapterKey>>,
    /// The page's or the row's primary action, rendered filled rather than ghosted.
    filled: bool,
    /// Row geometry (smaller type, tighter padding) rather than the hero's.
    compact: bool,
    label: String,
) -> Element {
    let i18n = use_i18n();
    let mut open_menu = open_menu;
    let key = chapter_key(chapter.number);
    let resolved = chapter.resolved().clone();
    let is_open = *open_menu.read() == Some(key);
    let glyph = if compact { 13 } else { 14 };
    let open_title = i18n.args(
        "series.opensOnSource",
        &[("source", &resolved.provider_name)],
    );

    let class = match (filled, compact) {
        (true, true) => "ik-split filled sm",
        (true, false) => "ik-split filled",
        (false, true) => "ik-split ghost sm",
        (false, false) => "ik-split ghost",
    };
    // A chapter row's control sits at the right edge of the table, so its menu hangs leftward;
    // the hero's sits at the left of the action row, so its menu hangs rightward.
    let anchor = if compact {
        "ik-openctl"
    } else {
        "ik-openctl start"
    };

    rsx! {
        if is_open {
            button {
                class: "ik-menu-backdrop",
                "aria-hidden": "true",
                tabindex: "-1",
                onclick: move |_| open_menu.set(None),
            }
        }
        // The wrapper is what the menu is positioned against — see `.ik-openctl`.
        div { class: "{anchor}",
            div { class: "{class}",
                a {
                    class: "main",
                    href: "{resolved.url}",
                    target: "_blank",
                    rel: "noopener",
                    title: "{open_title}",
                    "{label}"
                    Ic { icon: Icon::OpenInNew, size: glyph }
                }
                button {
                    class: "caret",
                    "aria-expanded": if is_open { "true" } else { "false" },
                    title: i18n.t("series.chooseSource"),
                    onclick: move |_| {
                        let next = if is_open { None } else { Some(key) };
                        open_menu.set(next);
                    },
                    Ic { icon: Icon::ChevronDown, size: glyph }
                }
            }
            if is_open {
                SourceMenu { chapter, sources, pinned, open_menu }
            }
        }
    }
}

/// The per-chapter source menu: who carries it, who does not and why, and the pin that makes a
/// source lead for the whole series.
#[component]
fn SourceMenu(
    chapter: MergedChapter,
    sources: Vec<RankedSource>,
    pinned: Signal<Option<SeriesSourceId>>,
    open_menu: Signal<Option<ChapterKey>>,
) -> Element {
    let i18n = use_i18n();
    let mut open_menu = open_menu;
    let label_number = chapter_number(chapter.number);
    let preferred = chapter.resolved().source_id;

    rsx! {
        div {
            class: "ik-srcmenu",
            onkeydown: move |event| {
                if event.key() == Key::Escape {
                    open_menu.set(None);
                }
            },
            div { class: "head", {i18n.args("series.menuTitle", &[("number", &label_number)])} }
            for ranked in sources.iter().cloned() {
                SourceMenuRow {
                    key: "{ranked.source.id}",
                    ranked,
                    chapter: chapter.clone(),
                    preferred,
                    pinned,
                    open_menu,
                }
            }
            div { class: "foot",
                span { style: "font-size:11.5px;color:var(--muted);", {i18n.t("series.pinHint")} }
                span { class: "ik-mono", style: "margin-left:auto;font-size:10.5px;color:var(--faint);",
                    {i18n.t("series.enterHint")}
                }
            }
        }
    }
}

/// One source in the menu: openable when it carries the chapter, explained when it does not.
#[component]
fn SourceMenuRow(
    ranked: RankedSource,
    chapter: MergedChapter,
    preferred: SeriesSourceId,
    pinned: Signal<Option<SeriesSourceId>>,
    open_menu: Signal<Option<ChapterKey>>,
) -> Element {
    let i18n = use_i18n();
    let mut pinned = pinned;
    let mut open_menu = open_menu;
    let source_id = ranked.source.id;
    let name = ranked.source.provider_name.clone();
    let tile = monogram(&name);
    let is_preferred = source_id == preferred;

    let Some(carrier) = chapter
        .carriers
        .iter()
        .find(|c| c.source_id == source_id)
        .cloned()
    else {
        // Not carried: say why, in the source's own terms, and offer nothing to click.
        let why = match ranked.ceiling {
            Some(ceiling) if ceiling < chapter.number => {
                i18n.args("series.onlyUpTo", &[("number", &chapter_number(ceiling))])
            }
            _ => i18n.t("series.notCarried"),
        };
        return rsx! {
            div { class: "ik-srcrow off",
                span { class: "ik-mono-tile md", "{tile}" }
                span {
                    span { class: "nm", style: "display:block;color:var(--text-3);", "{name}" }
                    span { class: "why", style: "display:block;", "{why}" }
                }
            }
        };
    };

    let role = if is_preferred {
        i18n.t("series.preferred")
    } else {
        i18n.t("series.backup")
    };
    let when = rel_time(i18n, carrier.published_at.as_deref());

    rsx! {
        div { class: if is_preferred { "ik-srcrow preferred" } else { "ik-srcrow" },
            a {
                href: "{carrier.url}",
                target: "_blank",
                rel: "noopener",
                onclick: move |_| open_menu.set(None),
                span { class: if is_preferred { "ik-mono-tile md pref" } else { "ik-mono-tile md" },
                    "{tile}"
                }
                span { style: "min-width:0;",
                    span { class: "nm", style: "display:block;", "{name}" }
                    span {
                        class: if is_preferred { "why pref" } else { "why" },
                        style: "display:block;",
                        "{role} · {when}"
                    }
                }
            }
            if is_preferred {
                span { style: "margin-left:auto;display:flex;color:var(--acc);",
                    Ic { icon: Icon::Check, size: 14 }
                }
            } else {
                button {
                    class: "ik-pinbtn",
                    title: i18n.t("series.pinSource"),
                    onclick: move |_| {
                        pinned.set(Some(source_id));
                        open_menu.set(None);
                    },
                    Ic { icon: Icon::Bookmark, size: 12 }
                }
            }
        }
    }
}
