//! Loading, empty and error states (§17.3: never a bare spinner; name what failed and how to
//! retry), plus the [`async_view`] helper that renders all three from one `Resource`.
//!
//! The shapes themselves live in `inkstone-ui`. What stays here is the part that needs this
//! app: the message catalogue, the session, and the [`Reload`] handle a retry bumps.

use crate::hooks::Reload;
use crate::i18n::use_i18n;
use dioxus::prelude::*;
use inkstone_ui::{button_class, Size, Tone};
pub(crate) use inkstone_ui::{
    EmptyBox, ErrorLine, OutcomeLine, Skeleton as SkeletonBlock, SkeletonGrid, SkeletonRows,
};

/// A named error state with a retry affordance. `message` is an already-resolved sentence
/// (typically from [`crate::api::friendly_error`]); the framing around it is translated here.
#[component]
pub(crate) fn ErrorBox(message: String, on_retry: EventHandler<()>) -> Element {
    let i18n = use_i18n();
    let message = i18n.args("feedback.failed", &[("message", &message)]);
    rsx! {
        inkstone_ui::ErrorBox {
            message,
            retry_label: i18n.t("common.tryAgain"),
            on_retry,
        }
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
            Link {
                to: crate::Route::Login {},
                class: button_class(Tone::Primary, Size::Md, false),
                {i18n.t("common.signIn")}
            }
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

/// Render a fetched `Resource` through the standard three states: `loading` while it is in
/// flight, an [`ErrorBox`] wired to `reload` when it failed, and `content` once it resolved.
///
/// Not the kit's own `async_view`: this one renders the [`ErrorBox`] above, which reaches the
/// message catalogue from inside a component rather than asking every caller for a retry label.
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

/// [`async_block`] for a list, adding the empty state.
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
