//! The cover-grid alternate (design turn 4, option `4b`).
//!
//! Same data, same filters, same bands as the list — this is a density choice, not a different
//! screen, which is why it takes the already-fetched rows rather than fetching its own. What a
//! cover card can carry is a strict subset of what a 54px row carries, so the row menu and the
//! per-row actions stay on the list; the grid is for recognising a title by its art.

use crate::components::Cover;
use crate::i18n::use_i18n;
use crate::icons::{Ic, Icon};
use crate::models::*;
use crate::util::{chapter_number, rel_time, thousands};
use crate::Route;
use dioxus::prelude::*;
use std::collections::HashSet;

#[component]
pub(super) fn CoverGrid(items: Vec<WatchlistItem>, selected: Signal<HashSet<SeriesId>>) -> Element {
    let i18n = use_i18n();
    rsx! {
        div { class: "ik-wl-grid",
            for item in items {
                {
                    let series_id = item.series_id;
                    let is_selected = selected.read().contains(&series_id);
                    let read = item.last_read_number.unwrap_or(0.0);
                    let percent = if item.total_chapters > 0 {
                        #[expect(
                            clippy::cast_precision_loss,
                            reason = "a chapter count that loses f64 precision is 2^53 chapters"
                        )]
                        ((read / item.total_chapters as f64) * 100.0).clamp(0.0, 100.0)
                    } else {
                        0.0
                    };
                    rsx! {
                        div {
                            key: "{series_id}",
                            class: if is_selected { "ik-wl-tile selected" } else { "ik-wl-tile" },
                            Link { to: Route::Series { id: series_id.to_string() }, class: "ik-wl-tile-art",
                                Cover { url: item.cover_url.clone(), title: item.series_title.clone() }
                                if item.unread > 0 {
                                    span { class: "ik-wl-tile-badge", "{thousands(item.unread)}" }
                                }
                                if item.source_degraded {
                                    span {
                                        class: "ik-wl-tile-warn",
                                        title: i18n.t("watchlist.sourceOffline"),
                                        Ic { icon: Icon::Warning, size: 13 }
                                    }
                                }
                                span { class: "ik-wl-tile-bar", span { style: "width:{percent}%;" } }
                            }
                            div { class: "ik-wl-tile-name", "{item.series_title}" }
                            div { class: "ik-mono ik-wl-tile-meta",
                                "{chapter_number(read)} / {thousands(item.total_chapters)} · "
                                {rel_time(i18n, item.latest_chapter_at.as_deref())}
                            }
                        }
                    }
                }
            }
        }
    }
}
