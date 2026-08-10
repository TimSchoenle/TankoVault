//! Discover's collapsible filter panel and the three chip kinds it is built from.
//!
//! Split out of `views/discover/mod.rs`: `FilterPanel` alone is a screen's worth of markup sitting
//! inside another screen's module. The panel owns no filter state — every control hands the whole
//! next [`DiscoverFilters`] to its caller, which puts it in the URL (see `super::query`).

use super::query::{DiscoverFilters, Tracking, MIN_CHAPTERS_MAX, YEAR_MAX, YEAR_MIN};
use crate::i18n::use_i18n;
use crate::icons::{Ic, Icon};
use crate::models::*;
use crate::state::use_session;
use crate::util::thousands;
use dioxus::prelude::*;

/// Which of the tag chip's three states one tag is in.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum TagState {
    Neutral,
    Include,
    Exclude,
}

// `#[component]` turns these props into a struct, so `too_many_arguments` never fires here.
#[component]
pub(super) fn FilterPanel(
    filters: DiscoverFilters,
    tags: Vec<TagFacet>,
    providers: Vec<PublicProvider>,
    on_change: EventHandler<DiscoverFilters>,
    on_reset: EventHandler<()>,
) -> Element {
    let i18n = use_i18n();
    let session = use_session();
    // Panel-local, not part of the URL: the API has no "match all tags" parameter yet, so putting
    // it in the query string would publish a filter that changes nothing and reset the grid every
    // time it was flipped.
    let mut match_all = use_signal(|| false);
    // The sliders report continuously so the number beside them follows the thumb, but only a
    // released slider is a filter: committing every intermediate value would be one navigation
    // and one catalogue query per pixel dragged.
    let mut year_draft = use_signal(|| Option::<(i32, i32)>::None);
    let mut chapters_draft = use_signal(|| Option::<i32>::None);

    let active_count = filters.active_count();
    let all = *match_all.read();
    let provider_now = filters.provider.clone().unwrap_or_default();
    let (year_min, year_max) = year_draft
        .read()
        .unwrap_or((filters.year_min, filters.year_max));
    let min_chapters = chapters_draft.read().unwrap_or(filters.min_chapters);

    // `EventHandler`, not a bare closure: a closure that captures the filters is neither `Copy`
    // nor cloneable into a `for` body, and one chip per content type needs the same handler.
    let toggle_type = EventHandler::new({
        let filters = filters.clone();
        move |value: ContentType| {
            let mut next = filters.clone();
            next.toggle_type(value);
            on_change.call(next);
        }
    });
    let toggle_status = EventHandler::new({
        let filters = filters.clone();
        move |value: SeriesStatus| {
            let mut next = filters.clone();
            next.toggle_status(value);
            on_change.call(next);
        }
    });
    let cycle_tag = EventHandler::new({
        let filters = filters.clone();
        move |slug: String| {
            let mut next = filters.clone();
            next.cycle_tag(&slug);
            on_change.call(next);
        }
    });
    let set_provider = EventHandler::new({
        let filters = filters.clone();
        move |slug: String| {
            let mut next = filters.clone();
            next.provider = Some(slug).filter(|s| !s.is_empty());
            on_change.call(next);
        }
    });
    // Exclusive, unlike the type and status chips: "only what I track" and "only what I don't"
    // are the two halves of one question, and holding both would ask for an empty catalogue.
    let set_tracking = EventHandler::new({
        let filters = filters.clone();
        move |value: Tracking| {
            let mut next = filters.clone();
            next.tracking = value;
            on_change.call(next);
        }
    });
    // Both commits read the draft rather than a value captured at render time: `input` and
    // `change` are two events, and taking the number from the render between them would commit
    // whatever the slider read one step ago if that render had not landed yet.
    let commit_years = EventHandler::new({
        let filters = filters.clone();
        move |()| {
            let Some((min, max)) = *year_draft.peek() else {
                return;
            };
            year_draft.set(None);
            let next = DiscoverFilters {
                year_min: min.min(max),
                year_max: max.max(min),
                ..filters.clone()
            };
            if next != filters {
                on_change.call(next);
            }
        }
    });
    let commit_chapters = EventHandler::new({
        let filters = filters.clone();
        move |()| {
            let Some(value) = *chapters_draft.peek() else {
                return;
            };
            chapters_draft.set(None);
            if value != filters.min_chapters {
                on_change.call(DiscoverFilters {
                    min_chapters: value,
                    ..filters.clone()
                });
            }
        }
    });

    rsx! {
        aside { class: "ik-filter-panel",
            div { class: "ik-filter-head",
                strong { Ic { icon: Icon::Tune, size: 16 } {i18n.t("discover.filters")} }
                button { class: "reset", onclick: move |_| on_reset.call(()), {i18n.t("common.reset")} }
            }
            div { class: "ik-muted", style: "font-size:12px;",
                if active_count == 0 {
                    {i18n.t("discover.noFilters")}
                } else {
                    {i18n.args("discover.activeFilters", &[("count", &active_count.to_string())])}
                }
            }

            // CONTENT TYPE
            div { class: "ik-filter-group",
                div { class: "lbl", {i18n.t("discover.contentType")} }
                div { class: "ik-chips", style: "margin-bottom:0;",
                    for t in ContentType::all().iter().copied() {
                        TypeChip { t, active: filters.types.contains(&t), on_toggle: toggle_type }
                    }
                }
            }

            // STATUS
            div { class: "ik-filter-group",
                div { class: "lbl", {i18n.t("discover.status")} }
                div { class: "ik-chips", style: "margin-bottom:0;",
                    for s in SeriesStatus::all().iter().copied() {
                        StatusChip { s, active: filters.statuses.contains(&s), on_toggle: toggle_status }
                    }
                }
            }

            // GENRES / TAGS
            div { class: "ik-filter-group",
                div { class: "lbl",
                    {i18n.t("discover.tags")}
                    button {
                        class: "reset",
                        style: "font-family:var(--font-mono);text-transform:none;",
                        onclick: move |_| {
                            let cur = *match_all.peek();
                            match_all.set(!cur);
                        },
                        if all {
                            {i18n.t("discover.matchAll")}
                        } else {
                            {i18n.t("discover.matchAny")}
                        }
                    }
                }
                if tags.is_empty() {
                    div { class: "ik-muted", style: "font-size:12px;", {i18n.t("discover.noTags")} }
                } else {
                    TagFacetPanel {
                        tags: tags.clone(),
                        inc: filters.inc.clone(),
                        exc: filters.exc.clone(),
                        on_cycle: cycle_tag,
                    }
                }
            }

            // YOUR LIBRARY — resolved server-side against the caller's own token, so it is only
            // offered to a reader who has a watchlist for it to mean anything about.
            if session.is_authenticated() {
                div { class: "ik-filter-group",
                    div { class: "lbl", {i18n.t("discover.tracking.label")} }
                    div { class: "ik-chips", style: "margin-bottom:0;",
                        for option in Tracking::ALL {
                            TrackingChip {
                                key: "{option.label_key()}",
                                option,
                                active: filters.tracking == option,
                                on_pick: set_tracking,
                            }
                        }
                    }
                }
            }

            // PROVIDER — public providers list (§9.3), filtered server-side by slug (§9.1).
            div { class: "ik-filter-group",
                div { class: "lbl", {i18n.t("discover.provider")} }
                if providers.is_empty() {
                    div { class: "ik-muted", style: "font-size:12px;", {i18n.t("discover.noProviders")} }
                } else {
                    select {
                        class: "ik-select",
                        style: "width:100%;",
                        value: "{provider_now}",
                        onchange: move |e| set_provider.call(e.value()),
                        option { value: "", selected: provider_now.is_empty(), {i18n.t("discover.allProviders")} }
                        for p in providers.iter().cloned() {
                            option {
                                key: "{p.id}",
                                value: "{p.slug}",
                                selected: provider_now == p.slug,
                                "{p.name} ({p.series_count})"
                            }
                        }
                    }
                }
            }

            // RELEASE YEAR — server-side range (§9.1); bounds sent only when narrowed.
            div { class: "ik-filter-group",
                div { class: "lbl", {i18n.t("discover.releaseYear")} }
                div { class: "ik-range-row",
                    span { "{year_min}" }
                    input {
                        id: "tv-year-min",
                        class: "ik-range",
                        r#type: "range",
                        // Constants, not literals — duplicating these as strings previously
                        // let the sliders drift out of sync with the declared range.
                        min: "{YEAR_MIN}",
                        max: "{YEAR_MAX}",
                        "aria-label": i18n.t("discover.releaseYearFrom"),
                        value: "{year_min}",
                        oninput: move |e| {
                            if let Ok(v) = e.value().parse::<i32>() { year_draft.set(Some((v, year_max))); }
                        },
                        onchange: move |_| commit_years.call(()),
                    }
                    span { "{year_max}" }
                }
                input {
                    id: "tv-year-max",
                    class: "ik-range",
                    r#type: "range",
                    min: "{YEAR_MIN}",
                    max: "{YEAR_MAX}",
                    "aria-label": i18n.t("discover.releaseYearTo"),
                    value: "{year_max}",
                    oninput: move |e| {
                        if let Ok(v) = e.value().parse::<i32>() { year_draft.set(Some((year_min, v))); }
                    },
                    onchange: move |_| commit_years.call(()),
                }
            }

            // MIN. CHAPTERS — server-side (§9.1).
            div { class: "ik-filter-group",
                label { class: "lbl", r#for: "tv-min-chapters",
                    {i18n.t("discover.minChapters")}
                    span { class: "ik-mono", style: "color:var(--muted);", "{min_chapters}+" }
                }
                input {
                    id: "tv-min-chapters",
                    class: "ik-range",
                    r#type: "range",
                    min: "0",
                    max: "{MIN_CHAPTERS_MAX}",
                    step: "10",
                    value: "{min_chapters}",
                    oninput: move |e| {
                        if let Ok(v) = e.value().parse::<i32>() { chapters_draft.set(Some(v)); }
                    },
                    onchange: move |_| commit_chapters.call(()),
                }
            }

            // SAVED PRESETS — stub.
            div { class: "ik-filter-group",
                div { class: "lbl", {i18n.t("discover.presets")} }
                div { class: "ik-muted", style: "font-size:12px;", {i18n.t("discover.presetsSoon")} }
            }
        }
    }
}

