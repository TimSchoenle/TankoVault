//! Discover (`DESIGN_SPEC` §7.2) — a two-pane screen: a collapsible **filter panel**
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
use crate::components::{async_list, async_view, CoverCard, Pagination, SkeletonGrid};
use crate::hooks::use_reload;
use crate::i18n::use_i18n;
use crate::icons::{Ic, Icon};
use crate::models::*;
use dioxus::prelude::*;
use progenitor_client::ResponseValue;

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
    /// The catalogue key of this option's display name (see [`crate::i18n`]).
    fn label_key(self) -> &'static str {
        match self {
            Self::Updated => "discover.sort.updated",
            Self::Title => "discover.sort.title",
            Self::Chapters => "discover.sort.chapters",
            Self::Sources => "discover.sort.sources",
            Self::Rating => "discover.sort.rating",
            Self::Year => "discover.sort.year",
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
pub(crate) fn Discover() -> Element {
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
    let reload = use_reload();
    let i18n = use_i18n();
    let api = api::use_api();

    // Facet data. A failure degrades to an empty facet rather than an error state: the grid
    // is still usable without the tag or provider filter, and blocking the whole screen on a
    // secondary list would be worse than offering fewer controls.
    let tags_res = use_resource(move || {
        let client = api.client();
        async move {
            client
                .tags()
                .send()
                .await
                .map(ResponseValue::into_inner)
                .unwrap_or_default()
        }
    });
    let all_tags: Vec<Tag> = tags_res.read_unchecked().clone().unwrap_or_default();

    let providers_res = use_resource(move || {
        let client = api.client();
        async move {
            client
                .providers()
                .send()
                .await
                .map(ResponseValue::into_inner)
                .unwrap_or_default()
        }
    });
    let all_providers: Vec<PublicProvider> =
        providers_res.read_unchecked().clone().unwrap_or_default();

    // Build the server-side filter from the current control state, then fetch one page.
    let resource = {
        use_resource(move || {
            reload.track();
            let ymin = *year_min.read();
            let ymax = *year_max.read();
            let mc = *min_ch.read();
            let content_type = types.read().first().map(|ct| ct.token().to_owned());
            let status = statuses.read().first().map(|st| st.token().to_owned());
            let provider = provider.read().clone();
            let tags = if inc.read().is_empty() {
                None
            } else {
                Some(inc.read().clone())
            };
            let exclude_tag = if exc.read().is_empty() {
                None
            } else {
                Some(exc.read().clone())
            };
            let sort = sort.read().value().to_owned();
            let page = i64::try_from(*page.read()).unwrap_or(0);
            let client = api.client();

            async move {
                let mut builder = client.list();
                if let Some(ct) = content_type {
                    builder = builder.content_type(ct);
                }
                if let Some(st) = status {
                    builder = builder.status(st);
                }
                if let Some(p) = provider {
                    builder = builder.provider(p);
                }
                if let Some(tags) = tags {
                    builder = builder.tag(tags);
                }
                if let Some(exclude_tag) = exclude_tag {
                    builder = builder.exclude_tag(exclude_tag);
                }
                if ymin > YEAR_MIN {
                    builder = builder.year_min(ymin);
                }
                if ymax < YEAR_MAX {
                    builder = builder.year_max(ymax);
                }
                if mc > 0 {
                    builder = builder.min_chapters(mc);
                }
                builder = builder.sort(sort);
                builder = builder.page(page);
                builder = builder.limit(i64::try_from(PAGE_SIZE).unwrap_or(24));

                builder
                    .send()
                    .await
                    .map(|r| {
                        let total = r
                            .headers()
                            .get("x-total-count")
                            .and_then(|v| v.to_str().ok())
                            .and_then(|s| s.parse::<i64>().ok())
                            .unwrap_or_else(|| i64::try_from(r.as_ref().len()).unwrap_or(0));
                        let next_cursor = r
                            .headers()
                            .get("x-next-cursor")
                            .and_then(|v| v.to_str().ok())
                            .and_then(|s| s.parse::<i64>().ok());
                        SeriesPage {
                            items: r.into_inner(),
                            total,
                            next_cursor,
                        }
                    })
                    .map_err(|e| api::friendly_error(i18n, e))
            }
        })
    };

    let active_count = types.read().len()
        + statuses.read().len()
        + inc.read().len()
        + exc.read().len()
        + usize::from(provider.read().is_some());

    // ---- results ----
    let content = async_view(
        &resource,
        reload,
        || rsx! { SkeletonGrid { count: 12 } },
        |page_data| {
            let total = usize::try_from(page_data.total).unwrap_or(0);
            let has_next = page_data.next_cursor.is_some();
            let pages = total.div_ceil(PAGE_SIZE).max(1);
            let current = *page.read();

            if page_data.items.is_empty() {
                // Filtered-to-nothing gets its own state rather than the generic empty box:
                // the reader's only useful next move is to widen, so offer exactly that.
                return rsx! {
                    div { class: "ik-empty",
                        Ic { icon: Icon::Search, size: 28 }
                        p { style: "margin:10px 0 4px;font-weight:600;", {i18n.t("discover.noMatch.title")} }
                        p { class: "ik-muted", style: "font-size:13px;", {i18n.t("discover.noMatch.hint")} }
                        button {
                            class: "ik-btn",
                            style: "margin-top:10px;",
                            onclick: move |_| clear_all(types, statuses, inc, exc, provider, page),
                            {i18n.t("discover.resetFilters")}
                        }
                    }
                };
            }
            rsx! {
                // One interpolated sentence rather than span-wrapped fragments: splitting a
                // sentence around markup fixes its word order to English and leaves the
                // translator with unorderable scraps.
                div { class: "ik-count-line",
                    {
                        i18n.args(
                            "discover.countLine",
                            &[
                                ("total", &total.to_string()),
                                ("page", &(current + 1).to_string()),
                                ("pages", &pages.to_string()),
                            ],
                        )
                    }
                }
                div { class: "ik-grid",
                    for series in page_data.items.iter().cloned() {
                        CoverCard { key: "{series.id}", series }
                    }
                }
                Pagination { page, pages, has_next }
            }
        },
    );

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
                title: i18n.t("discover.toggleFilters"),
                onclick: move |_| {
                    let cur = *panel_open.peek();
                    panel_open.set(!cur);
                },
                Ic { icon: Icon::Tune, size: 18 }
            }
            h1 { class: "ik-page-title", {i18n.t("nav.discover")} }
            span { class: "ik-rail-spacer", style: "flex:1;" }
            label { class: "ik-muted", style: "font-size:13px;", {i18n.t("discover.sortLabel")} }
            select {
                class: "ik-select",
                value: "{sort_val}",
                onchange: move |e| {
                    sort.set(Sort::parse(&e.value()));
                    page.set(0);
                },
                for s in Sort::ALL {
                    option { value: "{s.value()}", selected: *sort.read() == s, {i18n.t(s.label_key())} }
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
                    div { class: "ik-chips", style: "margin-bottom:0;",
                        for tag in tags.iter().take(40).cloned() {
                            TagChip { tag, inc, exc, page }
                        }
                    }
                    div { class: "ik-legend",
                        span { class: "inc", {i18n.t("discover.legend.include")} }
                        span { class: "exc", {i18n.t("discover.legend.exclude")} }
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
                    {i18n.t("discover.minChapters")}
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
                div { class: "lbl", {i18n.t("discover.presets")} }
                div { class: "ik-muted", style: "font-size:12px;", {i18n.t("discover.presetsSoon")} }
            }
        }
    }
}

