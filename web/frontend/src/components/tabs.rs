//! The app's tab strip: the kit's tablist, keyed by a screen's own tab enum and worded from the
//! message catalogue.

use crate::i18n::use_i18n;
use dioxus::prelude::*;
use inkstone_ui::TabItem;

/// A closed set of tabs: what they are, and the catalogue key wording each one.
pub(crate) trait TabKind: Copy + PartialEq + 'static {
    /// Every tab this kind defines, in strip order.
    fn all() -> &'static [Self]
    where
        Self: Sized;

    /// The catalogue key of this tab's label.
    fn label_key(self) -> &'static str;
}

/// A tab strip with real tab semantics.
///
/// `visible` restricts the strip to a subset — Account hides panels a reader has no capability
/// for, and rendering a tab that opens nothing is worse than omitting it.
///
/// Controlled: the caller owns the selection, because in the console it is a URL parameter, and
/// a signal here would hold a second copy of it.
#[component]
pub(crate) fn TabBar<T: TabKind + Clone + PartialEq + 'static>(
    selected: T,
    on_select: EventHandler<T>,
    #[props(default)] visible: Option<Vec<T>>,
    /// Names the strip for a screen reader; the kit puts it on the tablist itself.
    #[props(default)]
    label: Option<String>,
    /// `ik-tabs flush` + the console's top margin, for strips that sit inside an inspector.
    #[props(default = false)]
    flush: bool,
    /// One row that scrolls sideways with a trailing fade, for a strip too long to wrap
    /// gracefully.
    #[props(default = false)]
    scroll: bool,
) -> Element {
    let i18n = use_i18n();
    let items = visible
        .unwrap_or_else(|| T::all().to_vec())
        .into_iter()
        .map(|entry| TabItem::new(entry, i18n.t(entry.label_key())))
        .collect();

    rsx! {
        inkstone_ui::TabBar { items, selected, on_select, label, flush, scroll }
    }
}
