//! Reusable Inkstone UI components (design §17.3/§17.4).

mod confirm;
mod cover;
mod data;
mod feedback;
mod form;
mod layout;
mod nav;
mod pagination;
mod shell;
mod tabs;
mod topbar;

pub(crate) use confirm::{InlineConfirm, TypeToConfirm};
pub(crate) use cover::{Cover, CoverCard};
pub(crate) use data::{HealthPill, Kpi};
pub(crate) use feedback::{
    async_block, async_block_list, async_list, async_view, AuthRequired, EmptyBox, ErrorBox,
    ErrorLine, OutcomeLine, SkeletonBlock, SkeletonGrid, SkeletonRows,
};
pub(crate) use form::{Field, ListSearch, SegControl, SliderRow};
pub(crate) use layout::{ListFooter, NoSelection, PanelCard, Section};
pub(crate) use pagination::{CompactPager, Pagination, Window};
pub(crate) use shell::Shell;
pub(crate) use tabs::{TabBar, TabKind};

use dioxus::prelude::*;

/// Global unread-notification count, provided at the app root, pushed to by the SSE stream
/// and recomputed by the Notifications view. A newtype so it is distinct in the context map.
#[derive(Clone, Copy)]
pub(crate) struct UnreadBadge(pub Signal<i64>);
