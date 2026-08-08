//! Console · Catalogue — what is in the catalogue, and the two ways to take things out of it:
//! a ticked selection deleted from the bulk-action menu, and the purge in the danger zone.
//!
//! Off the shared auto-refresh tick like the other work surfaces: a background refetch that
//! reshuffled the list under a half-made selection would arm a delete against rows the operator
//! never ticked. It reloads after its own writes, and on the manual refresh.

mod purge;
mod row;

use crate::api;
use crate::components::{
    async_view, use_step_up_gate, CompactPager, InlineConfirm, Kpi, ListSearch, OutcomeLine,
    SegControl, SkeletonRows, StepUpPrompt, Window,
};
use crate::hooks::{use_busy, use_outcome, use_reload};
use crate::i18n::use_i18n;
use crate::icons::{Ic, Icon};
use crate::state::capabilities::use_capabilities;
use crate::util::thousands;
use crate::views::console::{use_console_nav, ConsoleQuery};
use crate::wire::types::{BulkDeleteSeries, HealthFilter, Permission, SeriesId};
use dioxus::prelude::*;
use progenitor_client::ResponseValue;
use purge::PurgePanel;
use row::CatalogueTableRow;
use std::collections::HashSet;

/// Rows per page. The server's default; it clamps regardless.
const PAGE_SIZE: i64 = 50;

/// The health filter, as the `?status=` token the URL carries.
///
/// A local mirror of the wire enum rather than the enum itself: the URL vocabulary is this
/// panel's own, and an unrecognised token has to widen to "everything" rather than refuse the
/// link — behaviour a generated `FromStr` cannot provide.
fn health_from_token(token: &str) -> HealthFilter {
    match token {
        "orphaned" => HealthFilter::Orphaned,
        "empty" => HealthFilter::Empty,
        _ => HealthFilter::Any,
    }
}

/// The `?status=` token for a health filter. `Any` is the absence of the parameter, not a value.
fn health_token(health: HealthFilter) -> &'static str {
    match health {
        HealthFilter::Any => "",
        HealthFilter::Orphaned => "orphaned",
        HealthFilter::Empty => "empty",
    }
}

