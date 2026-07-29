//! Reusable Inkstone UI components (design §17.3/§17.4).

mod cover;
mod feedback;
mod form;
mod nav;
mod shell;
mod topbar;

pub(crate) use cover::{Cover, CoverCard};
pub(crate) use feedback::{
    async_list, async_view, AuthRequired, EmptyBox, ErrorBox, ErrorLine, OutcomeLine,
    SkeletonBlock, SkeletonGrid, SkeletonRows,
};
pub(crate) use form::Field;
pub(crate) use shell::Shell;

use dioxus::prelude::*;

/// Global unread-notification count, provided at the app root, pushed to by the SSE stream
/// and recomputed by the Notifications view. A newtype so it is distinct in the context map.
#[derive(Clone, Copy)]
pub(crate) struct UnreadBadge(pub Signal<i64>);