#[component]
pub(super) fn TypeChip(
    t: ContentType,
    active: bool,
    on_toggle: EventHandler<ContentType>,
) -> Element {
    let i18n = use_i18n();
    let class = if active { "ik-chip active" } else { "ik-chip" };
    rsx! {
        button {
            class: "{class}",
            r#type: "button",
            "aria-pressed": "{active}",
            onclick: move |_| on_toggle.call(t),
            {i18n.t(t.label_key())}
        }
    }
}

/// One of the three watchlist options. A radio in chip clothing: picking one replaces the
/// selection rather than adding to it.
#[component]
pub(super) fn TrackingChip(
    option: Tracking,
    active: bool,
    on_pick: EventHandler<Tracking>,
) -> Element {
    let i18n = use_i18n();
    let class = if active { "ik-chip active" } else { "ik-chip" };
    rsx! {
        button {
            class: "{class}",
            r#type: "button",
            "aria-pressed": "{active}",
            onclick: move |_| on_pick.call(option),
            {i18n.t(option.label_key())}
        }
    }
}

#[component]
pub(super) fn StatusChip(
    s: SeriesStatus,
    active: bool,
    on_toggle: EventHandler<SeriesStatus>,
) -> Element {
    let i18n = use_i18n();
    let class = if active { "ik-chip active" } else { "ik-chip" };
    rsx! {
        button {
            class: "{class}",
            r#type: "button",
            "aria-pressed": "{active}",
            onclick: move |_| on_toggle.call(s),
            {i18n.t(s.label_key())}
        }
    }
}

