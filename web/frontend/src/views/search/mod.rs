//! Search — the trigram-backed catalogue query as a screen of its own (§7.6).
//!
//! Two states, not one. Opened from the rail there is no term yet, and the screen used to answer
//! that with `Results for ""` over an empty grid: a heading about a search nobody had made, and
//! no field to make one in — the only way in was the top bar's box, which is not where a reader
//! who just clicked "Search" is looking. The landing state is now a form.
//!
//! Its options are the browse endpoint's own parameters, because this *is* that endpoint with a
//! `query`. Anything Discover can narrow by, this can offer without inventing a vocabulary — and
//! all of it rides in the URL, so a search worth sending to someone carries what it was narrowed
//! to.

mod query;

pub(crate) use query::SearchQuery;

use crate::api;
use crate::components::{
    async_list, unmeasured, use_grid_fill, CoverCard, GridFitProbe, SkeletonGrid,
};
use crate::hooks::use_reload;
use crate::i18n::use_i18n;
use crate::icons::{Ic, Icon};
use crate::models::{ContentType, ContentTypeExt, SeriesStatus, SeriesStatusExt};
use crate::state::use_session;
use crate::views::discover::{Sort, Tracking};
use crate::Route;
use dioxus::prelude::*;
use inkstone_ui::{Button, Size, Tone};
use progenitor_client::ResponseValue;

/// Search screen — trigram-backed query passed straight to the API (§7.6).
#[component]
pub(crate) fn Search(query: SearchQuery) -> Element {
    let i18n = use_i18n();
    let api = api::use_api();
    let session = use_session();
    let nav = navigator();
    let reload = use_reload();

    // `query` is a plain prop; re-running a search from this same page only changes the prop,
    // which alone doesn't restart `use_resource` (it only reacts to signals). Mirror it into a
    // signal so the fetch actually restarts.
    let mut state = use_signal(|| query.clone());
    if *state.peek() != query {
        state.set(query.clone());
    }

    // This screen has no pager, so the result set *is* one page: as many whole rows of covers as
    // the ceiling allows, rather than a fixed 60 that ends in a ragged row at most widths.
    let fit = use_grid_fill();
    let resource = use_resource(move || {
        let search = state.read().clone();
        reload.track();
        let limit = fit.page_size();
        let client = api.client();
        async move {
            // Parked until the grid is measured; see `crate::components::unmeasured`.
            let Some(limit) = limit else {
                return unmeasured().await;
            };
            // No term, no request: the landing state is a form, and a `query=` of nothing would
            // fetch the whole catalogue to render behind it.
            if search.is_empty() {
                return Ok(Vec::new());
            }
            let mut builder = client
                .list()
                .query(search.q.trim().to_owned())
                .limit(i64::try_from(limit).unwrap_or(60));
            if let Some(content_type) = search.content_type {
                builder = builder.content_type(content_type.token());
            }
            if let Some(status) = search.status {
                builder = builder.status(status.token());
            }
            if let Some(tracking) = search.tracking.param() {
                builder = builder.tracking(tracking);
            }
            // Only when the reader chose one. The absent parameter is the server's relevance
            // ranking, and sending this screen's own default would rank an exact title match by
            // whenever it was last scanned.
            if let Some(sort) = search.sort {
                builder = builder.sort(sort.token());
            }
            builder
                .send()
                .await
                .map(ResponseValue::into_inner)
                .map_err(|e| api::friendly_error(i18n, e))
        }
    });

    let go = use_callback(move |next: SearchQuery| {
        nav.push(Route::Search { query: next });
    });

    if query.is_empty() {
        return rsx! {
            SearchLanding { query: query.clone(), on_search: move |next| go.call(next) }
        };
    }

    // The count line reports what actually loaded, so it stays hidden rather than claiming
    // "0 results" while the request is still in flight or after it failed.
    let count = match &*resource.read_unchecked() {
        Some(Ok(items)) => Some(items.len()),
        _ => None,
    };
    let body = async_list(
        &resource,
        reload,
        || rsx! { SkeletonGrid { count: fit.page_size_or_default() } },
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
        div { class: "ik-page-head",
            h1 { class: "ik-page-title", style: "font-size:34px;margin:0;",
                {i18n.args("search.title", &[("query", query.q.trim())])}
            }
        }
        SearchField {
            term: query.q.clone(),
            on_submit: {
                let query = query.clone();
                move |term: String| go.call(query.with_term(term))
            },
        }
        SearchOptions {
            query: query.clone(),
            authenticated: session.is_authenticated(),
            on_change: move |next| go.call(next),
        }
        if let Some(count) = count {
            div { class: "ik-count-line",
                {i18n.args("search.countLine", &[("count", &count.to_string())])}
            }
        }
        // Sits in the same column as the grid and stays mounted through the skeleton, which is
        // what releases the parked fetch.
        GridFitProbe { fit }
        {body}
    }
}

/// The screen before there is anything to show: the field, given the room it needs, and a
/// statement of what it searches.
#[component]
fn SearchLanding(query: SearchQuery, on_search: EventHandler<SearchQuery>) -> Element {
    let i18n = use_i18n();
    rsx! {
        div { class: "ik-search-landing",
            div { class: "ik-search-mark", Ic { icon: Icon::Search, size: 26 } }
            h1 { class: "ik-page-title", style: "margin:14px 0 6px;", {i18n.t("nav.search")} }
            p { class: "ik-muted", style: "margin:0 0 20px;max-width:56ch;",
                {i18n.t("search.landing.hint")}
            }
            SearchField {
                term: query.q.clone(),
                autofocus: true,
                on_submit: move |term: String| on_search.call(query.with_term(term)),
            }
        }
    }
}

