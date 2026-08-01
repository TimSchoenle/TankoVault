//! Discover (`DESIGN_SPEC` §7.2): filter panel plus results grid, filtered/sorted/paginated
//! server-side via `GET /v1/series` (§9.1); the panel and chip bar live in [`filters`] and [`active`].

mod active;
mod filters;

use crate::api;
use crate::components::{async_view, CoverCard, Pagination, SkeletonGrid};
use crate::hooks::use_reload;
use crate::i18n::use_i18n;
use crate::icons::{Ic, Icon};
use crate::models::*;
use active::ActiveFilters;
use dioxus::prelude::*;
use filters::FilterPanel;
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

    // Degrades to an empty facet on failure rather than blocking the screen — a missing
    // tag/provider filter is cheaper than an error state.
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
                // One interpolated sentence, not span-wrapped fragments — splitting around
                // markup fixes word order to English, unorderable in other languages.
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
