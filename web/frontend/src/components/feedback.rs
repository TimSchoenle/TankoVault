//! Loading, empty and error states (§17.3: never a bare spinner; name what failed and how to
//! retry), plus the [`async_view`] helper that renders all three from one `Resource`.

use crate::hooks::Reload;
use dioxus::prelude::*;

/// Skeleton placeholder grid shown while covers load.
#[component]
pub(crate) fn SkeletonGrid(count: usize) -> Element {
    rsx! {
        div { class: "ik-grid",
            for _ in 0..count {
                div { class: "ik-card",
                    div { class: "ik-skeleton ik-skel-cover" }
                    div { class: "ik-card-body",
                        div { class: "ik-skeleton", style: "height:14px;width:80%;margin-bottom:6px;" }
                        div { class: "ik-skeleton", style: "height:12px;width:50%;" }
                    }
                }
            }
        }
    }
}

/// A stack of skeleton rows, for list-shaped screens (feed, notifications, sessions).
#[component]
pub(crate) fn SkeletonRows(count: usize, #[props(default = 16)] height: u32) -> Element {
    rsx! {
        for _ in 0..count {
            div { class: "ik-row",
                div { class: "ik-skeleton", style: "height:{height}px;width:45%;" }
            }
        }
    }
}

/// A plain rectangular skeleton, for a card-shaped region of known height.
#[component]
pub(crate) fn SkeletonBlock(#[props(default = 80)] height: u32) -> Element {
    rsx! {
        div { class: "ik-skeleton", style: "height:{height}px;" }
    }
}

/// A named error state with a retry affordance.
#[component]
pub(crate) fn ErrorBox(message: String, on_retry: EventHandler<()>) -> Element {
    rsx! {
        div { class: "ik-error",
            p { "Something went wrong: {message}" }
            button { class: "ik-btn", onclick: move |_| on_retry.call(()), "Try again" }
        }
    }
}

/// A one-line inline failure, for a panel too small to justify a full error box.
#[component]
pub(crate) fn ErrorLine(message: String) -> Element {
    rsx! {
        p { style: "font-size:13px;color:var(--acc);margin:0;", "{message}" }
    }
}

/// An inviting empty state (§17.2.1).
#[component]
pub(crate) fn EmptyBox(message: String) -> Element {
    rsx! {
        div { class: "ik-empty", "{message}" }
    }
}

/// A "please sign in" gate rendered by protected views when there is no session.
#[component]
pub(crate) fn SignInGate() -> Element {
    rsx! {
        div { class: "ik-empty",
            p { "Sign in to see this." }
            Link { to: crate::Route::Login {}, class: "ik-btn primary", "Sign in" }
        }
    }
}

/// A thin brush-stroke section divider (the one signature device, §17.1).
#[component]
pub(crate) fn Brush() -> Element {
    rsx! {
        div { class: "ik-brush" }
    }
}

/// An outcome line under a form: green when the action succeeded, accent when it failed.
///
/// Every mutating panel used to hand-roll this same two-arm match; sharing it keeps the two
/// states visually identical everywhere and stops one of them drifting.
#[component]
pub(crate) fn OutcomeLine(outcome: Option<Result<String, String>>) -> Element {
    match outcome {
        Some(Ok(message)) => rsx! {
            p { style: "font-size:13px;color:var(--jade-bright);margin:8px 0 0;", "{message}" }
        },
        Some(Err(message)) => rsx! {
            p { style: "font-size:13px;color:var(--acc);margin:8px 0 0;", "{message}" }
        },
        None => rsx! {},
    }
}

/// Render a fetched `Resource` through the standard three states: `loading` while it is in
/// flight, an [`ErrorBox`] wired to `reload` when it failed, and `content` once it resolved.
///
/// Roughly thirty call sites used to open-code this match, and they had already drifted —
/// some retried, some dead-ended; some surfaced the error, some swallowed it into an empty
/// list. Funnelling them through one helper makes "a failed fetch is always visible and
/// always retryable" a property of the app rather than of each screen.
pub(crate) fn async_view<T: 'static>(
    resource: &Resource<Result<T, String>>,
    reload: Reload,
    loading: impl FnOnce() -> Element,
    content: impl FnOnce(&T) -> Element,
) -> Element {
    match &*resource.read_unchecked() {
        None => loading(),
        Some(Err(message)) => {
            let message = message.clone();
            rsx! {
                ErrorBox { message, on_retry: move |()| reload.bump() }
            }
        }
        Some(Ok(value)) => content(value),
    }
}

/// [`async_view`] for a list: adds the "loaded, but there is nothing here" state, which is a
/// different message from an error and must never look like one.
pub(crate) fn async_list<T: 'static>(
    resource: &Resource<Result<Vec<T>, String>>,
    reload: Reload,
    loading: impl FnOnce() -> Element,
    empty: &str,
    content: impl FnOnce(&[T]) -> Element,
) -> Element {
    async_view(resource, reload, loading, |items| {
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
