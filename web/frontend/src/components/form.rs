//! Form primitives, flavoured for this app: the kit's controls with this app's icons and its
//! id conventions filled in.

use crate::icons::{Ic, Icon};
use dioxus::prelude::*;

pub(crate) use inkstone_ui::SegControl;

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
    /// A validation failure. Replaces the hint and is announced with the control.
    #[props(default)]
    error: Option<String>,
    /// Take focus as soon as the field appears. For the one field a dialog exists to collect —
    /// reaching for the mouse to answer a question that just took the screen is friction, and
    /// the HTML attribute is ignored by browsers on an element inserted after load.
    #[props(default = false)]
    autofocus: bool,
) -> Element {
    rsx! {
        inkstone_ui::TextInput {
            id,
            label,
            r#type: kind,
            autocomplete,
            placeholder,
            value,
            on_input,
            on_enter,
            hint,
            error,
            autofocus,
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
        inkstone_ui::SliderRow {
            id: format!("tv-slider-{label}"),
            label,
            value,
            min,
            max,
            step,
            display,
            on_input,
        }
    }
}

/// A search field for a list pane's sticky header, with its live hit count.
#[component]
pub(crate) fn ListSearch(
    placeholder: String,
    /// Controlled: the caller owns the text, because in the console it lives in the URL, and a
    /// signal here would hold a second copy of it.
    query: String,
    on_input: EventHandler<String>,
    /// Already-worded, e.g. "4 hits" — the caller knows what it is counting.
    hits: String,
) -> Element {
    rsx! {
        inkstone_ui::SearchField {
            placeholder,
            query,
            on_input,
            hits,
            icon: rsx! {
                Ic { icon: Icon::Search, size: 14 }
            },
        }
    }
}
