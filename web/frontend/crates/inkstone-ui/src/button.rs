//! The button, in the three shapes an app needs: with a label, with only a glyph, and as a
//! class string for anything that has to be an `<a>` (a router `Link`) rather than a `<button>`.

use crate::skin::{use_skin, Flag, Part, Variant};
use crate::tone::{Size, Tone};
use dioxus::prelude::*;

/// The `type` attribute. Defaults to [`ButtonType::Button`], which is what a control outside a
/// `<form>` wants — the HTML default is `submit`, and inside a form that reloads the page.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum ButtonType {
    #[default]
    Button,
    Submit,
}

impl ButtonType {
    fn attr(self) -> &'static str {
        match self {
            Self::Button => "button",
            Self::Submit => "submit",
        }
    }
}

/// The class string a button of this shape carries, under the skin in force.
///
/// Public because a navigation control has to be an anchor to be middle-clickable, so a router
/// `Link` cannot be a `<button>` and still needs to look like one. Call it during a render — it
/// reads the skin from context.
#[must_use]
pub fn button_class(tone: Tone, size: Size, block: bool) -> String {
    use_skin().class(
        Part::Button,
        &[
            Variant::Tone(tone),
            Variant::Size(size),
            Variant::flag(block, Flag::Block),
        ],
    )
}

/// `""` for an unset tri-state ARIA attribute, so it is omitted rather than rendered `false`.
fn bool_attr(on: bool) -> &'static str {
    if on {
        "true"
    } else {
        "false"
    }
}

/// A button.
///
/// `busy` is not the same as `disabled`: it disables the control *and* announces the wait, so a
/// screen reader is told why the click did nothing. Both are honoured, so a caller can pass
/// `disabled: nothing_selected, busy: request_in_flight` without resolving the two itself.
#[component]
pub fn Button(
    #[props(default)] tone: Tone,
    #[props(default)] size: Size,
    /// Fill the container and centre the label.
    #[props(default = false)]
    block: bool,
    #[props(default = false)] disabled: bool,
    /// An action is in flight: refuses input and sets `aria-busy`.
    #[props(default = false)]
    busy: bool,
    #[props(default)] r#type: ButtonType,
    /// A glyph before the label. `Element` rather than an icon enum — the kit ships no icons.
    #[props(default)]
    icon: Option<Element>,
    /// Native tooltip. Also the accessible name when there are no children (see [`IconButton`]).
    #[props(default)]
    title: Option<String>,
    #[props(default)] aria_label: Option<String>,
    /// Renders as pressed. For a control that toggles a state which stays on — usually
    /// [`ToggleButton`], which is that control with its lit tone already decided.
    #[props(default)]
    pressed: Option<bool>,
    /// Announces the region this button shows and hides.
    #[props(default)]
    expanded: Option<bool>,
    /// Extra classes, for the one-off a variant does not cover. Appended, never replacing.
    #[props(default)]
    class: String,
    #[props(default)] style: Option<String>,
    #[props(default)] on_click: Option<EventHandler<MouseEvent>>,
    children: Element,
) -> Element {
    let class = use_skin().class_with(
        Part::Button,
        &[
            Variant::Tone(tone),
            Variant::Size(size),
            Variant::flag(block, Flag::Block),
        ],
        &class,
    );
    rsx! {
        button {
            class,
            style: style.unwrap_or_default(),
            r#type: r#type.attr(),
            disabled: disabled || busy,
            "aria-busy": if busy { "true" } else { "false" },
            "aria-pressed": pressed.map(bool_attr).unwrap_or_default(),
            "aria-expanded": expanded.map(bool_attr).unwrap_or_default(),
            title: title.clone().unwrap_or_default(),
            "aria-label": aria_label.clone().unwrap_or_default(),
            onclick: move |event| {
                if let Some(handler) = &on_click {
                    handler.call(event);
                }
            },
            {icon}
            {children}
        }
    }
}

/// A button that lights up while the state it controls is on.
///
/// The convention it fixes: the screens this replaced each decided for themselves what "on"
/// looks like — `primary`, `acc`, or `active`, and `active` was never a rule, so one row of
/// these had no visible selection at all. Here the lit tone is a prop with a default, and
/// `aria-pressed` is not optional.
#[component]
pub fn ToggleButton(
    on: bool,
    on_toggle: EventHandler<MouseEvent>,
    /// How the button looks while lit.
    #[props(default = Tone::Primary)]
    on_tone: Tone,
    #[props(default)] size: Size,
    #[props(default = false)] disabled: bool,
    #[props(default = false)] busy: bool,
    #[props(default)] icon: Option<Element>,
    #[props(default)] title: Option<String>,
    #[props(default)] class: String,
    #[props(default)] style: Option<String>,
    children: Element,
) -> Element {
    rsx! {
        Button {
            tone: if on { on_tone } else { Tone::Neutral },
            size,
            disabled,
            busy,
            icon,
            title,
            class,
            style,
            pressed: on,
            on_click: move |event| on_toggle.call(event),
            {children}
        }
    }
}

/// A button whose whole content is a glyph.
///
/// `label` is mandatory and becomes both the tooltip and the accessible name, because a button
/// with no text has neither otherwise — the commonest accessibility defect in the screens this
/// kit replaced.
#[component]
pub fn IconButton(
    icon: Element,
    label: String,
    #[props(default = Tone::Bare)] tone: Tone,
    #[props(default = Size::Xs)] size: Size,
    #[props(default = false)] disabled: bool,
    #[props(default = false)] busy: bool,
    /// Renders as pressed, for a control that toggles a state that stays on.
    #[props(default)]
    pressed: Option<bool>,
    #[props(default)] class: String,
    #[props(default)] on_click: Option<EventHandler<MouseEvent>>,
) -> Element {
    let class = use_skin().class_with(
        Part::IconButton,
        &[Variant::Tone(tone), Variant::Size(size)],
        &class,
    );
    rsx! {
        button {
            class,
            r#type: "button",
            disabled: disabled || busy,
            title: "{label}",
            "aria-label": "{label}",
            "aria-pressed": pressed.map(bool_attr).unwrap_or_default(),
            onclick: move |event| {
                if let Some(handler) = &on_click {
                    handler.call(event);
                }
            },
            {icon}
        }
    }
}
