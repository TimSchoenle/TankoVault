//! Form primitives: the labelled text input, the segmented control, the slider row and the
//! list-pane search field.
//!
//! The last three were `pub(super)` inside `views/console/shell.rs` and so unreachable from any
//! other view tree. They are ordinary form controls; nothing about them is console-specific.
//!
//! The console screens already paired every `<label>` with a matching `r#for`/`id`; the auth
//! screens — the one surface every single user must pass through — did not. Seven inputs
//! across sign-in, registration and password reset had a label that was a *sibling* rather
//! than an ancestor and carried no `for`, so there was not even an implicit association to
//! fall back on: a screen reader announced them as "edit text, blank".
//!
//! Nor was there any `autocomplete`, so password managers could not reliably fill or offer to
//! save credentials, and Enter-to-submit was hand-wired per input.
//!
//! Extracting the component that implicitly existed makes all of that structural rather than
//! per-site discipline.

use crate::icons::{Ic, Icon};
use dioxus::prelude::*;

/// A labelled text input.
///
/// `id` must be unique on the page — it is what binds the label to the control.
#[component]
pub(crate) fn Field(
    id: String,
    label: String,
    /// The `type` attribute. `"text"` unless given.
    #[props(default = "text".to_string())]
    kind: String,
    /// The `autocomplete` token. Worth setting on every credential field: `username`,
    /// `email`, `current-password`, `new-password`.
    #[props(default)]
    autocomplete: Option<String>,
    #[props(default)] placeholder: Option<String>,
    value: String,
    on_input: EventHandler<String>,
    /// Submit affordance. Enter in a single-input form is the expected behaviour and does not
    /// come free here, because these are not inside a `<form>`.
    #[props(default)]
    on_enter: Option<EventHandler<()>>,
    /// Rendered under the input, for the "why is this needed" note.
    #[props(default)]
    hint: Option<String>,
) -> Element {
    rsx! {
        div { class: "ik-field",
            label { r#for: "{id}", "{label}" }
            input {
                id: "{id}",
                class: "ik-input",
                r#type: "{kind}",
                autocomplete: autocomplete.clone().unwrap_or_default(),
                placeholder: placeholder.clone().unwrap_or_default(),
                value: "{value}",
                oninput: move |e| on_input.call(e.value()),
                onkeydown: move |e| {
                    if e.key() == Key::Enter {
                        if let Some(handler) = &on_enter {
                            handler.call(());
                        }
                    }
                },
            }
            if let Some(hint) = hint {
                div { class: "ik-muted", style: "font-size:12px;margin-top:6px;", "{hint}" }
            }
        }
    }
}

/// One option in a [`SegControl`]: the value written back, and the word for it.
pub(crate) type SegOption = (String, String);

/// A segmented control — a small closed set of choices with exactly one lit.
#[component]
pub(crate) fn SegControl(
    options: Vec<SegOption>,
    selected: String,
    /// Read-only mode: the choice is shown but cannot be changed. Genuinely `disabled`, not
    /// dimmed — a dimmed-but-live radio is worse than one that refuses the click.
    #[props(default = false)]
    disabled: bool,
    on_select: EventHandler<String>,
) -> Element {
    rsx! {
        div { class: "ik-seg", role: "radiogroup",
            for (value , label) in options {
                button {
                    key: "{value}",
                    class: if value == selected { "on" } else { "" },
                    r#type: "button",
                    role: "radio",
                    disabled,
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
pub(crate) fn SliderRow(
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

/// A search field for a list pane's sticky header, with its live hit count.
#[component]
pub(crate) fn ListSearch(
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
