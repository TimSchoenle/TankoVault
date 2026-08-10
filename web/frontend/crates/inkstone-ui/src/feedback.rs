//! Loading, empty and error states, plus the helpers that render all three from one `Resource`.
//!
//! The rule these encode: never a bare spinner, and never an empty list styled like a failure.
//! A reader has to be able to tell "nothing matched" from "we could not ask".

use crate::skin::{use_skin, Flag, Part, Variant};
use dioxus::prelude::*;

/// The result of a submitted action: `Ok` reads as a confirmation, `Err` as a failure, `None`
/// renders nothing.
pub type Outcome = Option<Result<String, String>>;

/// A rectangular loading placeholder of known height.
#[component]
pub fn Skeleton(
    #[props(default = 80)] height: u32,
    /// A percentage or CSS length; full width unless given.
    #[props(default)]
    width: Option<String>,
) -> Element {
    let width = width.unwrap_or_else(|| "100%".to_string());
    rsx! {
        div {
            class: use_skin().class(Part::Skeleton, &[]),
            style: "height:{height}px;width:{width};",
            "aria-hidden": "true",
        }
    }
}

/// A stack of skeleton rows, for list-shaped screens.
#[component]
pub fn SkeletonRows(count: usize, #[props(default = 16)] height: u32) -> Element {
    let skin = use_skin();
    rsx! {
        div { role: "status", "aria-busy": "true",
            for index in 0..count {
                div { key: "{index}", class: skin.class(Part::SkeletonRow, &[]),
                    Skeleton { height, width: "45%" }
                }
            }
        }
    }
}

/// A skeleton grid, for cover-shaped screens.
#[component]
pub fn SkeletonGrid(count: usize) -> Element {
    let skin = use_skin();
    rsx! {
        div { class: skin.class(Part::SkeletonGrid, &[]), role: "status", "aria-busy": "true",
            for index in 0..count {
                div { key: "{index}", class: skin.class(Part::SkeletonCard, &[]),
                    div { class: skin.class(Part::SkeletonCover, &[]) }
                    div { class: skin.class(Part::SkeletonCardBody, &[]),
                        Skeleton { height: 14, width: "80%" }
                        Skeleton { height: 12, width: "50%" }
                    }
                }
            }
        }
    }
}

/// A named failure with a retry affordance.
///
/// `message` is an already-resolved sentence and `retry_label` an already-translated word: the
/// kit carries no message catalogue.
#[component]
pub fn ErrorBox(message: String, retry_label: String, on_retry: EventHandler<()>) -> Element {
    rsx! {
        div { class: use_skin().class(Part::Error, &[]), role: "alert",
            p { "{message}" }
            crate::Button { on_click: move |_| on_retry.call(()), "{retry_label}" }
        }
    }
}

/// A one-line inline failure, for a panel too small to justify a full [`ErrorBox`].
#[component]
pub fn ErrorLine(message: String) -> Element {
    rsx! {
        p { class: use_skin().class(Part::ErrorLine, &[]), role: "alert", "{message}" }
    }
}

/// An empty state. Never styled as an error — it is not one.
#[component]
pub fn EmptyBox(message: String, #[props(default)] action: Option<Element>) -> Element {
    rsx! {
        div { class: use_skin().class(Part::Empty, &[]),
            p { "{message}" }
            {action}
        }
    }
}

/// The result line under a form.
#[component]
pub fn OutcomeLine(outcome: Outcome) -> Element {
    let skin = use_skin();
    match outcome {
        Some(Ok(message)) => rsx! {
            p {
                class: skin.class(Part::Outcome, &[Variant::Flag(Flag::Ok)]),
                role: "status",
                "{message}"
            }
        },
        Some(Err(message)) => rsx! {
            p {
                class: skin.class(Part::Outcome, &[Variant::Flag(Flag::Err)]),
                role: "alert",
                "{message}"
            }
        },
        None => rsx! {},
    }
}

/// Render a `Resource` through its three states: `loading` while in flight, an [`ErrorBox`] when
/// it failed, `content` once it resolved.
pub fn async_view<T: 'static>(
    resource: &Resource<Result<T, String>>,
    retry_label: &str,
    on_retry: EventHandler<()>,
    loading: impl FnOnce() -> Element,
    content: impl FnOnce(&T) -> Element,
) -> Element {
    match &*resource.read_unchecked() {
        None => loading(),
        Some(Err(message)) => {
            let message = message.clone();
            let retry_label = retry_label.to_owned();
            rsx! {
                ErrorBox { message, retry_label, on_retry }
            }
        }
        Some(Ok(value)) => content(value),
    }
}

/// [`async_view`] for a list, adding the "loaded, but there is nothing here" state.
///
/// `empty` is already-resolved text, so the caller can interpolate the filter or query.
pub fn async_list<T: 'static>(
    resource: &Resource<Result<Vec<T>, String>>,
    retry_label: &str,
    on_retry: EventHandler<()>,
    loading: impl FnOnce() -> Element,
    empty: &str,
    content: impl FnOnce(&[T]) -> Element,
) -> Element {
    async_view(resource, retry_label, on_retry, loading, |items| {
        if items.is_empty() {
            let message = empty.to_owned();
            rsx! {
                EmptyBox { message }
            }
        } else {
            content(items)
        }
    })
}
