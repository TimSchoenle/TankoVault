//! The data table's chrome: the horizontal-scroll wrapper, the header row, and the caption that
//! names the table for a screen reader.

use crate::skin::{use_skin, Flag, Part, Variant};
use dioxus::prelude::*;

/// One header cell.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct TableColumn {
    label: String,
    numeric: bool,
    width: Option<String>,
}

impl TableColumn {
    /// A text column.
    #[must_use]
    pub fn new(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            numeric: false,
            width: None,
        }
    }

    /// Right-aligned and tabular — for counts, durations and sizes.
    #[must_use]
    pub fn numeric(mut self) -> Self {
        self.numeric = true;
        self
    }

    /// A fixed column width (any CSS length).
    #[must_use]
    pub fn width(mut self, width: impl Into<String>) -> Self {
        self.width = Some(width.into());
        self
    }
}

/// A table.
///
/// `caption` is required and visually hidden by default: a table with no accessible name is
/// announced as "table" and nothing else, and every table in the screens this replaced was one.
#[component]
pub fn Table(
    caption: String,
    columns: Vec<TableColumn>,
    /// Denser padding and uppercase headers. The default, because these tables are dashboards.
    #[props(default = true)]
    compact: bool,
    /// Show the caption instead of hiding it.
    #[props(default = false)]
    show_caption: bool,
    /// Bound the table's height to `--ik-table-max-h` and scroll the rows under a header row
    /// that stays put. Without it the wrapper grows with the page and the sticky header, which
    /// sticks to the wrapper rather than to the window, has nothing to stick against.
    #[props(default = false)]
    scroll: bool,
    #[props(default)] class: String,
    children: Element,
) -> Element {
    let skin = use_skin();
    let class = skin.class_with(
        Part::Table,
        &[Variant::flag(compact, Flag::Compact)],
        &class,
    );
    rsx! {
        div { class: skin.class(Part::TableWrap, &[Variant::flag(scroll, Flag::Scroll)]),
            table { class,
                caption {
                    class: if show_caption { String::new() } else { skin.class(Part::VisuallyHidden, &[]) },
                    "{caption}"
                }
                thead {
                    tr {
                        for column in columns {
                            th {
                                key: "{column.label}",
                                class: if column.numeric { skin.class(Part::NumericCell, &[]) } else { String::new() },
                                style: column.width.map(|width| format!("width:{width};")).unwrap_or_default(),
                                scope: "col",
                                "{column.label}"
                            }
                        }
                    }
                }
                tbody { {children} }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    /// The console's tables page at fifty rows of dense mono and the header — 11px, uppercase,
    /// tracked — was gone by row twelve. Both halves of the fix are here because either alone
    /// is a rule that looks right and never fires: the sticky header sticks to `.ik-tablewrap`,
    /// which is a scroll container in both axes, so it does nothing until that wrapper's height
    /// is bounded; and an unfilled header lets fifty rows scroll straight through it.
    #[test]
    fn the_header_row_stays_put() {
        let css = include_str!("../styles/inkstone.css");
        let head = css
            .split_once(".ik-table th {")
            .and_then(|(_, rest)| rest.split_once('}'))
            .expect("the stylesheet must carry a `.ik-table th` rule")
            .0;
        assert!(
            head.contains("position: sticky"),
            "the header row must stay put while the rows scroll under it"
        );
        assert!(
            head.contains("background:"),
            "a sticky header without an opaque fill has the rows scroll through it"
        );
        assert!(
            css.contains(".ik-tablewrap.scroll"),
            "sticky needs a bounded scrollport: `.scroll` is what gives the wrapper one"
        );
    }
}
