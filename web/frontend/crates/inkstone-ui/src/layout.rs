//! Layout primitives.
//!
//! These exist to delete inline styles. A `style: "display:flex;gap:8px;align-items:baseline;"`
//! is invisible to the stylesheet, to the theme switch and to every audit that greps for a class
//! — and the screens this kit replaced carried more than eight hundred of them. [`Row`] and
//! [`Stack`] express the same intent as a closed set of tokens the skin names.

use crate::skin::{use_skin, Flag, Part, Variant};
use crate::tone::{Align, Gap, Justify};
use dioxus::prelude::*;

/// A horizontal flex line.
#[component]
pub fn Row(
    #[props(default)] gap: Gap,
    #[props(default)] align: Align,
    #[props(default)] justify: Justify,
    #[props(default = false)] wrap: bool,
    /// Fill the remaining space in a parent flex line.
    #[props(default = false)]
    grow: bool,
    #[props(default)] class: String,
    children: Element,
) -> Element {
    let class = use_skin().class_with(
        Part::Row,
        &[
            Variant::Gap(gap),
            Variant::Align(align),
            Variant::Justify(justify),
            Variant::flag(wrap, Flag::Wrap),
            Variant::flag(grow, Flag::Grow),
        ],
        &class,
    );
    rsx! {
        div { class, {children} }
    }
}

/// A vertical flex column.
#[component]
pub fn Stack(
    #[props(default = Gap::Sm)] gap: Gap,
    #[props(default = Align::Stretch)] align: Align,
    #[props(default = false)] grow: bool,
    #[props(default)] class: String,
    children: Element,
) -> Element {
    let class = use_skin().class_with(
        Part::Stack,
        &[
            Variant::Gap(gap),
            Variant::Align(align),
            Variant::flag(grow, Flag::Grow),
        ],
        &class,
    );
    rsx! {
        div { class, {children} }
    }
}

/// A plain surface box: border, radius, padding, no header.
#[component]
pub fn Tile(#[props(default)] class: String, children: Element) -> Element {
    let class = use_skin().class_with(Part::Tile, &[], &class);
    rsx! {
        div { class, {children} }
    }
}

/// A surface panel with an icon + title header.
#[component]
pub fn Card(
    #[props(default)] title: Option<String>,
    /// A glyph before the title. The kit ships no icons, so this is the app's.
    #[props(default)]
    icon: Option<Element>,
    /// Right-aligned beside the title — a count, a state pill, an action.
    #[props(default)]
    trailing: Option<Element>,
    #[props(default)] class: String,
    children: Element,
) -> Element {
    let skin = use_skin();
    let class = skin.class_with(Part::Panel, &[], &class);
    rsx! {
        div { class,
            if title.is_some() || icon.is_some() || trailing.is_some() {
                div { class: skin.class(Part::PanelHead, &[]),
                    {icon}
                    if let Some(title) = title {
                        strong { "{title}" }
                    }
                    if let Some(trailing) = trailing {
                        span { class: skin.class(Part::PanelHeadEnd, &[]), {trailing} }
                    }
                }
            }
            {children}
        }
    }
}

/// A mono uppercase section label with its content beneath.
#[component]
pub fn Section(
    label: String,
    /// Right-aligned beside the label — a validity note, a result pill.
    #[props(default)]
    trailing: Option<Element>,
    #[props(default)] class: String,
    children: Element,
) -> Element {
    let skin = use_skin();
    let class = skin.class_with(Part::Section, &[], &class);
    rsx! {
        div { class,
            div { class: skin.class(Part::SectionHead, &[]),
                span { class: skin.class(Part::SectionLabel, &[]), "{label}" }
                if let Some(trailing) = trailing {
                    span { class: skin.class(Part::SectionHeadEnd, &[]), {trailing} }
                }
            }
            {children}
        }
    }
}

/// A definition list: a fixed-width key column beside its values.
///
/// A `<dl>` rather than a grid of `<div>`s so the pairing survives a screen reader, which is the
/// whole reason to use one instead of two columns of text.
#[component]
pub fn Kv(
    #[props(default = false)] narrow: bool,
    #[props(default)] class: String,
    children: Element,
) -> Element {
    let class = use_skin().class_with(
        Part::DefinitionList,
        &[Variant::flag(narrow, Flag::Narrow)],
        &class,
    );
    rsx! {
        dl { class, {children} }
    }
}

/// One key/value pair inside a [`Kv`].
#[component]
pub fn KvRow(key_label: String, #[props(default)] class: String, children: Element) -> Element {
    rsx! {
        dt { class: use_skin().class(Part::DefinitionKey, &[]), "{key_label}" }
        dd { class, {children} }
    }
}

/// A thin brush-stroke divider — the design's one signature device.
#[component]
pub fn Brush() -> Element {
    rsx! {
        div { class: use_skin().class(Part::Divider, &[]), role: "separator" }
    }
}
