//! The status tab strip and the filter/sort toolbar.
//!
//! Both write the route rather than a local signal — see [`super::query`] for why the view
//! state lives in the URL.

use super::query::{Released, Sort, View, WatchlistQuery};
use crate::i18n::use_i18n;
use crate::icons::{Ic, Icon};
use crate::models::*;
use dioxus::prelude::*;
use gloo_timers::future::TimeoutFuture;

/// The filter input's DOM id, so `/` can focus it from the list's keyboard handler without
/// either side hard-coding a string the other could rename.
pub(super) const FILTER_INPUT_ID: &str = "wl-filter";

/// How long the filter box waits before committing. Long enough that typing a title does not
/// fire a request per keystroke, short enough that it still feels live.
const DEBOUNCE_MS: u32 = 200;

/// The status tabs, with counts from the server.
///
/// The counts are deliberately *not* `items.len()`: the list is one page of one status, so a
/// count derived from it would read `60` on every tab. They come from a query that applies
/// every filter **except** status, which is what makes "Plan to read · 21" the answer to "what
/// would I see if I clicked this".
#[component]
pub(super) fn StatusTabs(
    query: WatchlistQuery,
    counts: Option<WatchlistCounts>,
    on_change: EventHandler<WatchlistQuery>,
) -> Element {
    let i18n = use_i18n();
    let count_for = |status: Option<WatchStatus>| -> Option<i64> {
        let c = counts.as_ref()?;
        Some(match status {
            None => c.all,
            Some(WatchStatus::Reading) => c.reading,
            Some(WatchStatus::Planned) => c.planned,
            Some(WatchStatus::Paused) => c.paused,
            Some(WatchStatus::Completed) => c.completed,
            Some(WatchStatus::Dropped) => c.dropped,
        })
    };

    // `All` last and set off by a rule, because it is a different kind of choice from the five
    // shelves beside it.
    let tabs: Vec<Option<WatchStatus>> = WatchStatus::all()
        .iter()
        .copied()
        .map(Some)
        .chain(std::iter::once(None))
        .collect();

    rsx! {
        div { class: "ik-wl-tabs", role: "tablist",
            for status in tabs {
                {
                    let active = query.status == status;
                    let label = status.map_or_else(
                        || i18n.t("watchlist.tab.all"),
                        |s| i18n.t(s.label_key()),
                    );
                    let query = query.clone();
                    rsx! {
                        button {
                            key: "{status.map_or(\"all\", |s| s.token())}",
                            class: if status.is_none() {
                                if active { "ik-wl-tab all active" } else { "ik-wl-tab all" }
                            } else if active { "ik-wl-tab active" } else { "ik-wl-tab" },
                            r#type: "button",
                            role: "tab",
                            "aria-selected": if active { "true" } else { "false" },
                            onclick: move |_| on_change.call(WatchlistQuery { status, ..query.clone() }),
                            "{label}"
                            if let Some(count) = count_for(status) {
                                span { class: "ik-wl-tabcount", "{count}" }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// The sticky toolbar: filter text, the unread toggle, the recency window, the sort, and the
/// list/grid switch.
#[component]
pub(super) fn FilterBar(
    query: WatchlistQuery,
    /// Rows currently loaded — only used to word the placeholder, so it says "Filter 564
    /// titles…" rather than a number the reader has to guess the meaning of.
    visible: usize,
    /// How many rows across the whole filtered list have an unhealthy source. The chip only
    /// exists when there is something wrong to point at — a permanent "Source issues (0)" is
    /// chrome that trains the reader to ignore the one time it matters.
    source_issues: i64,
    on_change: EventHandler<WatchlistQuery>,
    /// Used for the debounced filter text: committing it with `push` would put one history
    /// entry per keystroke between the reader and the page they came from.
    on_change_quiet: EventHandler<WatchlistQuery>,
) -> Element {
    let i18n = use_i18n();
    let mut draft = use_signal(|| query.q.clone());
    // Bumped on every keystroke; the timer that wakes up to find it changed knows a newer
    // keystroke superseded it and does nothing. That is the whole debounce — no timer handle to
    // cancel, and no way for two in-flight timers to both commit.
    let mut generation = use_signal(|| 0u32);

    // The route is authoritative: a back-navigation, or the "reset filters" button, has to be
    // reflected in the box. Guarded so it does not clobber what the reader is mid-way through
    // typing.
    use_effect(use_reactive!(|query| {
        if *draft.peek() != query.q {
            draft.set(query.q.clone());
        }
    }));

    let committed = query.clone();
    let on_input = move |event: Event<FormData>| {
        let value = event.value();
        draft.set(value.clone());
        let mine = *generation.peek() + 1;
        generation.set(mine);
        let committed = committed.clone();
        spawn(async move {
            TimeoutFuture::new(DEBOUNCE_MS).await;
            if *generation.peek() != mine {
                return;
            }
            if committed.q != value {
                on_change_quiet.call(WatchlistQuery {
                    q: value,
                    ..committed
                });
            }
        });
    };

    let unread_query = query.clone();
    let issues_query = query.clone();
    let released_query = query.clone();
    let sort_query = query.clone();
    let list_query = query.clone();
    let grid_query = query.clone();

    rsx! {
        div { class: "ik-wl-toolbar",
            div { class: "ik-wl-search",
                span { class: "lead", Ic { icon: Icon::Search, size: 15 } }
                input {
                    id: FILTER_INPUT_ID,
                    class: "ik-input",
                    r#type: "search",
                    value: "{draft}",
                    placeholder: i18n.args("watchlist.filterPlaceholder", &[("count", &visible.to_string())]),
                    "aria-label": i18n.t("watchlist.filterLabel"),
                    oninput: on_input,
                }
                span { class: "kbd", "/" }
            }

            button {
                class: if query.unread_only { "ik-chip active" } else { "ik-chip" },
                r#type: "button",
                "aria-pressed": if query.unread_only { "true" } else { "false" },
                onclick: move |_| {
                    on_change.call(WatchlistQuery {
                        unread_only: !unread_query.unread_only,
                        ..unread_query.clone()
                    });
                },
                {i18n.t("watchlist.unreadOnly")}
            }

            if source_issues > 0 {
                button {
                    class: if query.source_issues { "ik-chip warn active" } else { "ik-chip warn" },
                    r#type: "button",
                    "aria-pressed": if query.source_issues { "true" } else { "false" },
                    onclick: move |_| {
                        on_change.call(WatchlistQuery {
                            source_issues: !issues_query.source_issues,
                            ..issues_query.clone()
                        });
                    },
                    Ic { icon: Icon::Warning, size: 14 }
                    {i18n.args("watchlist.sourceIssues", &[("count", &source_issues.to_string())])}
                }
            }

            label { class: "ik-wl-ctl",
                span { class: "ik-muted", {i18n.t("watchlist.releasedWithin")} }
                select {
                    class: "ik-select",
                    value: "{query.released.token()}",
                    onchange: move |event| {
                        let released = Released::ALL
                            .into_iter()
                            .find(|r| r.token() == event.value())
                            .unwrap_or_default();
                        on_change.call(WatchlistQuery { released, ..released_query.clone() });
                    },
                    for option_released in Released::ALL {
                        option {
                            value: "{option_released.token()}",
                            selected: option_released == query.released,
                            {i18n.t(option_released.label_key())}
                        }
                    }
                }
            }

            label { class: "ik-wl-ctl",
                span { class: "ik-muted", {i18n.t("watchlist.sortBy")} }
                select {
                    class: "ik-select",
                    value: "{query.sort.token()}",
                    onchange: move |event| {
                        let sort = Sort::ALL
                            .into_iter()
                            .find(|s| s.token() == event.value())
                            .unwrap_or_default();
                        // Changing the key drops any pinned direction: the reader picked a new
                        // thing to order by, not a direction to order it in, and carrying
                        // `desc` onto `Title` answers Z→A.
                        on_change.call(WatchlistQuery { sort, order: None, ..sort_query.clone() });
                    },
                    for option_sort in Sort::ALL {
                        option {
                            value: "{option_sort.token()}",
                            selected: option_sort == query.sort,
                            {i18n.t(option_sort.label_key())}
                        }
                    }
                }
            }

            div { class: "ik-wl-viewtoggle", role: "group", "aria-label": i18n.t("watchlist.viewToggle"),
                button {
                    class: if query.view == View::List { "on" } else { "" },
                    r#type: "button",
                    title: i18n.t("watchlist.viewList"),
                    "aria-pressed": if query.view == View::List { "true" } else { "false" },
                    onclick: move |_| {
                        on_change.call(WatchlistQuery { view: View::List, ..list_query.clone() });
                    },
                    Ic { icon: Icon::ViewList, size: 16 }
                }
                button {
                    class: if query.view == View::Grid { "on" } else { "" },
                    r#type: "button",
                    title: i18n.t("watchlist.viewGrid"),
                    "aria-pressed": if query.view == View::Grid { "true" } else { "false" },
                    onclick: move |_| {
                        on_change.call(WatchlistQuery { view: View::Grid, ..grid_query.clone() });
                    },
                    Ic { icon: Icon::ViewGrid, size: 16 }
                }
            }
        }
    }
}
