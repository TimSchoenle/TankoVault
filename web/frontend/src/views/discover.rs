//! Discover (§17.2.1) — masonry cover grid with tag/status/type filter chips and a sort
//! control — and Search (§17.2.6) — instant, grouped results.
//!
//! Match-based branches are computed into an `Element` variable and interpolated, which is
//! the pattern the Dioxus docs recommend for conditional rendering.

use crate::api;
use crate::components::{Brush, CoverCard, EmptyBox, ErrorBox, SkeletonGrid};
use crate::models::{ContentType, SeriesStatus, SeriesSummary};
use dioxus::prelude::*;

#[derive(Debug, Clone, Copy, PartialEq)]
enum Sort {
    Updated,
    Alpha,
    Sources,
}

/// Discover screen.
#[component]
pub fn Discover() -> Element {
    let mut ctype = use_signal(|| Option::<ContentType>::None);
    let mut status = use_signal(|| Option::<SeriesStatus>::None);
    let sort = use_signal(|| Sort::Updated);
    let mut reload = use_signal(|| 0u32);

    let resource = use_resource(move || {
        let _ = reload.read();
        async move { api::list_series(None, 80).await }
    });

    let content = match &*resource.read_unchecked() {
        None => rsx! { SkeletonGrid { count: 12 } },
        Some(Err(e)) => {
            let msg = e.clone();
            rsx! {
                ErrorBox { message: msg, on_retry: move |()| reload += 1 }
            }
        }
        Some(Ok(all)) => {
            let mut items: Vec<SeriesSummary> = all
                .iter()
                .filter(|s| ctype.read().is_none_or(|t| s.content_type == t))
                .filter(|s| status.read().is_none_or(|st| s.status == st))
                .cloned()
                .collect();
            match *sort.read() {
                Sort::Updated => {}
                Sort::Alpha => {
                    items.sort_by(|a, b| a.title.to_lowercase().cmp(&b.title.to_lowercase()));
                }
                Sort::Sources => items.sort_by(|a, b| b.source_count.cmp(&a.source_count)),
            }
            if items.is_empty() {
                rsx! {
                    EmptyBox {
                        message: "Nothing here yet — add a provider in the console to start indexing."
                            .to_string(),
                    }
                }
            } else {
                rsx! {
                    div { class: "ik-grid",
                        for s in items {
                            CoverCard { key: "{s.id}", series: s }
                        }
                    }
                }
            }
        }
    };

    rsx! {
        h1 { class: "ik-page-title", "Discover" }
        Brush {}
        div { class: "ik-chips",
            FilterChip {
                label: "All types",
                active: ctype.read().is_none(),
                onclick: move |_| ctype.set(None),
            }
            for t in ContentType::ALL {
                FilterChip {
                    label: t.label().to_string(),
                    active: *ctype.read() == Some(t),
                    onclick: move |_| ctype.set(Some(t)),
                }
            }
        }
        div { class: "ik-chips",
            FilterChip {
                label: "Any status",
                active: status.read().is_none(),
                onclick: move |_| status.set(None),
            }
            for s in SeriesStatus::ALL {
                FilterChip {
                    label: s.label().to_string(),
                    active: *status.read() == Some(s),
                    onclick: move |_| status.set(Some(s)),
                }
            }
            span { class: "ik-rail-spacer" }
            SortControl { sort }
        }
        {content}
    }
}

#[component]
fn FilterChip(label: String, active: bool, onclick: EventHandler<()>) -> Element {
    let class = if active { "ik-chip active" } else { "ik-chip" };
    rsx! {
        button {
            class: "{class}",
            r#type: "button",
            onclick: move |_| onclick.call(()),
            "{label}"
        }
    }
}

#[component]
fn SortControl(sort: Signal<Sort>) -> Element {
    rsx! {
        div { class: "ik-flex",
            span { class: "ik-muted", "Sort" }
            SortChip { this: Sort::Updated, label: "Recently updated", sort }
            SortChip { this: Sort::Alpha, label: "A–Z", sort }
            SortChip { this: Sort::Sources, label: "Most sources", sort }
        }
    }
}

#[component]
fn SortChip(this: Sort, label: String, sort: Signal<Sort>) -> Element {
    let mut sort = sort;
    let class = if *sort.read() == this {
        "ik-chip active"
    } else {
        "ik-chip"
    };
    rsx! {
        button {
            class: "{class}",
            r#type: "button",
            onclick: move |_| sort.set(this),
            "{label}"
        }
    }
}

/// Search screen — trigram-backed query passed straight to the API, grouped as Series
/// (tag grouping is a follow-up once the API exposes tag search).
#[component]
pub fn Search(q: String) -> Element {
    let query = q.clone();
    let mut reload = use_signal(|| 0u32);
    let resource = use_resource(move || {
        let q = query.clone();
        let _ = reload.read();
        async move { api::list_series(Some(&q), 60).await }
    });

    let body = match &*resource.read_unchecked() {
        None => rsx! { SkeletonGrid { count: 8 } },
        Some(Err(e)) => {
            let msg = e.clone();
            rsx! {
                ErrorBox { message: msg, on_retry: move |()| reload += 1 }
            }
        }
        Some(Ok(items)) if items.is_empty() => rsx! {
            EmptyBox { message: "No series matched that. Try fewer words.".to_string() }
        },
        Some(Ok(items)) => {
            let items = items.clone();
            rsx! {
                div { class: "ik-grid",
                    for s in items {
                        CoverCard { key: "{s.id}", series: s }
                    }
                }
            }
        }
    };

    rsx! {
        h1 { class: "ik-page-title", "Results for “{q}”" }
        Brush {}
        h3 { class: "ik-dayhead", "Series" }
        {body}
    }
}
