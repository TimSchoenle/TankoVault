//! The removable "active filter" chip bar above Discover's results.

use crate::i18n::use_i18n;
use crate::icons::{Ic, Icon};
use crate::models::*;
use dioxus::prelude::*;

#[component]
pub(super) fn ActiveFilters(
    types: Signal<Vec<ContentType>>,
    statuses: Signal<Vec<SeriesStatus>>,
    inc: Signal<Vec<String>>,
    exc: Signal<Vec<String>>,
    provider: Signal<Option<String>>,
    tags: Vec<Tag>,
    providers: Vec<PublicProvider>,
    page: Signal<usize>,
) -> Element {
    let i18n = use_i18n();
    let ty = types.read().clone();
    let st = statuses.read().clone();
    let inc_v = inc.read().clone();
    let exc_v = exc.read().clone();
    let prov = provider.read().clone();
    if ty.is_empty() && st.is_empty() && inc_v.is_empty() && exc_v.is_empty() && prov.is_none() {
        return rsx! {};
    }
    let name_of = |slug: &str| {
        tags.iter()
            .find(|t| t.slug == slug)
            .map_or_else(|| slug.to_owned(), |t| t.name.clone())
    };
    let provider_label = prov.as_ref().map(|slug| {
        providers
            .iter()
            .find(|p| &p.slug == slug)
            .map_or_else(|| slug.clone(), |p| p.name.clone())
    });
    rsx! {
        div { class: "ik-active-filters",
            if let Some(label) = provider_label {
                div { class: "ik-afchip",
                    "{label}"
                    button {
                        onclick: move |_| {
                            let mut v = provider;
                            v.set(None);
                            page.set(0);
                        },
                        Ic { icon: Icon::Close, size: 12 }
                    }
                }
            }
            for t in ty {
                div { class: "ik-afchip",
                    {i18n.t(t.label_key())}
                    button {
                        onclick: move |_| {
                            let mut v = types;
                            let pos = v.read().iter().position(|x| *x == t);
                            if let Some(i) = pos { v.write().remove(i); }
                            page.set(0);
                        },
                        Ic { icon: Icon::Close, size: 12 }
                    }
                }
            }
            for s in st {
                div { class: "ik-afchip",
                    {i18n.t(s.label_key())}
                    button {
                        onclick: move |_| {
                            let mut v = statuses;
                            let pos = v.read().iter().position(|x| *x == s);
                            if let Some(i) = pos { v.write().remove(i); }
                            page.set(0);
                        },
                        Ic { icon: Icon::Close, size: 12 }
                    }
                }
            }
            for slug in inc_v {
                {
                    let label = name_of(&slug);
                    rsx! {
                        div { class: "ik-afchip",
                            "+ {label}"
                            button {
                                onclick: move |_| {
                                    let mut v = inc;
                                    let pos = v.read().iter().position(|x| x == &slug);
                                    if let Some(i) = pos { v.write().remove(i); }
                                    page.set(0);
                                },
                                Ic { icon: Icon::Close, size: 12 }
                            }
                        }
                    }
                }
            }
            for slug in exc_v {
                {
                    let label = name_of(&slug);
                    rsx! {
                        div { class: "ik-afchip",
                            "− {label}"
                            button {
                                onclick: move |_| {
                                    let mut v = exc;
                                    let pos = v.read().iter().position(|x| x == &slug);
                                    if let Some(i) = pos { v.write().remove(i); }
                                    page.set(0);
                                },
                                Ic { icon: Icon::Close, size: 12 }
                            }
                        }
                    }
                }
            }
        }
    }
}
