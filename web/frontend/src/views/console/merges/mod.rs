//! Console · Merges — the merges that were actually performed, and the one screen that takes one
//! back.
//!
//! A merge is the row here, not a line in a journal of everything the sweep considered. That
//! distinction is the whole point of the section: reverting used to be a button on a row in
//! [`super::decisions`], which lists every verdict including the thousands that merged nothing,
//! so the operator asking "what did we absorb into this title, and can I undo it" had to find it
//! by typing a title into a search box that matched on loaded rows only.
//!
//! The journal itself is unchanged — this is a different read over it, plus the two writes that
//! were already live (`revert`, `flag`). [`super::decisions`] keeps the read side for every other
//! outcome and for the sync journal; it no longer carries the actions.

mod inspect;
mod row;
mod unmerge;

use crate::api;
use crate::components::{
    async_view, use_step_up_gate, ListSearch, NoSelection, SeekPager, SkeletonBlock, StepUpGuard,
};
use crate::hooks::use_reload;
use crate::i18n::use_i18n;
use crate::models::*;
use crate::views::console::{landing_selection, use_console_nav, RefreshTick};
use dioxus::prelude::*;
use inkstone_ui::{Size, ToggleButton};
use inspect::MergeInspector;
use progenitor_client::ResponseValue;
use row::MergeListRow;

/// Rows per page. Every lens and the search run in the endpoint, so this sizes a page, not the
/// reach of the section.
const PAGE_SIZE: usize = 50;

/// How the operator narrows the list. Each lens is one of the endpoint's own filters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Lens {
    All,
    Reverted,
    Flagged,
    ByOperator,
}

impl Lens {
    const ALL: [Self; 4] = [Self::All, Self::Reverted, Self::Flagged, Self::ByOperator];

    /// This lens' `?status=` token. Shares the parameter every other console filter uses.
    const fn token(self) -> &'static str {
        match self {
            Self::All => "",
            Self::Reverted => "reverted",
            Self::Flagged => "flagged",
            Self::ByOperator => "operator",
        }
    }

    const fn label_key(self) -> &'static str {
        match self {
            Self::All => "console.merges.lens.all",
            Self::Reverted => "console.merges.lens.reverted",
            Self::Flagged => "console.merges.lens.flagged",
            Self::ByOperator => "console.merges.lens.byOperator",
        }
    }

    /// An unrecognised token opens the unfiltered list rather than refusing the link.
    fn parse(token: &str) -> Self {
        Self::ALL
            .into_iter()
            .find(|lens| lens.token() == token)
            .unwrap_or(Self::All)
    }
}

/// What the inspector should open, before any fetch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pick {
    /// This row of the loaded page.
    Row(usize),
    /// A decision the URL names that is not on this page; it is fetched by id.
    Fetch(uuid::Uuid),
    /// Nothing to open.
    Nothing,
}

/// Resolve `sel` against the loaded page.
///
/// A `sel` that is a decision id but not on this page is fetched rather than replaced by the
/// first row: a link from a series page or the journal names one merge, and opening another in
/// its place is the bug this section had while it only read the newest two hundred. A `sel`
/// that is not an id at all falls back to the first row.
fn pick(sel: Option<&str>, page: &[MergeDecision]) -> Pick {
    let first = if page.is_empty() {
        Pick::Nothing
    } else {
        Pick::Row(0)
    };
    let Some(sel) = sel else {
        return first;
    };
    if let Some(at) = page.iter().position(|d| d.id.to_string() == sel) {
        return Pick::Row(at);
    }
    uuid::Uuid::parse_str(sel).map_or(first, Pick::Fetch)
}

