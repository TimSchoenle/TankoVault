//! Reusable Inkstone UI components (design §17.3/§17.4).

mod bottombar;
mod confirm;
mod cover;
mod footer;
mod data;
mod feedback;
mod form;
mod layout;
mod nav;
mod pagination;
mod shell;
mod tabs;
mod topbar;

pub(crate) use bottombar::BottomTabs;
pub(crate) use confirm::{InlineConfirm, TypeToConfirm};
pub(crate) use footer::Footer;
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

/// Global unread-notification count, provided at the app root and updated by the SSE stream.
#[derive(Clone, Copy)]
pub(crate) struct UnreadBadge(pub Signal<i64>);
