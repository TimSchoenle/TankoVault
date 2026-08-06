//! Discover's collapsible filter panel and the three chip kinds it is built from.
//!
//! Split out of `views/discover/mod.rs`: `FilterPanel` alone takes fourteen props, and together
//! with the chips it is a screen's worth of markup sitting inside another screen's module.

use super::{YEAR_MAX, YEAR_MIN};
use crate::i18n::use_i18n;
use crate::icons::{Ic, Icon};
use crate::models::*;
use crate::util::thousands;
use dioxus::prelude::*;

// `#[component]` turns these props into a struct, so `too_many_arguments` never fires here.
#[component]
pub(super) fn FilterPanel(
    types: Signal<Vec<ContentType>>,
    statuses: Signal<Vec<SeriesStatus>>,
    inc: Signal<Vec<String>>,
    exc: Signal<Vec<String>>,
    match_all: Signal<bool>,
    year_min: Signal<i32>,
    year_max: Signal<i32>,
    min_ch: Signal<i32>,
    provider: Signal<Option<String>>,
    tags: Vec<TagFacet>,
    providers: Vec<PublicProvider>,
    active_count: usize,
    page: Signal<usize>,
    on_reset: EventHandler<()>,
) -> Element {
    let i18n = use_i18n();
    let ma = *match_all.read();
    let cur_provider = provider.read().clone().unwrap_or_default();
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
                        TypeChip { t, types, page }
                    }
                }
            }

            // STATUS
            div { class: "ik-filter-group",
                div { class: "lbl", {i18n.t("discover.status")} }
                div { class: "ik-chips", style: "margin-bottom:0;",
                    for s in SeriesStatus::all().iter().copied() {
                        StatusChip { s, statuses, page }
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
                            let mut m = match_all;
                            m.set(!cur);
                        },
                        if ma {
                            {i18n.t("discover.matchAll")}
                        } else {
                            {i18n.t("discover.matchAny")}
                        }
                    }
                }
                if tags.is_empty() {
                    div { class: "ik-muted", style: "font-size:12px;", {i18n.t("discover.noTags")} }
                } else {
                    TagFacetPanel { tags: tags.clone(), inc, exc, page }
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
                        value: "{cur_provider}",
                        onchange: move |e| {
                            let v = e.value();
                            let mut provider = provider;
                            provider.set(if v.is_empty() { None } else { Some(v) });
                            page.set(0);
                        },
                        option { value: "", selected: cur_provider.is_empty(), {i18n.t("discover.allProviders")} }
                        for p in providers.iter().cloned() {
                            option {
                                key: "{p.id}",
                                value: "{p.slug}",
                                selected: cur_provider == p.slug,
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
                            if let Ok(v) = e.value().parse::<i32>() { year_min.set(v); }
                        },
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
                        if let Ok(v) = e.value().parse::<i32>() { year_max.set(v); }
                    },
                }
            }

            // MIN. CHAPTERS — server-side (§9.1).
            div { class: "ik-filter-group",
                label { class: "lbl", r#for: "tv-min-chapters",
                    {i18n.t("discover.minChapters")}
                    span { class: "ik-mono", style: "color:var(--muted);", "{min_ch}+" }
                }
                input {
                    id: "tv-min-chapters",
                    class: "ik-range",
                    r#type: "range",
                    min: "0",
                    max: "500",
                    step: "10",
                    value: "{min_ch}",
                    oninput: move |e| {
                        if let Ok(v) = e.value().parse::<i32>() { min_ch.set(v); }
                    },
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
    types: Signal<Vec<ContentType>>,
    page: Signal<usize>,
) -> Element {
    let i18n = use_i18n();
    let active = types.read().contains(&t);
    let class = if active { "ik-chip active" } else { "ik-chip" };
    rsx! {
        button {
            class: "{class}",
            r#type: "button",
            onclick: move |_| {
                let mut v = types;
                let pos = v.read().iter().position(|x| *x == t);
                if let Some(i) = pos { v.write().remove(i); } else { v.write().push(t); }
                page.set(0);
            },
            {i18n.t(t.label_key())}
        }
    }
}

#[component]
pub(super) fn StatusChip(
    s: SeriesStatus,
    statuses: Signal<Vec<SeriesStatus>>,
    page: Signal<usize>,
) -> Element {
    let i18n = use_i18n();
    let active = statuses.read().contains(&s);
    let class = if active { "ik-chip active" } else { "ik-chip" };
    rsx! {
        button {
            class: "{class}",
            r#type: "button",
            onclick: move |_| {
                let mut v = statuses;
                let pos = v.read().iter().position(|x| *x == s);
                if let Some(i) = pos { v.write().remove(i); } else { v.write().push(s); }
                page.set(0);
            },
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
    inc: Signal<Vec<String>>,
    exc: Signal<Vec<String>>,
    page: Signal<usize>,
) -> Element {
    let i18n = use_i18n();
    let mut query = use_signal(String::new);
    let mut expanded = use_signal(|| false);

    let needle = query.read().trim().to_lowercase();
    let matching: Vec<TagFacet> = if needle.is_empty() {
        tags.clone()
    } else {
        tags.iter()
            .filter(|tag| {
                tag.name.to_lowercase().contains(&needle) || tag.slug.contains(&needle)
            })
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
                    TagChip { key: "{tag.slug}", tag, inc, exc, page }
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
pub(super) fn TagChip(
    tag: TagFacet,
    inc: Signal<Vec<String>>,
    exc: Signal<Vec<String>>,
    page: Signal<usize>,
) -> Element {
    let slug = tag.slug.clone();
    let is_inc = inc.read().contains(&slug);
    let is_exc = exc.read().contains(&slug);
    let class = if is_inc {
        "ik-tagchip inc"
    } else if is_exc {
        "ik-tagchip exc"
    } else {
        "ik-tagchip"
    };
    let prefix = if is_inc {
        "+ "
    } else if is_exc {
        "− "
    } else {
        ""
    };
    rsx! {
        button {
            class: "{class}",
            r#type: "button",
            onclick: move |_| {
                let mut inc = inc;
                let mut exc = exc;
                if is_inc {
                    let pos = inc.read().iter().position(|x| x == &slug);
                    if let Some(i) = pos { inc.write().remove(i); }
                    exc.write().push(slug.clone());
                } else if is_exc {
                    let pos = exc.read().iter().position(|x| x == &slug);
                    if let Some(i) = pos { exc.write().remove(i); }
                } else {
                    inc.write().push(slug.clone());
                }
                page.set(0);
            },
            "{prefix}{tag.name}"
            // How much the chip would narrow the grid, before it is clicked. A facet whose
            // options are all unlabelled is a facet you have to click to learn anything from.
            span { class: "ik-tagchip-n", {thousands(tag.series_count)} }
        }
    }
}