/// The catalogue maintenance panel: totals, a filterable list with a bulk-action menu, and the
/// purge.
#[component]
pub(super) fn CatalogueEntity() -> Element {
    let api = api::use_api();
    let i18n = use_i18n();
    let caps = use_capabilities();
    let reload = use_reload();
    let nav = use_console_nav();
    let view = nav.query();

    let can_delete = caps.can(Permission::CatalogueDelete);
    let search = view.q.clone();
    let provider = view.provider.clone();
    let health = health_from_token(view.status_token());
    let page = i64::from(view.page);

    let summary = use_resource(move || {
        reload.track();
        let client = api.client();
        async move {
            client
                .catalogue_summary()
                .send()
                .await
                .map(ResponseValue::into_inner)
                .map_err(|e| api::friendly_error(i18n, e))
        }
    });

    let listing = use_resource(use_reactive!(|(search, provider, health, page)| {
        reload.track();
        let client = api.client();
        async move {
            let mut request = client
                .list_catalogue()
                .limit(PAGE_SIZE)
                .offset(page * PAGE_SIZE)
                .health(health);
            if !search.trim().is_empty() {
                request = request.search(search);
            }
            if let Some(slug) = provider.as_deref().filter(|s| !s.trim().is_empty()) {
                request = request.provider(slug);
            }
            request
                .send()
                .await
                .map(ResponseValue::into_inner)
                .map_err(|e| api::friendly_error(i18n, e))
        }
    }));

    // The ticked rows, by id rather than by index: the page under a selection can be refetched,
    // and an index-keyed selection would then point at whatever moved into that slot.
    let mut picked = use_signal(HashSet::<SeriesId>::new);
    // A filter change clears it. Ticking rows, narrowing the filter, then deleting is otherwise
    // a delete aimed at rows the operator can no longer see — the one way this surface could
    // destroy something nobody looked at. Paging does *not* clear it, so a selection can span
    // pages, which is the whole point of the count in the bar.
    let filter_key = format!(
        "{}|{}|{}",
        view.q,
        view.provider.clone().unwrap_or_default(),
        health_token(health)
    );
    use_effect(use_reactive!(|filter_key| {
        let _ = &filter_key;
        picked.write().clear();
    }));

    let (rows, total) = match &*listing.read() {
        Some(Ok(page_data)) => (page_data.items.clone(), page_data.total),
        _ => (Vec::new(), 0),
    };
    let window = Window {
        offset: page * PAGE_SIZE,
        page_len: i64::try_from(rows.len()).unwrap_or(0),
        total,
    };
    let page_ids: Vec<SeriesId> = rows.iter().map(|r| r.id).collect();
    let page_all_picked =
        !page_ids.is_empty() && page_ids.iter().all(|id| picked.read().contains(id));

    rsx! {
        div { class: "ik-cons-pane",
            section { style: "margin-bottom:18px;",
                h3 { {i18n.t("console.catalogue.title")} }
                p { class: "ik-muted", style: "font-size:12.5px;line-height:1.5;margin:0 0 12px;",
                    {i18n.t("console.catalogue.intro")}
                }
                match &*summary.read() {
                    Some(Ok(totals)) => rsx! {
                        div { class: "ik-kpis", style: "margin-bottom:14px;",
                            Kpi {
                                label: i18n.t("console.catalogue.stat.series"),
                                value: thousands(totals.series_total),
                                large: true,
                            }
                            Kpi {
                                label: i18n.t("console.catalogue.stat.sources"),
                                value: thousands(totals.sources_total),
                                large: true,
                            }
                            Kpi {
                                label: i18n.t("console.catalogue.stat.chapters"),
                                value: thousands(totals.chapters_total),
                                large: true,
                            }
                            Kpi {
                                label: i18n.t("console.catalogue.stat.orphaned"),
                                value: thousands(totals.orphaned_series),
                                accent: if totals.orphaned_series > 0 { "warn".to_owned() } else { String::new() },
                                large: true,
                            }
                            Kpi {
                                label: i18n.t("console.catalogue.stat.empty"),
                                value: thousands(totals.empty_series),
                                accent: if totals.empty_series > 0 { "warn".to_owned() } else { String::new() },
                                large: true,
                            }
                        }
                    },
                    _ => rsx! {},
                }

                div { class: "ik-flex", style: "gap:8px;flex-wrap:wrap;margin-bottom:10px;",
                    div { style: "flex:1;min-width:16ch;",
                        ListSearch {
                            placeholder: i18n.t("console.catalogue.searchPlaceholder"),
                            query: view.q.clone(),
                            on_input: move |text| nav.filter(nav.query().with_search(text)),
                            hits: i18n.plural("console.catalogue.hits", total, &[]),
                        }
                    }
                    input {
                        class: "ik-input",
                        style: "width:auto;min-width:12ch;",
                        r#type: "search",
                        placeholder: i18n.t("console.scan.filter.provider"),
                        "aria-label": i18n.t("console.scan.filter.provider"),
                        value: view.provider.clone().unwrap_or_default(),
                        oninput: move |event: FormEvent| {
                            let slug = event.value();
                            nav.filter(ConsoleQuery {
                                provider: (!slug.trim().is_empty()).then_some(slug),
                                page: 0,
                                ..nav.query()
                            });
                        },
                    }
                    SegControl {
                        options: vec![
                            (String::new(), i18n.t("console.catalogue.health.any")),
                            ("orphaned".to_owned(), i18n.t("console.catalogue.health.orphaned")),
                            ("empty".to_owned(), i18n.t("console.catalogue.health.empty")),
                        ],
                        selected: health_token(health).to_owned(),
                        on_select: move |token: String| {
                            nav.filter(ConsoleQuery {
                                status: (!token.is_empty()).then_some(token),
                                page: 0,
                                ..nav.query()
                            });
                        },
                    }
                    button {
                        class: "ik-btn xs",
                        onclick: move |_| reload.bump(),
                        Ic { icon: Icon::Refresh, size: 12 }
                        {i18n.t("console.live.refresh")}
                    }
                }

                if can_delete {
                    BulkBar { picked, reload }
                }

                {
                    async_view(
                        &listing,
                        reload,
                        || rsx! { SkeletonRows { count: 8, height: 24 } },
                        |_| {
                            if rows.is_empty() {
                                return rsx! {
                                    div { class: "ik-empty", style: "padding:24px;",
                                        {i18n.t("console.catalogue.empty")}
                                    }
                                };
                            }
                            let ids = page_ids.clone();
                            rsx! {
                                div { class: "ik-tablewrap",
                                    table { class: "ik-table ik-table-compact",
                                        thead {
                                            tr {
                                                if can_delete {
                                                    th { style: "width:30px;",
                                                        input {
                                                            class: "ik-cbx",
                                                            r#type: "checkbox",
                                                            "aria-label": i18n.t("console.catalogue.selectPage"),
                                                            checked: page_all_picked,
                                                            onchange: move |event: FormEvent| {
                                                                let mut set = picked.write();
                                                                for id in &ids {
                                                                    if event.checked() {
                                                                        set.insert(*id);
                                                                    } else {
                                                                        set.remove(id);
                                                                    }
                                                                }
                                                            },
                                                        }
                                                    }
                                                }
                                                th { {i18n.t("console.catalogue.col.title")} }
                                                th { {i18n.t("console.catalogue.col.providers")} }
                                                th { style: "text-align:right;", {i18n.t("console.catalogue.col.chapters")} }
                                                th { style: "text-align:right;", {i18n.t("console.catalogue.col.watchers")} }
                                                th { {i18n.t("console.catalogue.col.added")} }
                                            }
                                        }
                                        tbody {
                                            for entry in rows.clone() {
                                                CatalogueTableRow {
                                                    key: "{entry.id}",
                                                    entry: entry.clone(),
                                                    selectable: can_delete,
                                                    picked,
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        },
                    )
                }
                CompactPager {
                    page,
                    window,
                    on_page: move |next: i64| {
                        nav.select(nav.query().with_page(u32::try_from(next).unwrap_or(0)));
                    },
                }
            }

            if can_delete {
                PurgePanel {
                    totals: match &*summary.read() {
                        Some(Ok(totals)) => Some(totals.clone()),
                        _ => None,
                    },
                    reload,
                }
            }
        }
    }
}

/// The selection bar: how many rows are armed, and the menu that acts on them.
///
/// A `<select>` rather than a row of buttons because the list of bulk actions is expected to
/// grow, and because a destructive action reached by picking it from a closed menu cannot be
/// hit by a mis-aimed click the way a permanently visible Delete button can.
#[component]
fn BulkBar(picked: Signal<HashSet<SeriesId>>, reload: crate::hooks::Reload) -> Element {
    let api = api::use_api();
    let i18n = use_i18n();
    let gate = use_step_up_gate();
    let busy = use_busy();
    let mut outcome = use_outcome();
    let mut picked = picked;
    let mut confirming = use_signal(|| false);

    let count = i64::try_from(picked.read().len()).unwrap_or(i64::MAX);
    if count == 0 {
        return rsx! {};
    }

    let delete = move |()| {
        if !busy.claim() {
            return;
        }
        outcome.set(None);
        let series_ids: Vec<SeriesId> = picked.peek().iter().copied().collect();
        // Elevated: the API guards every mutating operator capability with a second factor, and
        // answers `403 step_up_required` until it has one.
        let client = gate.client(api);
        spawn(async move {
            match client
                .bulk_delete_series()
                .body(BulkDeleteSeries { series_ids })
                .send()
                .await
                .map(ResponseValue::into_inner)
            {
                Ok(report) => {
                    outcome.set(Some(Ok(i18n.args(
                        "console.catalogue.deleted",
                        &[
                            ("series", &thousands(report.series)),
                            ("chapters", &thousands(report.chapters)),
                            ("watchers", &thousands(report.watchlist_entries)),
                        ],
                    ))));
                    picked.write().clear();
                    confirming.set(false);
                    reload.bump();
                }
                Err(e) => {
                    if !gate.refused(api::Refusal::of(&e)) {
                        outcome.set(Some(Err(api::guarded_error(i18n, e))));
                    }
                }
            }
            busy.release();
        });
    };

    rsx! {
        div { class: "ik-cons-selbar",
            span { class: "cnt", {i18n.plural("console.catalogue.selected", count, &[])} }
            select {
                class: "ik-select",
                style: "font-size:11.5px;padding:5px 8px;",
                "aria-label": i18n.t("console.catalogue.bulkActions"),
                disabled: busy.is_busy(),
                // Controlled back to the placeholder every render: the menu is a command
                // launcher, not a setting, and a `<select>` left showing "Delete selected"
                // reads as a state the rows are now in.
                value: "",
                onchange: move |event: FormEvent| {
                    if event.value() == "delete" {
                        confirming.set(true);
                    }
                },
                option { value: "", {i18n.t("console.catalogue.bulkActions")} }
                option { value: "delete", {i18n.t("console.catalogue.bulk.delete")} }
            }
            button {
                class: "ik-btn xs",
                style: "margin-left:auto;",
                onclick: move |_| {
                    picked.write().clear();
                    confirming.set(false);
                },
                {i18n.t("console.catalogue.clearSelection")}
            }
        }
        if *confirming.read() {
            div { class: "ik-danger", style: "margin-bottom:10px;",
                InlineConfirm {
                    title: i18n.t("console.catalogue.bulk.delete"),
                    body: i18n.plural("console.catalogue.deleteRadius", count, &[]),
                    cta: i18n.t("console.catalogue.bulk.deleteCta"),
                    busy: busy.is_busy(),
                    on_cancel: move |()| confirming.set(false),
                    on_confirm: delete,
                }
            }
        }
        if gate.is_open() {
            StepUpPrompt {
                enrolled: true,
                intro: Some(i18n.t("console.stepUp.intro")),
                on_done: move |()| {
                    gate.close();
                    outcome.set(Some(Ok(i18n.t("stepUp.confirmedRetry"))));
                },
            }
        }
        OutcomeLine { outcome: outcome.read().clone() }
    }
}

#[cfg(test)]
mod tests {
    use super::{health_from_token, health_token};
    use crate::wire::types::HealthFilter;

    /// The URL is user-editable, so an unrecognised token must widen the list rather than
    /// refuse the link — the same rule the rest of `ConsoleQuery` follows.
    #[test]
    fn an_unknown_health_token_shows_everything() {
        assert_eq!(health_from_token("nonsense"), HealthFilter::Any);
        assert_eq!(health_from_token(""), HealthFilter::Any);
    }

    #[test]
    fn every_health_filter_round_trips_through_its_token() {
        for health in [
            HealthFilter::Any,
            HealthFilter::Orphaned,
            HealthFilter::Empty,
        ] {
            assert_eq!(health_from_token(health_token(health)), health);
        }
    }
}
