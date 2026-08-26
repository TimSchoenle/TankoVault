//! Per-provider statistics: catalogue footprint, content freshness and last-scan health.
//!
//! Ten columns at ~12 px is a wall, so which of them are drawn — and how tightly — is the
//! operator's choice, persisted to `localStorage` rather than the URL: it belongs to the
//! reader, not to the link they were sent.

use crate::api;
use crate::components::{async_block_list, HealthPill};
use crate::i18n::use_i18n;
use crate::models::*;
use crate::state::prefs;
use crate::util::{rel_time, thousands};
use crate::views::console::RefreshTick;
use dioxus::prelude::*;
use progenitor_client::ResponseValue;

/// A hideable column of the provider stats table.
///
/// The provider itself is not here: a table of numbers with no row identity is not a narrower
/// table, it is an unreadable one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Column {
    Adapter,
    Series,
    Sources,
    Chapters,
    Day,
    Week,
    Newest,
    LastScan,
    LastRun,
}

impl Column {
    const ALL: [Column; 9] = [
        Self::Adapter,
        Self::Series,
        Self::Sources,
        Self::Chapters,
        Self::Day,
        Self::Week,
        Self::Newest,
        Self::LastScan,
        Self::LastRun,
    ];

    /// The token this column is remembered by.
    fn token(self) -> &'static str {
        match self {
            Self::Adapter => "adapter",
            Self::Series => "series",
            Self::Sources => "sources",
            Self::Chapters => "chapters",
            Self::Day => "day",
            Self::Week => "week",
            Self::Newest => "newest",
            Self::LastScan => "lastScan",
            Self::LastRun => "lastRun",
        }
    }

    /// The catalogue key of this column's heading (see [`crate::i18n`]).
    fn label_key(self) -> &'static str {
        match self {
            Self::Adapter => "console.stats.col.adapter",
            Self::Series => "console.stats.col.series",
            Self::Sources => "console.stats.col.sources",
            Self::Chapters => "console.stats.col.chapters",
            Self::Day => "console.stats.col.day",
            Self::Week => "console.stats.col.week",
            Self::Newest => "console.stats.col.newest",
            Self::LastScan => "console.stats.col.lastScan",
            Self::LastRun => "console.stats.col.lastRun",
        }
    }

    /// Whether this column's values are numbers, and so right-aligned.
    fn numeric(self) -> bool {
        matches!(
            self,
            Self::Series | Self::Sources | Self::Chapters | Self::Day | Self::Week
        )
    }
}

