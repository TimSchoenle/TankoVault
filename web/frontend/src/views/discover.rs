//! Discover (DESIGN_SPEC §7.2) — a two-pane screen: a collapsible **filter panel**
//! (content-type / status / tags / providers / release-year / min-chapters / presets) and a
//! **results** pane with a sort select, removable active-filter chips, a count line, the
//! cover-card grid, and pagination.
//!
//! **Data.** Every control now filters/sorts/paginates **server-side** via
//! `GET /v1/series` (§9.1): content-type, status, provider slug, include/exclude tags,
//! release-year range, minimum chapters, sort, and offset pagination. The match total +
//! next page ride on the `X-Total-Count` / `X-Next-Cursor` headers (surfaced as
//! [`SeriesPage`](crate::models::SeriesPage)). The provider facet is populated from the
//! public `GET /v1/providers` list (§9.3).
//!
//! Search (§7.6) shares the cover grid with a larger query echo and a result count.

use crate::api;
use crate::components::{CoverCard, EmptyBox, ErrorBox, SkeletonGrid};
use crate::icons::{Ic, Icon};
use crate::models::{ContentType, PublicProvider, SeriesFilter, SeriesStatus, Tag};
use dioxus::prelude::*;

/// How many series a page of the grid shows.
const PAGE_SIZE: usize = 24;
/// Lowest / highest release year the panel's slider exposes; sending a bound only when the
/// user narrows past these avoids the server dropping series with an unknown year.
const YEAR_MIN: i32 = 1970;
const YEAR_MAX: i32 = 2026;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Sort {
    Updated,
    Title,
    Chapters,
    Sources,
    Rating,
    Year,
}

impl Sort {
    const ALL: [Sort; 6] = [
        Self::Updated,
        Self::Title,
        Self::Chapters,
        Self::Sources,
        Self::Rating,
        Self::Year,
    ];
    fn label(self) -> &'static str {
        match self {
            Self::Updated => "Recently updated",
            Self::Title => "Title (A–Z)",
            Self::Chapters => "Most chapters",
            Self::Sources => "Most sources",
            Self::Rating => "Highest rated",
            Self::Year => "Newest",
        }
    }
    fn value(self) -> &'static str {
        match self {
            Self::Updated => "updated",
            Self::Title => "title",
            Self::Chapters => "chapters",
            Self::Sources => "sources",
            Self::Rating => "rating",
            Self::Year => "year",
        }
    }
    fn parse(v: &str) -> Sort {
        match v {
            "title" => Self::Title,
            "chapters" => Self::Chapters,
            "sources" => Self::Sources,
            "rating" => Self::Rating,
            "year" => Self::Year,
            _ => Self::Updated,
        }
    }
}

