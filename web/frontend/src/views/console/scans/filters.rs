//! The scan panel's filter bar.
//!
//! Every control writes straight to the URL and reads straight back out of it — no signal
//! shadows a parameter, because a shadowed filter is one that reverts on the back button.

use super::{ScanFilter, STATE_FILTERS};
use crate::i18n::use_i18n;
use crate::models::{RunSort, RunSortExt, RunStateExt as _, ScanMode, ScanModeExt};
use crate::views::console::query::Window as TimeWindow;
use crate::views::console::{use_console_nav, ConsoleQuery};
use dioxus::prelude::*;
use inkstone_ui::{Button, Size};
/// State, mode, window, ordering, provider and the show-cleared toggle, plus a reset.
#[component]
pub(super) fn FilterBar(filter: ScanFilter) -> Element {
    let i18n = use_i18n();
    let nav = use_console_nav();
    let active = filter.narrows_runs() || filter.cleared;

    rsx! {
        div { class: "ik-flex", style: "gap:8px;flex-wrap:wrap;margin-top:10px;",
            select {
                class: "ik-input",
                style: "width:auto;",
                "aria-label": i18n.t("console.scan.filter.state"),
                value: filter.status.clone().unwrap_or_default(),
                onchange: move |event: FormEvent| {
                    let chosen = event.value();
                    nav.filter(ConsoleQuery {
                        status: (!chosen.is_empty()).then_some(chosen),
                        ..nav.query()
                    });
                },
                option { value: "", {i18n.t("console.scan.filter.anyState")} }
                for state in STATE_FILTERS {
                    option {
                        key: "{state.0}",
                        value: "{state.0}",
                        {i18n.t(state.1.label_key())}
                    }
                }
            }
            select {
                class: "ik-input",
                style: "width:auto;",
                "aria-label": i18n.t("console.scan.filter.mode"),
                value: filter.mode.clone().unwrap_or_default(),
                onchange: move |event: FormEvent| {
                    let chosen = event.value();
                    nav.filter(ConsoleQuery {
                        mode: (!chosen.is_empty()).then_some(chosen),
                        ..nav.query()
                    });
                },
                option { value: "", {i18n.t("console.scan.filter.anyMode")} }
                for option in <ScanMode as ScanModeExt>::all().iter().copied() {
                    option {
                        key: "{option.token()}",
                        value: option.token(),
                        {i18n.t(option.label_key())}
                    }
                }
            }
            select {
                class: "ik-input",
                style: "width:auto;",
                "aria-label": i18n.t("console.audit.filter.window"),
                value: filter.since.token(),
                onchange: move |event: FormEvent| {
                    nav.filter(ConsoleQuery {
                        since: TimeWindow::parse_token(&event.value()),
                        ..nav.query()
                    });
                },
                for option in TimeWindow::ALL {
                    option {
                        key: "{option.label_key()}",
                        value: option.token(),
                        {i18n.t(option.label_key())}
                    }
                }
            }
            select {
                class: "ik-input",
                style: "width:auto;",
                "aria-label": i18n.t("console.scan.filter.sort"),
                value: filter.ordering().token(),
                onchange: move |event: FormEvent| {
                    let chosen = <RunSort as RunSortExt>::parse(&event.value());
                    nav.filter(ConsoleQuery {
                        // The default ordering is the *absence* of the parameter, so a shared
                        // link names only what its sender actually changed.
                        sort: (chosen != RunSort::Recent).then(|| chosen.token().to_owned()),
                        ..nav.query()
                    });
                },
                for option in <RunSort as RunSortExt>::all().iter().copied() {
                    option {
                        key: "{option.token()}",
                        value: option.token(),
                        {i18n.t(option.label_key())}
                    }
                }
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
                        ..nav.query()
                    });
                },
            }
            label { class: "ik-flex", style: "gap:5px;font-size:12px;align-items:center;",
                input {
                    r#type: "checkbox",
                    checked: filter.cleared,
                    onchange: move |event: FormEvent| {
                        nav.filter(ConsoleQuery {
                            cleared: event.checked(),
                            ..nav.query()
                        });
                    },
                }
                {i18n.t("console.scan.filter.showCleared")}
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
/// A `datalist` rather than a `select`: the filter accepts free text so a link can name a
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