/// The term field. Submits on Enter or on the button, never per keystroke: every search is a
/// navigation, and one per character would be a history entry per character too.
#[component]
fn SearchField(
    term: String,
    #[props(default = false)] autofocus: bool,
    on_submit: EventHandler<String>,
) -> Element {
    let i18n = use_i18n();
    let mut draft = use_signal(|| term.clone());
    // Reset when the route's term changes under it — the back button is a term change.
    if *draft.peek() != term && !draft.peek().is_empty() && term.is_empty() {
        draft.set(term.clone());
    }

    let submit = move |()| {
        let text = draft.peek().trim().to_owned();
        if !text.is_empty() {
            on_submit.call(text);
        }
    };

    rsx! {
        form {
            class: "ik-search-form",
            onsubmit: move |event: FormEvent| {
                event.prevent_default();
                submit(());
            },
            span { class: "ik-search-icon", Ic { icon: Icon::Search, size: 16 } }
            input {
                class: "ik-input",
                r#type: "search",
                autofocus,
                placeholder: i18n.t("search.placeholder"),
                "aria-label": i18n.t("nav.search"),
                value: "{draft}",
                oninput: move |event: FormEvent| draft.set(event.value()),
            }
            Button {
                tone: Tone::Primary,
                on_click: move |_| submit(()),
                {i18n.t("nav.search")}
            }
        }
    }
}

/// Type, status, watchlist and ordering — the four the browse endpoint takes that a reader
/// searching a catalogue this size actually reaches for.
#[component]
fn SearchOptions(
    query: SearchQuery,
    authenticated: bool,
    on_change: EventHandler<SearchQuery>,
) -> Element {
    let i18n = use_i18n();
    rsx! {
        div { class: "ik-search-options",
            select {
                class: "ik-input",
                style: "width:auto;",
                "aria-label": i18n.t("discover.sortLabel"),
                value: query.sort.map_or("", Sort::token),
                onchange: {
                    let query = query.clone();
                    move |event: FormEvent| {
                        let chosen = event.value();
                        on_change
                            .call(SearchQuery {
                                sort: (!chosen.is_empty()).then(|| Sort::parse(&chosen)),
                                ..query.clone()
                            });
                    }
                },
                // The absent parameter *is* relevance, so this is not a separate ordering — it is
                // the one the server applies when nobody overrides it.
                option { value: "", {i18n.t("discover.sort.relevance")} }
                for option in Sort::ALL {
                    option { key: "{option.token()}", value: option.token(),
                        {i18n.t(option.label_key())}
                    }
                }
            }
            select {
                class: "ik-input",
                style: "width:auto;",
                "aria-label": i18n.t("discover.contentType"),
                value: query.content_type.map(|value| value.token()).unwrap_or_default(),
                onchange: {
                    let query = query.clone();
                    move |event: FormEvent| {
                        let chosen = event.value();
                        on_change
                            .call(SearchQuery {
                                content_type: <ContentType as ContentTypeExt>::all()
                                    .iter()
                                    .copied()
                                    .find(|t| t.token() == chosen),
                                ..query.clone()
                            });
                    }
                },
                option { value: "", {i18n.t("search.anyType")} }
                for option in <ContentType as ContentTypeExt>::all().iter().copied() {
                    option { key: "{option.token()}", value: option.token(),
                        {i18n.t(option.label_key())}
                    }
                }
            }
            select {
                class: "ik-input",
                style: "width:auto;",
                "aria-label": i18n.t("discover.status"),
                value: query.status.map(|value| value.token()).unwrap_or_default(),
                onchange: {
                    let query = query.clone();
                    move |event: FormEvent| {
                        let chosen = event.value();
                        on_change
                            .call(SearchQuery {
                                status: <SeriesStatus as SeriesStatusExt>::all()
                                    .iter()
                                    .copied()
                                    .find(|s| s.token() == chosen),
                                ..query.clone()
                            });
                    }
                },
                option { value: "", {i18n.t("search.anyStatus")} }
                for option in <SeriesStatus as SeriesStatusExt>::all().iter().copied() {
                    option { key: "{option.token()}", value: option.token(),
                        {i18n.t(option.label_key())}
                    }
                }
            }
            // Resolved server-side against the caller's own token, so it is only offered to a
            // reader who has a watchlist for it to mean anything about.
            if authenticated {
                select {
                    class: "ik-input",
                    style: "width:auto;",
                    "aria-label": i18n.t("discover.tracking.label"),
                    value: query.tracking.token(),
                    onchange: {
                        let query = query.clone();
                        move |event: FormEvent| {
                            on_change
                                .call(SearchQuery {
                                    tracking: Tracking::parse_token(&event.value()),
                                    ..query.clone()
                                });
                        }
                    },
                    for option in Tracking::ALL {
                        option { key: "{option.label_key()}", value: option.token(),
                            {i18n.t(option.label_key())}
                        }
                    }
                }
            }
            if query.has_options() {
                Button {
                    size: Size::Sm,
                    on_click: {
                        let term = query.q.clone();
                        move |_| {
                            on_change
                                .call(SearchQuery {
                                    q: term.clone(),
                                    ..SearchQuery::default()
                                });
                        }
                    },
                    {i18n.t("common.reset")}
                }
            }
        }
    }
}
