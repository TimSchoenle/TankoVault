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
    async_view, use_step_up_gate, ListFooter, ListSearch, NoSelection, SkeletonBlock, StepUpGuard,
};
use crate::hooks::use_reload;
use crate::i18n::use_i18n;
use crate::models::*;
use crate::views::console::{use_console_nav, RefreshTick};
use dioxus::prelude::*;
use inkstone_ui::{Size, ToggleButton};
use inspect::MergeInspector;
use progenitor_client::ResponseValue;
use row::MergeListRow;

/// How deep into the journal one page of this section reads.
///
/// Larger than the decision journal's page because two of the four filters and the search are
/// applied here rather than by the endpoint — see [`Lens`] — so the loaded window *is* the
/// searchable one.
const PAGE_SIZE: u32 = 200;

/// How the operator narrows the list.
///
/// `Flagged` is the endpoint's own predicate; `Reverted` and `ByOperator` are not — the journal
/// indexes neither `reverted_at` nor `trigger` as a filter — so they run over the loaded window.
/// That is stated in the footer rather than hidden, because "no reverted merges" and "none in the
/// newest two hundred" are different answers.
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

    /// Whether this row belongs under this lens.
    fn keeps(self, decision: &MergeDecision) -> bool {
        match self {
            Self::All => true,
            Self::Reverted => decision.reverted_at.is_some(),
            Self::Flagged => decision.flagged_at.is_some(),
            Self::ByOperator => decision.trigger == "operator",
        }
    }
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
    let flagged_only = lens == Lens::Flagged;

    let rows = use_resource(use_reactive!(|flagged_only| {
        tick.track();
        reload.track();
        let client = api.client();
        async move {
            // Only decisions that merged something: this section is about what can be taken
            // back, and a queued or declined pair has nothing to put back.
            let mut request = client
                .list_merge_decisions()
                .outcome("merged")
                .limit(PAGE_SIZE);
            if flagged_only {
                request = request.flagged(true);
            }
            request
                .send()
                .await
                .map(ResponseValue::into_inner)
                .map_err(|e| api::friendly_error(i18n, e))
        }
    }));

    let needle = view.q.trim().to_lowercase();
    let loaded: Vec<MergeDecision> = match &*rows.read_unchecked() {
        Some(Ok(list)) => list.clone(),
        _ => Vec::new(),
    };
    let shown: Vec<MergeDecision> = loaded
        .iter()
        .filter(|decision| lens.keeps(decision) && matches(&needle, decision))
        .cloned()
        .collect();
    let reversible = shown.iter().filter(|d| d.revertible).count();

    // Falls back to the first row so the inspector is never empty, and a `sel` naming a row the
    // filter dropped falls back too rather than lighting nothing in the list.
    let chosen = view
        .sel
        .as_deref()
        .and_then(|id| shown.iter().find(|d| d.id.to_string() == id))
        .or_else(|| shown.first())
        .cloned();
    let selected = chosen.as_ref().map(|d| d.id);

    rsx! {
        div { class: "ik-cons-list",
            div { class: "ik-cons-listhead",
                ListSearch {
                    placeholder: i18n.t("console.merges.filter"),
                    query: view.q.clone(),
                    on_input: move |text| nav.filter(nav.query().with_search(text)),
                    hits: i18n.plural(
                        "console.merges.hits",
                        i64::try_from(shown.len()).unwrap_or(0),
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
                                nav.filter(next);
                            },
                            {i18n.t(option.label_key())}
                        }
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
            ListFooter {
                count: i18n.args(
                    "console.merges.count",
                    &[
                        ("shown", &shown.len().to_string()),
                        ("reversible", &reversible.to_string()),
                    ],
                ),
            }
        }
        if let Some(decision) = chosen {
            div { class: "ik-cons-insp",
                StepUpGuard { gate, intro: Some(i18n.t("console.stepUp.intro")) }
                MergeInspector { key: "{decision.id}", decision, gate, tick }
            }
        } else {
            NoSelection { message: i18n.t("console.merges.pick") }
        }
    }
}

/// Whether a row answers the already-lowercased search text.
///
/// Both titles and both ids: an operator arriving from a reader's report has the id of the
/// series that stopped existing, which is on the row that absorbed it and nowhere else.
fn matches(needle: &str, decision: &MergeDecision) -> bool {
    if needle.is_empty() {
        return true;
    }
    [
        decision.left_title.to_lowercase(),
        decision.right_title.to_lowercase(),
        decision.left_id.to_string(),
        decision.right_id.to_string(),
        decision.id.to_string(),
    ]
    .iter()
    .any(|field| field.contains(needle))
}

#[cfg(test)]
mod tests {
    use super::Lens;

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
}
