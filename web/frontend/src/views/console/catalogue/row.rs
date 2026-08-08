//! One row of the catalogue maintenance table.

use crate::i18n::use_i18n;
use crate::util::{iso_date, thousands};
use crate::wire::types::{CatalogueRow, SeriesId};
use dioxus::prelude::*;
use std::collections::HashSet;

/// One series: what it is, who carries it, and the two numbers that say what deleting it costs.
///
/// The whole row toggles its checkbox, so ticking is a click anywhere on it — a selection surface
/// where only a 15-pixel box is live makes selecting fifty rows a chore.
#[component]
pub(super) fn CatalogueTableRow(
    entry: CatalogueRow,
    selectable: bool,
    picked: Signal<HashSet<SeriesId>>,
) -> Element {
    let i18n = use_i18n();
    let mut picked = picked;
    let id = entry.id;
    let checked = picked.read().contains(&id);
    // Precomputed rather than read inside `rsx!`: both handlers below own their captures, so a
    // single shared closure cannot be moved into each of them.
    let pick_label = i18n.args("console.catalogue.select", &[("title", &entry.title)]);
    let added = iso_date(Some(&entry.created_at)).to_owned();

    rsx! {
        tr {
            class: if checked { "ik-row-pick selected" } else { "ik-row-pick" },
            onclick: move |_| {
                if !selectable {
                    return;
                }
                let mut set = picked.write();
                if checked {
                    set.remove(&id);
                } else {
                    set.insert(id);
                }
            },
            if selectable {
                td {
                    input {
                        class: "ik-cbx",
                        r#type: "checkbox",
                        "aria-label": pick_label,
                        checked,
                        // The box sits inside the row it toggles, so without this the row's own
                        // handler fires next and undoes the click.
                        onclick: move |event: MouseEvent| event.stop_propagation(),
                        onchange: move |event: FormEvent| {
                            let mut set = picked.write();
                            if event.checked() {
                                set.insert(id);
                            } else {
                                set.remove(&id);
                            }
                        },
                    }
                }
            }
            td {
                div { style: "font-weight:600;", "{entry.title}" }
                div { class: "ik-mono ik-muted", style: "font-size:11px;",
                    "{entry.content_type} · {entry.status}"
                    if let Some(year) = entry.release_year {
                        " · {year}"
                    }
                }
            }
            td {
                if entry.providers.is_empty() {
                    span { class: "ik-pill amber", style: "font-size:9.5px;",
                        {i18n.t("console.catalogue.noProvider")}
                    }
                } else {
                    div { class: "ik-flex", style: "gap:4px;flex-wrap:wrap;",
                        for slug in entry.providers.clone() {
                            span { key: "{slug}", class: "ik-pill", style: "font-size:9.5px;", "{slug}" }
                        }
                    }
                }
            }
            td { class: "ik-mono", style: "text-align:right;", {thousands(entry.chapter_count)} }
            td {
                class: if entry.watcher_count > 0 { "ik-mono" } else { "ik-mono ik-muted" },
                style: "text-align:right;",
                {thousands(entry.watcher_count)}
            }
            td { class: "ik-muted ik-mono", style: "font-size:11.5px;white-space:nowrap;", "{added}" }
        }
    }
}