/// The list pane and the inspector pane, as the console shell's two grid children.
#[component]
pub(super) fn MergesEntity(tick: RefreshTick) -> Element {
    let api = api::use_api();
    let i18n = use_i18n();
    let nav = use_console_nav();
    let reload = use_reload();
    // One gate for the section: revert and flag are both elevated, and both report through the
    // inspector, so the prompt belongs at the top of the pane rather than once per row.
    let gate = use_step_up_gate();
    let view = nav.query();
    let lens = Lens::parse(view.status_token());
    let search = view.q.trim().to_owned();
    let page = i64::from(view.page);
    let page_size = i64::try_from(PAGE_SIZE).unwrap_or(i64::MAX);

    let rows = use_resource(use_reactive!(|(lens, search, page)| {
        tick.track();
        reload.track();
        let client = api.client();
        async move {
            // Only decisions that merged something: this section is about what can be taken
            // back, and a queued or declined pair has nothing to put back. One row past the page
            // is the probe for Next, since the endpoint reports no total.
            let mut request = client
                .list_merge_decisions()
                .outcome("merged")
                .limit(page_size + 1)
                .offset(page.saturating_mul(page_size));
            request = match lens {
                Lens::All => request,
                Lens::Reverted => request.reverted(true),
                Lens::Flagged => request.flagged(true),
                Lens::ByOperator => request.trigger("operator"),
            };
            if !search.is_empty() {
                request = request.search(search);
            }
            request
                .send()
                .await
                .map(ResponseValue::into_inner)
                .map_err(|e| api::friendly_error(i18n, e))
        }
    }));

    let mut shown: Vec<MergeDecision> = match &*rows.read_unchecked() {
        Some(Ok(list)) => list.clone(),
        _ => Vec::new(),
    };
    let has_next = shown.len() > PAGE_SIZE;
    shown.truncate(PAGE_SIZE);
    let reversible = shown.iter().filter(|d| d.revertible).count();

    let wanted = pick(view.sel.as_deref(), &shown);
    let off_page = match wanted {
        Pick::Fetch(id) => Some(id),
        Pick::Row(_) | Pick::Nothing => None,
    };
    let pinned = use_resource(use_reactive!(|off_page| {
        tick.track();
        reload.track();
        let client = api.client();
        async move {
            let id = off_page?;
            Some(
                client
                    .get_merge_decision()
                    .id(id)
                    .send()
                    .await
                    .map(ResponseValue::into_inner),
            )
        }
    }));

    // The outer `None` is "still loading the decision the URL names", so the first row never
    // flashes into the inspector in its place. A `sel` that resolves to nothing falls back to
    // the first row.
    let chosen: Option<Option<MergeDecision>> = match wanted {
        Pick::Row(at) => Some(shown.get(at).cloned()),
        Pick::Nothing => Some(None),
        Pick::Fetch(_) => match &*pinned.read_unchecked() {
            Some(Some(Ok(decision))) => Some(Some(decision.clone())),
            Some(Some(Err(_))) => Some(shown.first().cloned()),
            Some(None) | None => None,
        },
    };
    let selected = chosen.as_ref().and_then(|c| c.as_ref().map(|d| d.id));
    let outside_page = selected.is_some_and(|id| shown.iter().all(|d| d.id != id));

    // …and the fallback goes into the URL, so the address names the merge on screen rather than
    // whichever one is newest under the filter that happens to be applied. It replaces rather
    // than pushes — the operator did not choose this row and must not have to back out of it —
    // and the next query is built out here rather than inside the effect, because reading
    // `nav.query()` in there would subscribe the effect to the memo the write changes.
    let landing = landing_selection(view.sel.as_deref(), selected.map(|id| id.to_string()))
        .map(|sel| view.with_selection(Some(sel)));
    use_effect(use_reactive!(|landing| {
        if let Some(next) = landing {
            nav.filter(next);
        }
    }));

    let hits = i64::try_from(shown.len()).unwrap_or(0);
    let first = page.saturating_mul(page_size);
    let summary = if shown.is_empty() {
        String::new()
    } else {
        i18n.args(
            "console.merges.count",
            &[
                ("first", &(first + 1).to_string()),
                ("last", &(first + hits).to_string()),
                ("reversible", &reversible.to_string()),
            ],
        )
    };

    rsx! {
        div { class: "ik-cons-list",
            div { class: "ik-cons-listhead",
                ListSearch {
                    placeholder: i18n.t("console.merges.filter"),
                    query: view.q.clone(),
                    on_input: move |text| nav.filter(nav.query().with_search(text)),
                    hits: i18n.plural(
                        if has_next { "console.merges.hitsMore" } else { "console.merges.hits" },
                        hits,
                        &[],
                    ),
                }
                div { class: "ik-flex", style: "gap:5px;flex-wrap:wrap;",
                    for option in Lens::ALL {
                        ToggleButton {
                            key: "{option.token()}",
                            on: option == lens,
                            size: Size::Xs,
                            on_toggle: move |_| {
                                let mut next = nav.query();
                                next.status = (option != Lens::All).then(|| option.token().to_owned());
                                next.sel = None;
                                next.page = 0;
                                nav.filter(next);
                            },
                            {i18n.t(option.label_key())}
                        }
                    }
                }
                if outside_page {
                    div { class: "ik-muted", style: "font-size:12px;",
                        {i18n.t("console.merges.offPage")}
                    }
                }
            }
            {
                async_view(
                    &rows,
                    reload,
                    || rsx! {
                        div { style: "padding:12px;",
                            SkeletonBlock { height: 180 }
                        }
                    },
                    |_| {
                        if shown.is_empty() {
                            return rsx! {
                                div { class: "ik-empty", style: "margin:12px;padding:24px;",
                                    {i18n.t("console.merges.empty")}
                                }
                            };
                        }
                        rsx! {
                            for decision in shown.clone() {
                                MergeListRow {
                                    key: "{decision.id}",
                                    decision: decision.clone(),
                                    selected: selected == Some(decision.id),
                                    on_pick: move |id: uuid::Uuid| {
                                        nav.select(nav.query().with_selection(Some(id.to_string())));
                                    },
                                }
                            }
                        }
                    },
                )
            }
            SeekPager {
                page,
                has_next,
                summary,
                on_page: move |next: i64| {
                    nav.select(nav.query().with_page(u32::try_from(next).unwrap_or(0)));
                },
            }
        }
        match chosen {
            Some(Some(decision)) => rsx! {
                div { class: "ik-cons-insp",
                    StepUpGuard { gate, intro: Some(i18n.t("console.stepUp.intro")) }
                    MergeInspector { key: "{decision.id}", decision, gate, tick }
                }
            },
            Some(None) => rsx! {
                NoSelection { message: i18n.t("console.merges.pick") }
            },
            None => rsx! {
                div { class: "ik-cons-insp",
                    div { style: "padding:22px;",
                        SkeletonBlock { height: 280 }
                    }
                }
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{pick, Lens, Pick};

    /// Every lens is worded, and every token round-trips through the URL parameter it rides in.
    ///
    /// A lens whose token does not parse back reads as "All" the moment an operator sends the
    /// link, which is a filtered list silently becoming an unfiltered one.
    #[test]
    fn every_lens_round_trips_and_is_worded() {
        for lens in Lens::ALL {
            assert_eq!(Lens::parse(lens.token()), lens);
            assert!(
                crate::i18n::has_key(lens.label_key()),
                "`{}` has no catalogue entry",
                lens.token()
            );
        }
    }

    /// The bug this pins: a `sel` naming a merge that was not on the loaded page opened the
    /// first row instead, so "why merged" on a series page showed some other merge.
    #[test]
    fn a_selection_off_the_page_is_fetched_not_replaced() {
        let id = uuid::Uuid::from_u128(0xaa);
        assert_eq!(pick(Some(&id.to_string()), &[]), Pick::Fetch(id));
    }

    #[test]
    fn no_selection_or_a_garbled_one_lands_on_the_first_row() {
        assert_eq!(pick(None, &[]), Pick::Nothing);
        assert_eq!(pick(Some("not-an-id"), &[]), Pick::Nothing);
    }
}
