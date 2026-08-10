//! Offset pagination: the arithmetic, and the two chromes built on it.
//!
//! [`Window`] is the shared arithmetic; its failure mode is invisible until someone notices a
//! page they cannot reach.

use crate::i18n::use_i18n;
use dioxus::prelude::*;
use inkstone_ui::{Button, Size};
/// One page of an offset-paginated list, as the three counts the arithmetic needs.
///
/// `page_len` is what the **server** returned for this window. It is deliberately not "the
/// number of rows on screen": a screen that filters the page client-side has fewer rows than
/// the server sent, and using that count here is what made Next lie.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Window {
    /// Rows skipped before this page.
    pub(crate) offset: i64,
    /// Rows the server returned for this page.
    pub(crate) page_len: i64,
    /// Rows in the whole collection, per the server.
    pub(crate) total: i64,
}

impl Window {
    /// Whether an earlier page exists.
    pub(crate) fn has_prev(self) -> bool {
        self.offset > 0
    }

    /// Whether a later page exists.
    pub(crate) fn has_next(self) -> bool {
        self.offset + self.page_len < self.total
    }

    /// The 1-based index of this page's first row, or 0 when the page is empty.
    pub(crate) fn first(self) -> i64 {
        if self.page_len == 0 {
            0
        } else {
            self.offset + 1
        }
    }

    /// The 1-based index of this page's last row.
    pub(crate) fn last(self) -> i64 {
        self.offset + self.page_len
    }
}

/// Collapsed sequence of page indices to render around `cur` (0-based). Always keeps the
/// first and last page reachable and fills single-page gaps directly instead of spending an
/// ellipsis on them, so long result sets don't spam a button per page (`None` = ellipsis).
fn page_window(cur: usize, pages: usize) -> Vec<Option<usize>> {
    if pages == 0 {
        return Vec::new();
    }
    if pages <= 7 {
        return (0..pages).map(Some).collect();
    }
    let last = pages - 1;
    let mut keep = vec![0, last, cur];
    if cur > 0 {
        keep.push(cur - 1);
    }
    if cur < last {
        keep.push(cur + 1);
    }
    keep.sort_unstable();
    keep.dedup();

    let mut out = Vec::with_capacity(keep.len() + 2);
    let mut prev: Option<usize> = None;
    for p in keep {
        match prev {
            Some(pv) if p == pv + 2 => out.push(Some(pv + 1)),
            Some(pv) if p > pv + 1 => out.push(None),
            _ => {}
        }
        out.push(Some(p));
        prev = Some(p);
    }
    out
}

/// Jump-box handler: parses the typed page number (1-based) and moves there, clamped to range.
fn jump_to_page(mut jump: Signal<String>, mut page: Signal<usize>, pages: usize) {
    if let Ok(n) = jump.read().trim().parse::<usize>() {
        if n >= 1 {
            page.set((n - 1).min(pages.saturating_sub(1)));
        }
    }
    jump.set(String::new());
}

