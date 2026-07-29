//! Form primitives.
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
    #[props(default)] hint: Option<String>,
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
