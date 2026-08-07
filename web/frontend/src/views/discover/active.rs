//! The removable "active filter" chip bar above Discover's results.

use super::query::DiscoverFilters;
use crate::i18n::use_i18n;
use crate::icons::{Ic, Icon};
use crate::models::*;
use dioxus::prelude::*;

#[component]
pub(super) fn ActiveFilters(
    filters: DiscoverFilters,
    tags: Vec<TagFacet>,
    providers: Vec<PublicProvider>,
    on_change: EventHandler<DiscoverFilters>,
) -> Element {
    let i18n = use_i18n();
    if filters.active_count() == 0 {
        return rsx! {};
    }
    let name_of = |slug: &str| {
        tags.iter()
            .find(|t| t.slug == slug)
            .map_or_else(|| slug.to_owned(), |t| t.name.clone())
    };
    let provider_label = filters.provider.as_ref().map(|slug| {
        providers
            .iter()
            .find(|p| &p.slug == slug)
            .map_or_else(|| slug.clone(), |p| p.name.clone())
    });
    rsx! {
        div { class: "ik-active-filters",
            if let Some(label) = provider_label {
                {
                    let mut next = filters.clone();
                    next.provider = None;
                    rsx! { Chip { label, on_remove: move |()| on_change.call(next.clone()) } }
                }
            }
            for t in filters.types.clone() {
                {
                    let mut next = filters.clone();
                    next.toggle_type(t);
                    rsx! {
                        Chip {
                            key: "type-{t.token()}",
                            label: i18n.t(t.label_key()),
                            on_remove: move |()| on_change.call(next.clone()),
                        }
                    }
                }
            }
            for s in filters.statuses.clone() {
                {
                    let mut next = filters.clone();
                    next.toggle_status(s);
                    rsx! {
                        Chip {
                            key: "status-{s.token()}",
                            label: i18n.t(s.label_key()),
                            on_remove: move |()| on_change.call(next.clone()),
                        }
                    }
                }
            }
            for slug in filters.inc.clone() {
                {
                    let label = format!("+ {}", name_of(&slug));
                    let mut next = filters.clone();
                    next.drop_tag(&slug);
                    rsx! {
                        Chip { key: "inc-{slug}", label, on_remove: move |()| on_change.call(next.clone()) }
                    }
                }
            }
            for slug in filters.exc.clone() {
                {
                    let label = format!("− {}", name_of(&slug));
                    let mut next = filters.clone();
                    next.drop_tag(&slug);
                    rsx! {
                        Chip { key: "exc-{slug}", label, on_remove: move |()| on_change.call(next.clone()) }
                    }
                }
            }
        }
    }
}

/// One removable chip. The button carries the filter it would leave behind rather than a
/// description of what to remove, so the bar has no second copy of the removal rules.
#[component]
fn Chip(label: String, on_remove: EventHandler<()>) -> Element {
    rsx! {
        div { class: "ik-afchip",
            "{label}"
            button { onclick: move |_| on_remove.call(()), Ic { icon: Icon::Close, size: 12 } }
        }
    }
}