/// Per-provider statistics table: catalogue footprint, freshness, and last-scan health.
#[component]
pub(super) fn ProviderStatsTable(tick: RefreshTick) -> Element {
    let api = api::use_api();
    let i18n = use_i18n();
    let mut hidden = use_signal(prefs::console_hidden_columns);
    let mut compact = use_signal(prefs::console_compact);
    let mut picking = use_signal(|| false);

    let res = use_resource(move || {
        tick.track();
        let client = api.client();
        async move {
            client
                .provider_stats()
                .send()
                .await
                .map(ResponseValue::into_inner)
                .map_err(|e| api::friendly_error(i18n, e))
        }
    });

    let hidden_now = hidden.read().clone();
    let shown: Vec<Column> = Column::ALL
        .into_iter()
        .filter(|column| !hidden_now.contains(column.token()))
        .collect();
    let table_class = if *compact.read() {
        "ik-table ik-table-compact"
    } else {
        "ik-table"
    };

    let empty = i18n.t("console.stats.empty");
    rsx! {
        section { style: "margin-bottom:18px;",
            div { class: "ik-flex", style: "justify-content:space-between;align-items:center;gap:10px;",
                h3 { style: "margin:0;", {i18n.t("console.stats.title")} }
                div { class: "ik-flex", style: "gap:6px;",
                    button {
                        class: if *compact.read() { "ik-chip active" } else { "ik-chip" },
                        style: "font-size:11.5px;padding:4px 9px;",
                        onclick: move |_| {
                            let next = !*compact.peek();
                            compact.set(next);
                            prefs::set_console_compact(next);
                        },
                        {i18n.t("console.table.compact")}
                    }
                    button {
                        class: "ik-chip",
                        style: "font-size:11.5px;padding:4px 9px;",
                        "aria-expanded": if *picking.read() { "true" } else { "false" },
                        onclick: move |_| {
                            let next = !*picking.peek();
                            picking.set(next);
                        },
                        {i18n.t("console.table.columns")}
                    }
                }
            }
            if *picking.read() {
                fieldset {
                    class: "ik-card",
                    style: "margin:8px 0;padding:10px;display:flex;gap:12px;flex-wrap:wrap;border:1px solid var(--border-ctl);",
                    legend { class: "ik-muted", style: "font-size:11.5px;padding:0 4px;",
                        {i18n.t("console.table.columnsLegend")}
                    }
                    for column in Column::ALL {
                        label {
                            key: "{column.token()}",
                            class: "ik-flex",
                            style: "gap:5px;font-size:12px;align-items:center;",
                            input {
                                r#type: "checkbox",
                                checked: !hidden_now.contains(column.token()),
                                onchange: move |event: FormEvent| {
                                    let mut next = hidden.peek().clone();
                                    if event.checked() {
                                        next.remove(column.token());
                                    } else {
                                        next.insert(column.token().to_owned());
                                    }
                                    prefs::set_console_hidden_columns(&next);
                                    hidden.set(next);
                                },
                            }
                            {i18n.t(column.label_key())}
                        }
                    }
                }
            }
            {
                async_block_list(
                    &res,
                    tick.reload(),
                    120,
                    &empty,
                    |rows| {
                        let rows = rows.to_vec();
                        let columns = shown.clone();
                        rsx! {
                            div { class: "ik-tablewrap scroll",
                                table { class: "{table_class}",
                                    thead {
                                        tr {
                                            th { {i18n.t("console.stats.col.provider")} }
                                            for column in columns.clone() {
                                                th {
                                                    key: "{column.token()}",
                                                    style: if column.numeric() { "text-align:right;" } else { "" },
                                                    {i18n.t(column.label_key())}
                                                }
                                            }
                                        }
                                    }
                                    tbody {
                                        for p in rows {
                                            ProviderStatRow {
                                                key: "{p.provider_id}",
                                                stat: p,
                                                columns: columns.clone(),
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    },
                )
            }
        }
    }
}

#[component]
fn ProviderStatRow(stat: ProviderStat, columns: Vec<Column>) -> Element {
    let i18n = use_i18n();
    let s = stat;
    let blocked = if s.blocked_sources > 0 {
        i18n.args(
            "console.stats.blockedSources",
            &[("count", &thousands(s.blocked_sources))],
        )
    } else {
        String::new()
    };
    let last_run = match (&s.last_run_state, s.last_run_at.as_deref()) {
        (Some(state), at) => format!("{state} · {}", rel_time(i18n, at)),
        (None, _) => i18n.t("time.unknown"),
    };
    rsx! {
        tr {
            td {
                div { style: "font-weight:600;", "{s.name}" }
                div { class: "ik-flex", style: "gap:6px;margin-top:2px;",
                    HealthPill { state: s.state.parse().ok() }
                    span { class: "ik-mono ik-muted", style: "font-size:11px;", "{s.slug}" }
                }
            }
            for column in columns {
                match column {
                    Column::Adapter => rsx! {
                        td { key: "{column.token()}", class: "ik-mono ik-muted", style: "font-size:12px;", "{s.adapter}" }
                    },
                    Column::Series => rsx! {
                        td { key: "{column.token()}", class: "ik-mono", style: "text-align:right;", "{thousands(s.series_count)}" }
                    },
                    Column::Sources => rsx! {
                        td { key: "{column.token()}", class: "ik-mono", style: "text-align:right;",
                            "{thousands(s.source_count)}"
                            if !blocked.is_empty() {
                                span { class: "ik-muted", style: "font-size:11px;", "{blocked}" }
                            }
                        }
                    },
                    Column::Chapters => rsx! {
                        td { key: "{column.token()}", class: "ik-mono", style: "text-align:right;", "{thousands(s.chapter_count)}" }
                    },
                    Column::Day => rsx! {
                        td { key: "{column.token()}", class: "ik-mono", style: "text-align:right;",
                            if s.chapters_24h > 0 {
                                span { style: "color:var(--jade);", "+{thousands(s.chapters_24h)}" }
                            } else {
                                span { class: "ik-muted", "0" }
                            }
                        }
                    },
                    Column::Week => rsx! {
                        td { key: "{column.token()}", class: "ik-mono ik-muted", style: "text-align:right;", "{thousands(s.chapters_7d)}" }
                    },
                    Column::Newest => rsx! {
                        td { key: "{column.token()}", class: "ik-muted ik-mono", style: "font-size:12px;",
                            "{rel_time(i18n, s.last_chapter_at.as_deref())}"
                        }
                    },
                    Column::LastScan => rsx! {
                        td { key: "{column.token()}", class: "ik-muted ik-mono", style: "font-size:12px;",
                            "{rel_time(i18n, s.last_scanned_at.as_deref())}"
                        }
                    },
                    Column::LastRun => rsx! {
                        td { key: "{column.token()}", class: "ik-muted ik-mono", style: "font-size:12px;", "{last_run}" }
                    },
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Column;

    /// A column the operator hides is remembered by its token, so a token that is not unique
    /// would hide two columns with one checkbox — and a missing catalogue key renders as the
    /// key itself rather than as an error, so an unworded column ships as `console.stats.col.…`.
    #[test]
    fn every_column_has_a_unique_token_and_a_worded_heading() {
        let mut tokens: Vec<&str> = Column::ALL.iter().map(|c| c.token()).collect();
        tokens.sort_unstable();
        let unique = tokens.len();
        tokens.dedup();
        assert_eq!(tokens.len(), unique, "two columns share a token");

        for column in Column::ALL {
            let key = column.label_key();
            assert!(
                crate::i18n::has_key(key),
                "column `{}` is offered but `{key}` is not in the catalogue",
                column.token()
            );
        }
    }
}
