//! Loading, empty and error states (§17.3: never a bare spinner; name what failed and how to
//! retry), plus the [`async_view`] helper that renders all three from one `Resource`.

use crate::hooks::Reload;
use crate::i18n::use_i18n;
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

/// A named error state with a retry affordance. `message` is an already-resolved sentence
/// (typically from [`crate::api::friendly_error`]); the framing around it is translated here.
#[component]
pub(crate) fn ErrorBox(message: String, on_retry: EventHandler<()>) -> Element {
    let i18n = use_i18n();
    rsx! {
        div { class: "ik-error",
            p { {i18n.args("feedback.failed", &[("message", &message)])} }
            button { class: "ik-btn", onclick: move |_| on_retry.call(()), {i18n.t("common.tryAgain")} }
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
///
/// While the boot-time silent refresh is still in flight it is a skeleton instead: the token is
/// re-adopted from the refresh cookie by a network round trip, so every reload of a protected
/// screen used to flash "Sign in to see this" at a reader who was signed in the whole time.
#[component]
pub(crate) fn SignInGate() -> Element {
    let i18n = use_i18n();
    let session = crate::state::use_session();
    if !session.is_settled() {
        return rsx! {
            SkeletonRows { count: 3 }
        };
    }
    rsx! {
        div { class: "ik-empty",
            p { {i18n.t("feedback.signInGate")} }
            Link { to: crate::Route::Login {}, class: "ik-btn primary", {i18n.t("common.signIn")} }
        }
    }
}

/// The whole "you must be signed in" screen: the page title plus [`SignInGate`].
///
/// Deliberately a guard callers early-return, not a wrapper taking `children`: views compute
/// derived state after the check, and a wrapper would have to build that state before deciding
/// not to show it.
#[component]
pub(crate) fn AuthRequired(title: String) -> Element {
    rsx! {
        h1 { class: "ik-page-title", "{title}" }
        SignInGate {}
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

/// [`async_view`] with a fixed-height [`SkeletonBlock`] as its loading state — the shape every
/// console panel and sidebar card wants.
pub(crate) fn async_block<T: 'static>(
    resource: &Resource<Result<T, String>>,
    reload: Reload,
    height: u32,
    content: impl FnOnce(&T) -> Element,
) -> Element {
    async_view(
        resource,
        reload,
        || {
            rsx! {
                SkeletonBlock { height }
            }
        },
        content,
    )
}

/// [`async_list`] with a fixed-height [`SkeletonBlock`] as its loading state.
pub(crate) fn async_block_list<T: 'static>(
    resource: &Resource<Result<Vec<T>, String>>,
    reload: Reload,
    height: u32,
    empty: &str,
    content: impl FnOnce(&[T]) -> Element,
) -> Element {
    async_list(
        resource,
        reload,
        || {
            rsx! {
                SkeletonBlock { height }
            }
        },
        empty,
        content,
    )
}

/// [`async_view`] for a list: adds the "loaded, but there is nothing here" state, which is a
/// different message from an error and must never look like one.
///
/// `empty` is already-resolved text, so the caller can interpolate the filter or query.
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