/// Discover screen.
#[component]
pub fn Discover() -> Element {
    // Filter state (all applied server-side; see module docs).
    let types = use_signal(Vec::<ContentType>::new);
    let statuses = use_signal(Vec::<SeriesStatus>::new);
    let inc = use_signal(Vec::<String>::new);
    let exc = use_signal(Vec::<String>::new);
    let match_all = use_signal(|| false);
    let year_min = use_signal(|| YEAR_MIN);
    let year_max = use_signal(|| YEAR_MAX);
    let min_ch = use_signal(|| 0i32);
    let provider = use_signal(|| Option::<String>::None);
    let mut sort = use_signal(|| Sort::Updated);
    let mut page = use_signal(|| 0usize);
    let mut panel_open = use_signal(|| true);
    let mut reload = use_signal(|| 0u32);

    let tags_res = use_resource(move || async move { api::tags().await.unwrap_or_default() });
    let all_tags: Vec<Tag> = tags_res.read_unchecked().clone().unwrap_or_default();
    let providers_res =
        use_resource(move || async move { api::public_providers().await.unwrap_or_default() });
    let all_providers: Vec<PublicProvider> =
        providers_res.read_unchecked().clone().unwrap_or_default();

    // Build the server-side filter from the current control state, then fetch one page.
    let resource = use_resource(move || {
        let _ = reload.read();
        let ymin = *year_min.read();
        let ymax = *year_max.read();
        let mc = *min_ch.read();
        let filter = SeriesFilter {
            query: None,
            // The server filter is single-valued for type/status; send the first selection.
            content_type: types.read().first().copied(),
            status: statuses.read().first().copied(),
            provider: provider.read().clone(),
            tags: inc.read().clone(),
            exclude_tags: exc.read().clone(),
            year_min: (ymin > YEAR_MIN).then_some(ymin),
            year_max: (ymax < YEAR_MAX).then_some(ymax),
            min_chapters: (mc > 0).then_some(mc),
            sort: Some(sort.read().value().to_string()),
            page: *page.read() as i64,
            limit: PAGE_SIZE as i64,
        };
        async move { api::list_series_filtered(&filter).await }
    });

    let active_count = types.read().len()
        + statuses.read().len()
        + inc.read().len()
        + exc.read().len()
        + usize::from(provider.read().is_some());

    // ---- results ----
    let content = match &*resource.read_unchecked() {
        None => rsx! { SkeletonGrid { count: 12 } },
        Some(Err(e)) => {
            let msg = e.clone();
            rsx! {
                ErrorBox { message: msg, on_retry: move |()| reload += 1 }
            }
        }
        Some(Ok(pagedata)) => {
            let total = usize::try_from(pagedata.total).unwrap_or(0);
            let items = pagedata.items.clone();
            let has_next = pagedata.next_cursor.is_some();
            let pages = total.div_ceil(PAGE_SIZE).max(1);
            let cur = *page.read();

            if items.is_empty() {
                rsx! {
                    div { class: "ik-empty",
                        Ic { icon: Icon::Search, size: 28 }
                        p { style: "margin:10px 0 4px;font-weight:600;", "Nothing matched those filters" }
                        p { class: "ik-muted", style: "font-size:13px;", "Try widening the type or status, or reset everything." }
                        button { class: "ik-btn", style: "margin-top:10px;", onclick: move |_| clear_all(types, statuses, inc, exc, provider, page), "Reset filters" }
                    }
                }
            } else {
                rsx! {
                    div { class: "ik-count-line",
                        span { class: "ik-mono", "{total}" }
                        " series · page "
                        span { class: "ik-mono", "{cur + 1}" }
                        " of "
                        span { class: "ik-mono", "{pages}" }
                    }
                    div { class: "ik-grid",
                        for s in items {
                            CoverCard { key: "{s.id}", series: s }
                        }
                    }
                    Pagination { page, pages, has_next }
                }
            }
        }
    };

    let discover_class = if *panel_open.read() {
        "ik-discover"
    } else {
        "ik-discover collapsed"
    };
    let sort_val = sort.read().value().to_string();

    rsx! {
        div { class: "ik-results-head",
            button {
                class: "ik-panel-toggle",
                title: "Toggle filters",
                onclick: move |_| {
                    let cur = *panel_open.peek();
                    panel_open.set(!cur);
                },
                Ic { icon: Icon::Tune, size: 18 }
            }
            h1 { class: "ik-page-title", "Discover" }
            span { class: "ik-rail-spacer", style: "flex:1;" }
            label { class: "ik-muted", style: "font-size:13px;", "Sort" }
            select {
                class: "ik-select",
                value: "{sort_val}",
                onchange: move |e| {
                    sort.set(Sort::parse(&e.value()));
                    page.set(0);
                },
                for s in Sort::ALL {
                    option { value: "{s.value()}", selected: *sort.read() == s, "{s.label()}" }
                }
            }
        }

        ActiveFilters { types, statuses, inc, exc, provider, tags: all_tags.clone(), providers: all_providers.clone(), page }

        div { class: "{discover_class}",
            if *panel_open.read() {
                FilterPanel {
                    types, statuses, inc, exc, match_all,
                    year_min, year_max, min_ch,
                    provider,
                    tags: all_tags.clone(),
                    providers: all_providers.clone(),
                    active_count,
                    page,
                    on_reset: move |()| clear_all(types, statuses, inc, exc, provider, page),
                }
            }
            div { {content} }
        }
    }
}

/// Clear every active filter and jump back to page 1.
fn clear_all(
    mut types: Signal<Vec<ContentType>>,
    mut statuses: Signal<Vec<SeriesStatus>>,
    mut inc: Signal<Vec<String>>,
    mut exc: Signal<Vec<String>>,
    mut provider: Signal<Option<String>>,
    mut page: Signal<usize>,
) {
    types.write().clear();
    statuses.write().clear();
    inc.write().clear();
    exc.write().clear();
    provider.set(None);
    page.set(0);
}

// ---------------------------------------------------------------------------
// Filter panel
// ---------------------------------------------------------------------------

