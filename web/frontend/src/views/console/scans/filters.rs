//! Owns the scan panel's filter bar.
//!
//! Every control writes straight to the URL and reads straight back out of it: no signal
//! shadows a parameter, because a shadowed filter is one that reverts on the back button.
//!
//! Each one also returns to page one. A narrowed list has nothing at page four, so carrying the
//! index over answers a filter change with an empty table.

use super::{ScanFilter, STATE_FILTERS};
use crate::i18n::use_i18n;
use crate::models::{RunSort, RunSortExt, RunStateExt as _, ScanMode, ScanModeExt};
use crate::views::console::query::Window as TimeWindow;
use crate::views::console::{use_console_nav, ConsoleQuery};
use dioxus::prelude::*;
use inkstone_ui::{Button, Chip, FilterSelect, SegOption, Size};
/// State, mode, window, ordering, provider and the show-cleared toggle, and a reset.
#[component]
pub(super) fn FilterBar(filter: ScanFilter) -> Element {
    let i18n = use_i18n();
    let nav = use_console_nav();
    let active = filter.narrows_runs() || filter.cleared;
    let mut state_options: Vec<SegOption> =
        vec![(String::new(), i18n.t("console.scan.filter.anyState"))];
    state_options.extend(
        STATE_FILTERS
            .iter()
            .map(|state| (state.0.to_owned(), i18n.t(state.1.label_key()))),
    );
    let mut mode_options: Vec<SegOption> =
        vec![(String::new(), i18n.t("console.scan.filter.anyMode"))];
    mode_options.extend(
        <ScanMode as ScanModeExt>::all()
            .iter()
            .copied()
            .map(|option| (option.token().to_owned(), i18n.t(option.label_key()))),
    );
    let window_options: Vec<SegOption> = TimeWindow::ALL
        .into_iter()
        .map(|option| (option.token().to_owned(), i18n.t(option.label_key())))
        .collect();
    let sort_options: Vec<SegOption> = <RunSort as RunSortExt>::all()
        .iter()
        .copied()
        .map(|option| (option.token().to_owned(), i18n.t(option.label_key())))
        .collect();

    rsx! {
        div { class: "ik-flex", style: "gap:8px;flex-wrap:wrap;margin-top:10px;",
            FilterSelect {
                id: "tv-scans-state",
                aria_label: i18n.t("console.scan.filter.state"),
                options: state_options,
                selected: filter.status.clone().unwrap_or_default(),
                on_select: move |chosen: String| {
                    nav.filter(ConsoleQuery {
                        status: (!chosen.is_empty()).then_some(chosen),
                        page: 0,
                        ..nav.query()
                    });
                },
            }
            FilterSelect {
                id: "tv-scans-mode",
                aria_label: i18n.t("console.scan.filter.mode"),
                options: mode_options,
                selected: filter.mode.clone().unwrap_or_default(),
                on_select: move |chosen: String| {
                    nav.filter(ConsoleQuery {
                        mode: (!chosen.is_empty()).then_some(chosen),
                        page: 0,
                        ..nav.query()
                    });
                },
            }
            FilterSelect {
                id: "tv-scans-window",
                aria_label: i18n.t("console.audit.filter.window"),
                options: window_options,
                selected: filter.since.token().to_owned(),
                on_select: move |chosen: String| {
                    nav.filter(ConsoleQuery {
                        since: TimeWindow::parse_token(&chosen),
                        page: 0,
                        ..nav.query()
                    });
                },
            }
            FilterSelect {
                id: "tv-scans-sort",
                aria_label: i18n.t("console.scan.filter.sort"),
                options: sort_options,
                selected: filter.ordering().token().to_owned(),
                on_select: move |chosen: String| {
                    let chosen = <RunSort as RunSortExt>::parse(&chosen);
                    nav.filter(ConsoleQuery {
                        // The default ordering is the *absence* of the parameter, so a shared
                        // link names only what its sender actually changed.
                        sort: (chosen != RunSort::Recent).then(|| chosen.token().to_owned()),
                        page: 0,
                        ..nav.query()
                    });
                },
            }
            input {
                class: "ik-input",
                style: "width:auto;flex:1;min-width:12ch;",
                r#type: "search",
                list: "ik-scan-providers",
                placeholder: i18n.t("console.scan.filter.provider"),
                "aria-label": i18n.t("console.scan.filter.provider"),
                value: filter.provider.clone().unwrap_or_default(),
                oninput: move |event: FormEvent| {
                    let slug = event.value();
                    nav.filter(ConsoleQuery {
                        provider: (!slug.trim().is_empty()).then_some(slug),
                        page: 0,
                        ..nav.query()
                    });
                },
            }
            Chip {
                label: i18n.t("console.scan.filter.showCleared"),
                active: filter.cleared,
                size: Size::Xs,
                on_toggle: move |()| {
                    let query = nav.query();
                    nav.filter(ConsoleQuery {
                        cleared: !query.cleared,
                        page: 0,
                        ..query
                    });
                },
            }
            if active {
                Button {
                    size: Size::Xs,
                    on_click: move |_| {
                        // Selection survives a filter reset: the operator is widening what they
                        // can see, not closing the row they were reading.
                        nav.filter(ConsoleQuery {
                            sel: nav.query().sel,
                            ..ConsoleQuery::fresh()
                        });
                    },
                    {i18n.t("console.scan.filter.reset")}
                }
            }
        }
    }
}

/// The provider slugs the filter box completes against, taken from whatever the health table
/// already knows about.
///
/// A `datalist` instead of a `select`: the filter accepts free text so a link can name a
/// provider that has since gone quiet, and turning it into a picker would make that unreachable.
#[component]
pub(super) fn ProviderOptions(slugs: Vec<String>) -> Element {
    rsx! {
        datalist { id: "ik-scan-providers",
            for slug in slugs {
                option { key: "{slug}", value: "{slug}" }
            }
        }
    }
}