/// The full pager: prev/next, a collapsed page-number window, and a jump box.
#[component]
pub(crate) fn Pagination(page: Signal<usize>, pages: usize, has_next: bool) -> Element {
    let i18n = use_i18n();
    let cur = *page.read();
    let mut jump = use_signal(String::new);

    rsx! {
        nav { class: "ik-pagination", "aria-label": i18n.t("discover.page.label"),
            button {
                class: "page",
                r#type: "button",
                disabled: cur == 0,
                onclick: move |_| { if cur > 0 { page.set(cur - 1); } },
                {i18n.t("discover.page.prev")}
            }
            for p in page_window(cur, pages) {
                match p {
                    Some(idx) => rsx! {
                        button {
                            class: if idx == cur { "page active" } else { "page" },
                            r#type: "button",
                            "aria-current": if idx == cur { "page" } else { "false" },
                            onclick: move |_| page.set(idx),
                            "{idx + 1}"
                        }
                    },
                    None => rsx! { span { class: "ellipsis", "…" } },
                }
            }
            button {
                class: "page",
                r#type: "button",
                disabled: !has_next && cur + 1 >= pages,
                onclick: move |_| page.set(cur + 1),
                {i18n.t("discover.page.next")}
            }
            if pages > 1 {
                div { class: "ik-page-jump",
                    label { r#for: "tv-page-jump", {i18n.t("discover.page.goTo")} }
                    input {
                        id: "tv-page-jump",
                        r#type: "number",
                        min: "1",
                        max: "{pages}",
                        value: "{jump.read()}",
                        placeholder: "{cur + 1}",
                        oninput: move |e| jump.set(e.value()),
                        onkeydown: move |e| {
                            if e.key() == Key::Enter {
                                jump_to_page(jump, page, pages);
                            }
                        },
                    }
                    {i18n.args("discover.page.ofTotal", &[("pages", &pages.to_string())])}
                    button {
                        class: "page",
                        r#type: "button",
                        onclick: move |_| jump_to_page(jump, page, pages),
                        {i18n.t("discover.page.go")}
                    }
                }
            }
        }
    }
}

/// The console list footer's pager: a range sentence plus prev/next, sized for a 328px column.
///
/// `page` is a 0-based page index; `window` describes what the server returned for it.
#[component]
pub(crate) fn CompactPager(
    /// Controlled: the console's page lives in the URL, so the caller owns it.
    page: i64,
    window: Window,
    on_page: EventHandler<i64>,
) -> Element {
    let i18n = use_i18n();
    rsx! {
        div { class: "ik-cons-foot",
            span {
                {
                    i18n.args(
                        "console.users.range",
                        &[
                            ("first", &window.first().to_string()),
                            ("last", &window.last().to_string()),
                            ("total", &crate::util::thousands(window.total)),
                        ],
                    )
                }
            }
            span { class: "hint", style: "display:flex;gap:6px;",
                Button {
                    size: Size::Xs,
                    disabled: !window.has_prev(),
                    on_click: move |_| on_page.call(page - 1),
                    {i18n.t("common.previous")}
                }
                Button {
                    size: Size::Xs,
                    disabled: !window.has_next(),
                    on_click: move |_| on_page.call(page + 1),
                    {i18n.t("common.next")}
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_runs_list_every_page() {
        assert_eq!(
            page_window(2, 5),
            vec![Some(0), Some(1), Some(2), Some(3), Some(4)]
        );
    }

    #[test]
    fn long_runs_collapse_around_the_current_page() {
        assert_eq!(
            page_window(10, 30),
            vec![Some(0), None, Some(9), Some(10), Some(11), None, Some(29)]
        );
    }

    /// A one-page gap costs the same width as an ellipsis, so it renders as the page itself —
    /// an ellipsis hiding a single reachable page is strictly worse than the page.
    #[test]
    fn single_page_gaps_render_as_the_page() {
        assert_eq!(
            page_window(2, 8),
            vec![Some(0), Some(1), Some(2), Some(3), None, Some(7)]
        );
    }

    #[test]
    fn no_pages_renders_nothing() {
        assert!(page_window(0, 0).is_empty());
    }

    /// The bug this arithmetic used to have: the console directory passed the *filtered* row
    /// count as `page_len`, so switching on a chip that hid rows shrank the left-hand side and
    /// left Next enabled on the last page — and the range sentence reported a client-side count
    /// against a server-side total.
    #[test]
    fn next_follows_the_server_page_length_not_the_visible_rows() {
        // The server returned a full page of 25 out of 60; two later pages exist.
        let full = Window {
            offset: 0,
            page_len: 25,
            total: 60,
        };
        assert!(full.has_next());
        assert!(!full.has_prev());
        assert_eq!((full.first(), full.last()), (1, 25));

        // The last page: the window ends exactly at the total, so there is nothing after it.
        let tail = Window {
            offset: 50,
            page_len: 10,
            total: 60,
        };
        assert!(!tail.has_next());
        assert!(tail.has_prev());
        assert_eq!((tail.first(), tail.last()), (51, 60));
    }

    #[test]
    fn an_empty_collection_reports_a_zero_range() {
        let empty = Window {
            offset: 0,
            page_len: 0,
            total: 0,
        };
        assert_eq!((empty.first(), empty.last()), (0, 0));
        assert!(!empty.has_next());
        assert!(!empty.has_prev());
    }
}