#[component]
#[allow(clippy::too_many_arguments)]
fn FilterPanel(
    types: Signal<Vec<ContentType>>,
    statuses: Signal<Vec<SeriesStatus>>,
    inc: Signal<Vec<String>>,
    exc: Signal<Vec<String>>,
    match_all: Signal<bool>,
    year_min: Signal<i32>,
    year_max: Signal<i32>,
    min_ch: Signal<i32>,
    provider: Signal<Option<String>>,
    tags: Vec<Tag>,
    providers: Vec<PublicProvider>,
    active_count: usize,
    page: Signal<usize>,
    on_reset: EventHandler<()>,
) -> Element {
    let ma = *match_all.read();
    let cur_provider = provider.read().clone().unwrap_or_default();
    rsx! {
        aside { class: "ik-filter-panel",
            div { class: "ik-filter-head",
                strong { Ic { icon: Icon::Tune, size: 16 } "Filters" }
                button { class: "reset", onclick: move |_| on_reset.call(()), "Reset" }
            }
            div { class: "ik-muted", style: "font-size:12px;",
                if active_count == 0 { "No filters applied" } else { "{active_count} active" }
            }

            // CONTENT TYPE
            div { class: "ik-filter-group",
                div { class: "lbl", "Content type" }
                div { class: "ik-chips", style: "margin-bottom:0;",
                    for t in ContentType::ALL {
                        TypeChip { t, types, page }
                    }
                }
            }

            // STATUS
            div { class: "ik-filter-group",
                div { class: "lbl", "Status" }
                div { class: "ik-chips", style: "margin-bottom:0;",
                    for s in SeriesStatus::ALL {
                        StatusChip { s, statuses, page }
                    }
                }
            }

            // GENRES / TAGS
            div { class: "ik-filter-group",
                div { class: "lbl",
                    "Genres / tags"
                    button {
                        class: "reset",
                        style: "font-family:var(--font-mono);text-transform:none;",
                        onclick: move |_| {
                            let cur = *match_all.peek();
                            let mut m = match_all;
                            m.set(!cur);
                        },
                        if ma { "match: all" } else { "match: any" }
                    }
                }
                if tags.is_empty() {
                    div { class: "ik-muted", style: "font-size:12px;", "No tags indexed yet." }
                } else {
                    div { class: "ik-chips", style: "margin-bottom:0;",
                        for tag in tags.iter().take(40).cloned() {
                            TagChip { tag, inc, exc, page }
                        }
                    }
                    div { class: "ik-legend",
                        span { class: "inc", "+ include" }
                        span { class: "exc", "− exclude" }
                    }
                }
            }

            // PROVIDER — public providers list (§9.3), filtered server-side by slug (§9.1).
            div { class: "ik-filter-group",
                div { class: "lbl", "Provider" }
                if providers.is_empty() {
                    div { class: "ik-muted", style: "font-size:12px;", "No providers available yet." }
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
                        option { value: "", selected: cur_provider.is_empty(), "All providers" }
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
                div { class: "lbl", "Release year" }
                div { class: "ik-range-row",
                    span { "{year_min}" }
                    input {
                        class: "ik-range",
                        r#type: "range",
                        min: "1970",
                        max: "2026",
                        value: "{year_min}",
                        oninput: move |e| {
                            if let Ok(v) = e.value().parse::<i32>() { year_min.set(v); }
                        },
                    }
                    span { "{year_max}" }
                }
                input {
                    class: "ik-range",
                    r#type: "range",
                    min: "1970",
                    max: "2026",
                    value: "{year_max}",
                    oninput: move |e| {
                        if let Ok(v) = e.value().parse::<i32>() { year_max.set(v); }
                    },
                }
            }

            // MIN. CHAPTERS — server-side (§9.1).
            div { class: "ik-filter-group",
                div { class: "lbl",
                    "Min. chapters"
                    span { class: "ik-mono", style: "color:var(--muted);", "{min_ch}+" }
                }
                input {
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
                div { class: "lbl", "Saved presets" }
                div { class: "ik-muted", style: "font-size:12px;", "Saving filter presets is coming soon." }
            }
        }
    }
}

#[component]
fn TypeChip(t: ContentType, types: Signal<Vec<ContentType>>, page: Signal<usize>) -> Element {
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
            "{t.label()}"
        }
    }
}

#[component]
fn StatusChip(
    s: SeriesStatus,
    statuses: Signal<Vec<SeriesStatus>>,
    page: Signal<usize>,
) -> Element {
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
            "{s.label()}"
        }
    }
}

