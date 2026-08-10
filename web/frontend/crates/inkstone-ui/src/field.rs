//! Form controls. Every one of them is *controlled* — the caller owns the value and receives
//! edits — because the screens using this kit keep their state in a URL or a signal that other
//! panels read, and a control holding a second copy is how the two drift apart.

use crate::skin::{use_skin, Flag, Part, Variant};
use dioxus::prelude::*;

/// The label / control / hint / error frame every field shares.
///
/// Separate from [`TextInput`] so a caller can put anything inside it — a pair of controls, a
/// third-party widget — and still get the label association and error wiring right.
#[component]
pub fn FieldShell(
    /// Must be unique on the page: it binds the label to the control and the control to its
    /// error text.
    id: String,
    label: String,
    #[props(default)] hint: Option<String>,
    /// Rendered in the accent colour under the control, and announced via `aria-describedby`.
    #[props(default)]
    error: Option<String>,
    #[props(default)] class: String,
    children: Element,
) -> Element {
    let skin = use_skin();
    let class = skin.class_with(Part::Field, &[], &class);
    rsx! {
        div { class,
            label { r#for: "{id}", "{label}" }
            {children}
            if let Some(error) = error {
                div {
                    id: "{id}-error",
                    class: skin.class(Part::FieldError, &[]),
                    role: "alert",
                    "{error}"
                }
            } else if let Some(hint) = hint {
                div { id: "{id}-hint", class: skin.class(Part::FieldHint, &[]), "{hint}" }
            }
        }
    }
}

/// The `aria-describedby` target for a field, so the hint or error is read out with the control.
fn described_by(id: &str, hint: Option<&String>, error: Option<&String>) -> String {
    if error.is_some() {
        format!("{id}-error")
    } else if hint.is_some() {
        format!("{id}-hint")
    } else {
        String::new()
    }
}

/// A labelled single-line text input.
#[component]
pub fn TextInput(
    id: String,
    label: String,
    value: String,
    on_input: EventHandler<String>,
    /// The `type` attribute: `text`, `email`, `password`, `number`, `url`.
    #[props(default = "text".to_string())]
    r#type: String,
    /// The `autocomplete` token. Worth setting on every credential field: `username`, `email`,
    /// `current-password`, `new-password`.
    #[props(default)]
    autocomplete: Option<String>,
    #[props(default)] placeholder: Option<String>,
    #[props(default)] hint: Option<String>,
    #[props(default)] error: Option<String>,
    #[props(default = false)] disabled: bool,
    #[props(default = false)] required: bool,
    /// Rendered in the monospace face, for keys, hashes and identifiers.
    #[props(default = false)]
    mono: bool,
    /// Submit affordance: these controls are not inside a `<form>`, so Enter does nothing unless
    /// the caller says what it means.
    #[props(default)]
    on_enter: Option<EventHandler<()>>,
    /// Take focus as soon as the field appears. The HTML attribute is ignored by browsers on an
    /// element inserted after load, so this is done from `onmounted`.
    #[props(default = false)]
    autofocus: bool,
) -> Element {
    let described = described_by(&id, hint.as_ref(), error.as_ref());
    let invalid = error.is_some();
    let class = use_skin().class(Part::Input, &[Variant::flag(mono, Flag::Mono)]);
    rsx! {
        FieldShell { id: id.clone(), label, hint, error,
            input {
                id: "{id}",
                class,
                r#type: "{r#type}",
                value: "{value}",
                disabled,
                required,
                autocomplete: autocomplete.clone().unwrap_or_default(),
                placeholder: placeholder.clone().unwrap_or_default(),
                "aria-invalid": if invalid { "true" } else { "false" },
                "aria-describedby": "{described}",
                onmounted: move |event| {
                    if autofocus {
                        let element = event.data();
                        spawn(async move {
                            let _ = element.set_focus(true).await;
                        });
                    }
                },
                oninput: move |event| on_input.call(event.value()),
                onkeydown: move |event| {
                    if event.key() == Key::Enter {
                        if let Some(handler) = &on_enter {
                            handler.call(());
                        }
                    }
                },
            }
        }
    }
}

/// A labelled multi-line input.
#[component]
pub fn TextArea(
    id: String,
    label: String,
    value: String,
    on_input: EventHandler<String>,
    #[props(default = 4)] rows: u32,
    #[props(default)] placeholder: Option<String>,
    #[props(default)] hint: Option<String>,
    #[props(default)] error: Option<String>,
    #[props(default = false)] disabled: bool,
    #[props(default = false)] mono: bool,
) -> Element {
    let described = described_by(&id, hint.as_ref(), error.as_ref());
    let class = use_skin().class(Part::Input, &[Variant::flag(mono, Flag::Mono)]);
    rsx! {
        FieldShell { id: id.clone(), label, hint, error,
            textarea {
                id: "{id}",
                class,
                rows: "{rows}",
                disabled,
                placeholder: placeholder.clone().unwrap_or_default(),
                "aria-describedby": "{described}",
                oninput: move |event| on_input.call(event.value()),
                "{value}"
            }
        }
    }
}

/// A labelled `<select>` over `(value, label)` pairs.
#[component]
pub fn SelectField(
    id: String,
    label: String,
    options: Vec<SegOption>,
    selected: String,
    on_select: EventHandler<String>,
    #[props(default)] hint: Option<String>,
    #[props(default)] error: Option<String>,
    #[props(default = false)] disabled: bool,
) -> Element {
    let described = described_by(&id, hint.as_ref(), error.as_ref());
    let class = use_skin().class(Part::Select, &[]);
    rsx! {
        FieldShell { id: id.clone(), label, hint, error,
            select {
                id: "{id}",
                class,
                disabled,
                value: "{selected}",
                "aria-describedby": "{described}",
                onchange: move |event| on_select.call(event.value()),
                for (value , text) in options {
                    option { key: "{value}", value: "{value}", selected: value == selected, "{text}" }
                }
            }
        }
    }
}

/// One option in a [`SegControl`] or [`SelectField`]: the value written back, and the word for it.
pub type SegOption = (String, String);

/// A segmented control — a small closed set of choices with exactly one lit.
#[component]
pub fn SegControl(
    options: Vec<SegOption>,
    selected: String,
    on_select: EventHandler<String>,
    /// Names the group for a screen reader, which otherwise announces a bare set of buttons.
    #[props(default)]
    label: Option<String>,
    /// Read-only: the choice is shown but cannot be changed. Genuinely `disabled`, not dimmed —
    /// a dimmed-but-live radio is worse than one that refuses the click.
    #[props(default = false)]
    disabled: bool,
) -> Element {
    let skin = use_skin();
    rsx! {
        div {
            class: skin.class(Part::Seg, &[]),
            role: "radiogroup",
            "aria-label": label.clone().unwrap_or_default(),
            for (value , text) in options {
                button {
                    key: "{value}",
                    class: skin.class(Part::SegItem, &[Variant::flag(value == selected, Flag::Selected)]),
                    r#type: "button",
                    role: "radio",
                    disabled,
                    "aria-checked": if value == selected { "true" } else { "false" },
                    onclick: move |_| on_select.call(value.clone()),
                    "{text}"
                }
            }
        }
    }
}

/// A labelled checkbox with its description beside it.
#[component]
pub fn Checkbox(
    id: String,
    label: String,
    checked: bool,
    on_toggle: EventHandler<bool>,
    #[props(default)] hint: Option<String>,
    #[props(default = false)] disabled: bool,
) -> Element {
    let skin = use_skin();
    let described = if hint.is_some() {
        format!("{id}-hint")
    } else {
        String::new()
    };
    rsx! {
        div { class: skin.class(Part::Checkbox, &[]),
            input {
                id: "{id}",
                r#type: "checkbox",
                checked,
                disabled,
                "aria-describedby": "{described}",
                onchange: move |event| on_toggle.call(event.checked()),
            }
            div {
                label { r#for: "{id}", "{label}" }
                if let Some(hint) = hint {
                    div { id: "{id}-hint", class: skin.class(Part::FieldHint, &[]), "{hint}" }
                }
            }
        }
    }
}

/// A labelled slider with its value pinned to the right in a fixed-width cell, so a row of them
/// keeps one alignment while the numbers change.
#[component]
pub fn SliderRow(
    id: String,
    label: String,
    /// The raw slider position. Sliders are integer-stepped; a caller scaling to a fraction owns
    /// the conversion, as it owns `display`.
    value: f64,
    min: f64,
    max: f64,
    step: f64,
    /// Pre-formatted, because a value's unit belongs to the caller (`0.8`, `2`, `900ms`).
    display: String,
    on_input: EventHandler<f64>,
    #[props(default = false)] disabled: bool,
) -> Element {
    let skin = use_skin();
    rsx! {
        div { class: skin.class(Part::SliderRow, &[]),
            label { class: skin.class(Part::SliderLabel, &[]), r#for: "{id}", "{label}" }
            input {
                id: "{id}",
                class: skin.class(Part::Range, &[Variant::Flag(Flag::Grow)]),
                r#type: "range",
                min: "{min}",
                max: "{max}",
                step: "{step}",
                value: "{value}",
                disabled,
                "aria-valuetext": "{display}",
                oninput: move |event| {
                    if let Ok(parsed) = event.value().parse::<f64>() {
                        on_input.call(parsed);
                    }
                },
            }
            span { class: skin.class(Part::SliderValue, &[]), "{display}" }
        }
    }
}

/// A search field for a list pane's sticky header, with an optional live hit count.
#[component]
pub fn SearchField(
    placeholder: String,
    query: String,
    on_input: EventHandler<String>,
    /// A glyph shown at the leading edge. The kit ships no icons, so this is the app's.
    #[props(default)]
    icon: Option<Element>,
    /// Already-worded, e.g. "4 hits" — the caller knows what it is counting.
    #[props(default)]
    hits: Option<String>,
) -> Element {
    let skin = use_skin();
    rsx! {
        div { class: skin.class(Part::SearchField, &[]),
            if let Some(icon) = icon {
                span { class: skin.class(Part::SearchIcon, &[]), {icon} }
            }
            input {
                r#type: "search",
                class: skin.class(Part::SearchInput, &[]),
                placeholder: "{placeholder}",
                "aria-label": "{placeholder}",
                value: "{query}",
                oninput: move |event| on_input.call(event.value()),
            }
            if let Some(hits) = hits {
                span { class: skin.class(Part::SearchHits, &[]), "{hits}" }
            }
        }
    }
}

/// A labelled text input with no `FieldShell` around it — the bare control, for a toolbar.
#[component]
pub fn Field(
    id: String,
    label: String,
    value: String,
    on_input: EventHandler<String>,
    #[props(default = "text".to_string())] r#type: String,
    #[props(default)] placeholder: Option<String>,
    #[props(default = false)] mono: bool,
    #[props(default)] on_enter: Option<EventHandler<()>>,
) -> Element {
    let class = use_skin().class(Part::Input, &[Variant::flag(mono, Flag::Mono)]);
    rsx! {
        input {
            id: "{id}",
            class,
            r#type: "{r#type}",
            value: "{value}",
            "aria-label": "{label}",
            placeholder: placeholder.clone().unwrap_or_default(),
            oninput: move |event| on_input.call(event.value()),
            onkeydown: move |event| {
                if event.key() == Key::Enter {
                    if let Some(handler) = &on_enter {
                        handler.call(());
                    }
                }
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The hint and the error share one `aria-describedby` slot, and the error must win: a field
    /// that failed validation announcing only its hint tells the reader nothing about the
    /// failure.
    #[test]
    fn error_replaces_hint_in_the_description() {
        let hint = "we never show this".to_string();
        let error = "too short".to_string();
        assert_eq!(described_by("pw", Some(&hint), Some(&error)), "pw-error");
        assert_eq!(described_by("pw", Some(&hint), None), "pw-hint");
        assert_eq!(described_by("pw", None, None), "");
    }
}
