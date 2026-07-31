//! Search (`DESIGN_SPEC` §7.6) — a trigram-backed query passed straight to the API.
//!
//! A different route from Discover, which is why it is a different module: it was declared at
//! the foot of `views/discover.rs` and shared nothing with it but `CoverCard`.

use crate::api;
use crate::components::{async_list, CoverCard, SkeletonGrid};
use crate::hooks::use_reload;
use crate::i18n::use_i18n;
use dioxus::prelude::*;
use progenitor_client::ResponseValue;

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