/// Chips shown before the reader asks for the rest.
///
/// The list arrives commonest-first, so this prefix is the genres most of the catalogue is
/// actually tagged with rather than the ones whose names happen to sort early. It used to be a
/// flat `.take(40)` over an *alphabetical* list, which is why the panel appeared to stop
/// mid-alphabet: the cap was real, the ordering made it look like the catalogue simply had no
/// tags past whatever letter chip forty landed on.
const VISIBLE_TAGS: usize = 32;

/// The tag facet: a searchable, expandable chip list over the whole vocabulary.
///
/// Three affordances rather than one longer list, because they answer different questions. The
/// prefix answers "what is this catalogue mostly about"; the search box answers "does it have
/// *X*", which is the only workable interaction once a catalogue has hundreds of tags; and
/// "show all" is the escape hatch for browsing the tail. Every tag is reachable through at
/// least one of them, which is the property the old fixed cap did not have.
#[component]
pub(super) fn TagFacetPanel(
    tags: Vec<TagFacet>,
    inc: Vec<String>,
    exc: Vec<String>,
    on_cycle: EventHandler<String>,
) -> Element {
    let i18n = use_i18n();
    let mut query = use_signal(String::new);
    let mut expanded = use_signal(|| false);

    let needle = query.read().trim().to_lowercase();
    let matching: Vec<TagFacet> = if needle.is_empty() {
        tags.clone()
    } else {
        tags.iter()
            .filter(|tag| tag.name.to_lowercase().contains(&needle) || tag.slug.contains(&needle))
            .cloned()
            .collect()
    };
    // A search is already a narrowing, so it shows everything it found: capping a result set the
    // reader deliberately narrowed would hide the very tag they typed the name of.
    let searching = !needle.is_empty();
    let show_all = searching || *expanded.read();
    let hidden = matching.len().saturating_sub(VISIBLE_TAGS);
    let shown: Vec<TagFacet> = if show_all {
        matching.clone()
    } else {
        matching.iter().take(VISIBLE_TAGS).cloned().collect()
    };

    rsx! {
        input {
            class: "ik-input",
            style: "width:100%;margin-bottom:8px;font-size:12.5px;padding:6px 9px;",
            r#type: "search",
            placeholder: i18n.args("discover.tagSearch", &[("count", &tags.len().to_string())]),
            "aria-label": i18n.args("discover.tagSearch", &[("count", &tags.len().to_string())]),
            value: "{query}",
            oninput: move |e| query.set(e.value()),
        }
        if shown.is_empty() {
            div { class: "ik-muted", style: "font-size:12px;",
                {i18n.args("discover.noTagMatch", &[("query", query.read().trim())])}
            }
        } else {
            div { class: "ik-chips", style: "margin-bottom:0;",
                for tag in shown {
                    {
                        let state = if inc.contains(&tag.slug) {
                            TagState::Include
                        } else if exc.contains(&tag.slug) {
                            TagState::Exclude
                        } else {
                            TagState::Neutral
                        };
                        rsx! { TagChip { key: "{tag.slug}", tag, state, on_cycle } }
                    }
                }
            }
            // Only when collapsing actually hides something: a "show fewer" control under a
            // list that is already complete is a control that does nothing.
            if !searching && hidden > 0 {
                button {
                    class: "reset",
                    style: "margin-top:8px;text-transform:none;font-family:inherit;",
                    r#type: "button",
                    onclick: move |_| {
                        let cur = *expanded.peek();
                        expanded.set(!cur);
                    },
                    if *expanded.read() {
                        {i18n.t("discover.showFewerTags")}
                    } else {
                        {i18n.args("discover.showAllTags", &[("count", &hidden.to_string())])}
                    }
                }
            }
        }
        div { class: "ik-legend",
            span { class: "inc", {i18n.t("discover.legend.include")} }
            span { class: "exc", {i18n.t("discover.legend.exclude")} }
        }
    }
}

/// A 3-state tag chip: neutral → include (+) → exclude (−) → neutral.
#[component]
pub(super) fn TagChip(tag: TagFacet, state: TagState, on_cycle: EventHandler<String>) -> Element {
    let (class, prefix) = match state {
        TagState::Include => ("ik-tagchip inc", "+ "),
        TagState::Exclude => ("ik-tagchip exc", "− "),
        TagState::Neutral => ("ik-tagchip", ""),
    };
    let slug = tag.slug.clone();
    rsx! {
        button {
            class: "{class}",
            r#type: "button",
            onclick: move |_| on_cycle.call(slug.clone()),
            "{prefix}{tag.name}"
            // How much the chip would narrow the grid, before it is clicked. A facet whose
            // options are all unlabelled is a facet you have to click to learn anything from.
            span { class: "ik-tagchip-n", {thousands(tag.series_count)} }
        }
    }
}
