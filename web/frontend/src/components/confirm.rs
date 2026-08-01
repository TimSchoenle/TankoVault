//! Two-tier confirmation for destructive actions: [`InlineConfirm`] for reversible ones, and
//! [`TypeToConfirm`] for irreversible ones requiring an exact-match retype.

use crate::i18n::use_i18n;
use dioxus::prelude::*;

/// A reversible destructive action: state the consequence, ask once more, act.
#[component]
pub(crate) fn InlineConfirm(
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
pub(crate) fn TypeToConfirm(
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