#[component]
fn TypeChip(t: ContentType, types: Signal<Vec<ContentType>>, page: Signal<usize>) -> Element {
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
fn StatusChip(
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
    let i18n = use_i18n();
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
            .map_or_else(|| slug.to_owned(), |t| t.name.clone())
    };
    let provider_label = prov.as_ref().map(|slug| {
        providers
            .iter()
            .find(|p| &p.slug == slug)
            .map_or_else(|| slug.clone(), |p| p.name.clone())
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
                    {i18n.t(t.label_key())}
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
                    {i18n.t(s.label_key())}
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

// ---------------------------------------------------------------------------
// Search
// ---------------------------------------------------------------------------

/// Search screen — trigram-backed query passed straight to the API (§7.6).
#[component]
pub(crate) fn Search(q: String) -> Element {
    // `q` is a plain prop, not a signal, so re-running a search from the search page itself
    // (same route, new `?q=`) reuses this mounted component and only changes `q` — that
    // alone doesn't restart `use_resource`, which only reacts to signals read inside it.
    // Mirror the prop into a signal so the fetch actually restarts when it changes.
    let mut q_state = use_signal(|| q.clone());
    if *q_state.peek() != q {
        q_state.set(q.clone());
    }

    let reload = use_reload();
    let i18n = use_i18n();
    let api = api::use_api();
    let resource = use_resource(move || {
        let q = q_state.read().clone();
        reload.track();
        let client = api.client();
        async move {
            client
                .list()
                .query(q)
                .limit(60)
                .send()
                .await
                .map(ResponseValue::into_inner)
                .map_err(|e| api::friendly_error(i18n, e))
        }
    });

    // The count line reports what actually loaded, so it stays hidden rather than claiming
    // "0 results" while the request is still in flight or after it failed.
    let count = match &*resource.read_unchecked() {
        Some(Ok(items)) => Some(items.len()),
        _ => None,
    };
    let body = async_list(
        &resource,
        reload,
        || rsx! { SkeletonGrid { count: 8 } },
        &i18n.t("search.empty"),
        |items| {
            rsx! {
                div { class: "ik-grid",
                    for series in items.iter().cloned() {
                        CoverCard { key: "{series.id}", series }
                    }
                }
            }
        },
    );

    rsx! {
        h1 { class: "ik-page-title", style: "font-size:34px;",
            {i18n.args("search.title", &[("query", &q)])}
        }
        if let Some(count) = count {
            div { class: "ik-count-line",
                {i18n.args("search.countLine", &[("count", &count.to_string())])}
            }
        }
        {body}
    }
}