/// A 3-state tag chip: neutral → include (+) → exclude (−) → neutral.
#[component]
fn TagChip(
    tag: Tag,
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
                    // include -> exclude
                    let pos = inc.read().iter().position(|x| x == &slug);
                    if let Some(i) = pos { inc.write().remove(i); }
                    exc.write().push(slug.clone());
                } else if is_exc {
                    // exclude -> neutral
                    let pos = exc.read().iter().position(|x| x == &slug);
                    if let Some(i) = pos { exc.write().remove(i); }
                } else {
                    // neutral -> include
                    inc.write().push(slug.clone());
                }
                page.set(0);
            },
            "{prefix}{tag.name}"
        }
    }
}

// ---------------------------------------------------------------------------
// Active filters + pagination
// ---------------------------------------------------------------------------

#[component]
fn ActiveFilters(
    types: Signal<Vec<ContentType>>,
    statuses: Signal<Vec<SeriesStatus>>,
    inc: Signal<Vec<String>>,
    exc: Signal<Vec<String>>,
    provider: Signal<Option<String>>,
    tags: Vec<Tag>,
    providers: Vec<PublicProvider>,
    page: Signal<usize>,
) -> Element {
    let ty = types.read().clone();
    let st = statuses.read().clone();
    let inc_v = inc.read().clone();
    let exc_v = exc.read().clone();
    let prov = provider.read().clone();
    if ty.is_empty() && st.is_empty() && inc_v.is_empty() && exc_v.is_empty() && prov.is_none() {
        return rsx! {};
    }
    let name_of = |slug: &str| {
        tags.iter()
            .find(|t| t.slug == slug)
            .map(|t| t.name.clone())
            .unwrap_or_else(|| slug.to_owned())
    };
    let provider_label = prov.as_ref().map(|slug| {
        providers
            .iter()
            .find(|p| &p.slug == slug)
            .map(|p| p.name.clone())
            .unwrap_or_else(|| slug.clone())
    });
    rsx! {
        div { class: "ik-active-filters",
            if let Some(label) = provider_label {
                div { class: "ik-afchip",
                    "{label}"
                    button {
                        onclick: move |_| {
                            let mut v = provider;
                            v.set(None);
                            page.set(0);
                        },
                        Ic { icon: Icon::Close, size: 12 }
                    }
                }
            }
            for t in ty {
                div { class: "ik-afchip",
                    "{t.label()}"
                    button {
                        onclick: move |_| {
                            let mut v = types;
                            let pos = v.read().iter().position(|x| *x == t);
                            if let Some(i) = pos { v.write().remove(i); }
                            page.set(0);
                        },
                        Ic { icon: Icon::Close, size: 12 }
                    }
                }
            }
            for s in st {
                div { class: "ik-afchip",
                    "{s.label()}"
                    button {
                        onclick: move |_| {
                            let mut v = statuses;
                            let pos = v.read().iter().position(|x| *x == s);
                            if let Some(i) = pos { v.write().remove(i); }
                            page.set(0);
                        },
                        Ic { icon: Icon::Close, size: 12 }
                    }
                }
            }
            for slug in inc_v {
                {
                    let label = name_of(&slug);
                    rsx! {
                        div { class: "ik-afchip",
                            "+ {label}"
                            button {
                                onclick: move |_| {
                                    let mut v = inc;
                                    let pos = v.read().iter().position(|x| x == &slug);
                                    if let Some(i) = pos { v.write().remove(i); }
                                    page.set(0);
                                },
                                Ic { icon: Icon::Close, size: 12 }
                            }
                        }
                    }
                }
            }
            for slug in exc_v {
                {
                    let label = name_of(&slug);
                    rsx! {
                        div { class: "ik-afchip",
                            "− {label}"
                            button {
                                onclick: move |_| {
                                    let mut v = exc;
                                    let pos = v.read().iter().position(|x| x == &slug);
                                    if let Some(i) = pos { v.write().remove(i); }
                                    page.set(0);
                                },
                                Ic { icon: Icon::Close, size: 12 }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Collapsed sequence of page indices to render around `cur` (0-based). Always keeps the
/// first and last page reachable and fills single-page gaps directly instead of spending an
/// ellipsis on them, so long result sets don't spam a button per page (`None` = ellipsis).
fn page_window(cur: usize, pages: usize) -> Vec<Option<usize>> {
    if pages == 0 {
        return Vec::new();
    }
    if pages <= 7 {
        return (0..pages).map(Some).collect();
    }
    let last = pages - 1;
    let mut keep = vec![0, last, cur];
    if cur > 0 {
        keep.push(cur - 1);
    }
    if cur < last {
        keep.push(cur + 1);
    }
    keep.sort_unstable();
    keep.dedup();

    let mut out = Vec::with_capacity(keep.len() + 2);
    let mut prev: Option<usize> = None;
    for p in keep {
        match prev {
            Some(pv) if p == pv + 2 => out.push(Some(pv + 1)),
            Some(pv) if p > pv + 1 => out.push(None),
            _ => {}
        }
        out.push(Some(p));
        prev = Some(p);
    }
    out
}

/// Jump-box handler: parses the typed page number (1-based) and moves there, clamped to range.
fn jump_to_page(mut jump: Signal<String>, mut page: Signal<usize>, pages: usize) {
    if let Ok(n) = jump.read().trim().parse::<usize>() {
        if n >= 1 {
            page.set((n - 1).min(pages.saturating_sub(1)));
        }
    }
    jump.set(String::new());
}

#[component]
fn Pagination(page: Signal<usize>, pages: usize, has_next: bool) -> Element {
    let cur = *page.read();
    let mut jump = use_signal(String::new);

    rsx! {
        div { class: "ik-pagination",
            button {
                class: "page",
                disabled: cur == 0,
                onclick: move |_| { if cur > 0 { page.set(cur - 1); } },
                "Prev"
            }
            for p in page_window(cur, pages) {
                match p {
                    Some(idx) => rsx! {
                        button {
                            class: if idx == cur { "page active" } else { "page" },
                            onclick: move |_| page.set(idx),
                            "{idx + 1}"
                        }
                    },
                    None => rsx! { span { class: "ellipsis", "…" } },
                }
            }
            button {
                class: "page",
                disabled: !has_next && cur + 1 >= pages,
                onclick: move |_| page.set(cur + 1),
                "Next"
            }
            if pages > 1 {
                div { class: "ik-page-jump",
                    "Go to"
                    input {
                        r#type: "number",
                        min: "1",
                        max: "{pages}",
                        value: "{jump.read()}",
                        placeholder: "{cur + 1}",
                        oninput: move |e| jump.set(e.value()),
                        onkeydown: move |e| {
                            if e.key() == Key::Enter {
                                jump_to_page(jump, page, pages);
                            }
                        },
                    }
                    "of {pages}"
                    button {
                        class: "page",
                        r#type: "button",
                        onclick: move |_| jump_to_page(jump, page, pages),
                        "Go"
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Search
// ---------------------------------------------------------------------------

/// Search screen — trigram-backed query passed straight to the API (§7.6).
#[component]
pub fn Search(q: String) -> Element {
    // `q` is a plain prop, not a signal, so re-running a search from the search page itself
    // (same route, new `?q=`) reuses this mounted component and only changes `q` — that
    // alone doesn't restart `use_resource`, which only reacts to signals read inside it.
    // Mirror the prop into a signal so the fetch actually restarts when it changes.
    let mut q_state = use_signal(|| q.clone());
    if *q_state.peek() != q {
        q_state.set(q.clone());
    }

    let mut reload = use_signal(|| 0u32);
    let resource = use_resource(move || {
        let q = q_state.read().clone();
        let _ = reload.read();
        async move { api::list_series(Some(&q), 60).await }
    });

    let (count, body) = match &*resource.read_unchecked() {
        None => (0usize, rsx! { SkeletonGrid { count: 8 } }),
        Some(Err(e)) => {
            let msg = e.clone();
            (
                0usize,
                rsx! {
                    ErrorBox { message: msg, on_retry: move |()| reload += 1 }
                },
            )
        }
        Some(Ok(items)) if items.is_empty() => (
            0usize,
            rsx! {
                EmptyBox { message: "No series matched that. Try fewer words.".to_string() }
            },
        ),
        Some(Ok(items)) => {
            let items = items.clone();
            let n = items.len();
            (
                n,
                rsx! {
                    div { class: "ik-grid",
                        for s in items {
                            CoverCard { key: "{s.id}", series: s }
                        }
                    }
                },
            )
        }
    };

    rsx! {
        h1 { class: "ik-page-title", style: "font-size:34px;", "Results for “{q}”" }
        div { class: "ik-count-line",
            span { class: "ik-mono", "{count}" }
            " results · trigram fuzzy match"
        }
        {body}
    }
}
