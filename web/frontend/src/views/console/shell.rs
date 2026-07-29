//! Chrome and controls shared by the console's master–detail surfaces.
//!
//! The two-tier rule for destructive actions lives here, as two components rather than as a
//! convention each panel re-implements:
//!
//! - [`InlineConfirm`] — *reversible* (pause, blocklist, unlink, revoke): a second click, on
//!   the spot, with the consequence stated.
//! - [`TypeToConfirm`] — *irreversible* (delete a provider, erase an account): the operator
//!   types the exact slug or username, and the action stays disabled until it matches. The
//!   copy names the concrete blast radius with real counts; never "are you sure".

use crate::i18n::use_i18n;
use crate::icons::{Ic, Icon};
use dioxus::prelude::*;

/// A mono uppercase section label, optionally with something right-aligned beside it.
#[component]
pub(super) fn Section(
    label: String,
    /// Optional right-aligned status beside the label (a validity note, a result pill).
    #[props(default)]
    trailing: Option<Element>,
    children: Element,
) -> Element {
    rsx! {
        div {
            div { class: "ik-flex", style: "align-items:baseline;gap:8px;margin-bottom:9px;",
                span { class: "ik-sec-lbl", "{label}" }
                span { style: "margin-left:auto;display:flex;align-items:center;gap:8px;", {trailing} }
            }
            {children}
        }
    }
}

/// One option in a [`SegControl`]: the value written back, and the word for it.
pub(super) type SegOption = (String, String);

/// A segmented control — a small closed set of choices with exactly one lit.
#[component]
pub(super) fn SegControl(
    options: Vec<SegOption>,
    selected: String,
    on_select: EventHandler<String>,
) -> Element {
    rsx! {
        div { class: "ik-seg", role: "radiogroup",
            for (value , label) in options {
                button {
                    key: "{value}",
                    class: if value == selected { "on" } else { "" },
                    role: "radio",
                    "aria-checked": if value == selected { "true" } else { "false" },
                    onclick: move |_| on_select.call(value.clone()),
                    "{label}"
                }
            }
        }
    }
}

/// A labelled slider with its value pinned to the right in a fixed-width cell, so a row of
/// them keeps one alignment while the numbers change.
#[component]
pub(super) fn SliderRow(
    label: String,
    /// The raw slider position. Sliders are integer-stepped; fractional settings scale (see
    /// `step` and the caller's conversion).
    value: f64,
    min: f64,
    max: f64,
    step: f64,
    /// Pre-formatted, because a value's unit belongs to the caller (`0.8`, `2`, `900ms`).
    display: String,
    on_input: EventHandler<f64>,
) -> Element {
    rsx! {
        div { class: "ik-slider-row",
            label { class: "k", r#for: "tv-slider-{label}", "{label}" }
            input {
                id: "tv-slider-{label}",
                class: "ik-range grow",
                r#type: "range",
                min: "{min}",
                max: "{max}",
                step: "{step}",
                value: "{value}",
                oninput: move |event| {
                    if let Ok(parsed) = event.value().parse::<f64>() {
                        on_input.call(parsed);
                    }
                },
            }
            span { class: "v", "{display}" }
        }
    }
}

/// A reversible destructive action: state the consequence, ask once more, act.
#[component]
pub(super) fn InlineConfirm(
    title: String,
    body: String,
    cta: String,
    busy: bool,
    on_cancel: EventHandler<()>,
    on_confirm: EventHandler<()>,
) -> Element {
    let i18n = use_i18n();
    rsx! {
        div { class: "ik-inline-confirm",
            div { style: "min-width:0;",
                div { class: "ttl", "{title}" }
                div { class: "why", "{body}" }
            }
            div { class: "ik-flex", style: "margin-left:auto;gap:6px;flex:none;",
                button {
                    class: "ik-btn xs",
                    onclick: move |_| on_cancel.call(()),
                    {i18n.t("common.cancel")}
                }
                button {
                    class: "ik-btn xs primary",
                    disabled: busy,
                    onclick: move |_| on_confirm.call(()),
                    "{cta}"
                }
            }
        }
    }
}

/// An irreversible destructive action, gated on typing the exact identifier.
///
/// The button is genuinely `disabled` until the typed value matches, so it is unreachable by
/// keyboard as well as unclickable — a dimmed-but-live control would be worse than none.
#[component]
pub(super) fn TypeToConfirm(
    title: String,
    /// What will actually be destroyed, with real counts.
    body: String,
    /// The exact string the operator has to type.
    expect: String,
    cta: String,
    busy: bool,
    on_confirm: EventHandler<()>,
) -> Element {
    let i18n = use_i18n();
    let mut typed = use_signal(String::new);
    let matches = typed.read().trim() == expect;

    rsx! {
        div { class: "grave",
            div { class: "ttl", "{title}" }
            div { class: "why", "{body}" }
            div { class: "ik-confirm",
                span { class: "ik-mono", style: "font-size:11.5px;color:var(--faint);",
                    {i18n.t("console.confirm.type")}
                }
                span { class: "slug", "{expect}" }
                input {
                    autocomplete: "off",
                    spellcheck: "false",
                    "aria-label": i18n.args("console.confirm.label", &[("value", &expect)]),
                    placeholder: i18n.t("console.confirm.placeholder"),
                    value: "{typed}",
                    oninput: move |event| typed.set(event.value()),
                }
                button {
                    class: "go",
                    disabled: busy || !matches,
                    onclick: move |_| on_confirm.call(()),
                    "{cta}"
                }
            }
        }
    }
}

/// The list pane's pinned footer: how many rows there are, and the keys that move between them.
#[component]
pub(super) fn ListFooter(count: String, #[props(default = true)] keys: bool) -> Element {
    let i18n = use_i18n();
    rsx! {
        div { class: "ik-cons-foot",
            span { "{count}" }
            if keys {
                span { class: "hint", {i18n.t("console.listKeys")} }
            }
        }
    }
}

/// The empty inspector: shown until a row is chosen.
#[component]
pub(super) fn NoSelection(message: String) -> Element {
    rsx! {
        div { class: "ik-cons-pane",
            div { class: "ik-empty", "{message}" }
        }
    }
}

/// A search field for a list pane's sticky header, with its live hit count.
#[component]
pub(super) fn ListSearch(
    placeholder: String,
    query: Signal<String>,
    /// Already-worded, e.g. "4 hits" — the caller knows what it is counting.
    hits: String,
) -> Element {
    let mut query = query;
    rsx! {
        div { class: "ik-flex", style: "gap:8px;background:var(--surface);border:1px solid var(--border-ctl);border-radius:9px;padding:7px 10px;",
            span { style: "display:flex;color:var(--faint);flex:none;",
                Ic { icon: Icon::Search, size: 14 }
            }
            input {
                r#type: "search",
                style: "flex:1;min-width:0;background:none;border:none;outline:none;color:var(--text);font:inherit;font-size:12.5px;",
                placeholder: "{placeholder}",
                "aria-label": "{placeholder}",
                value: "{query}",
                oninput: move |event| query.set(event.value()),
            }
            span { class: "ik-mono", style: "font-size:11px;color:var(--faint);flex:none;", "{hits}" }
        }
    }
}
