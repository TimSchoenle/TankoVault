//! Appearance panel (`DESIGN_SPEC` §8) — theme, accent, density and cover style.
//!
//! Every knob is real: each writes a `data-*` attribute on `<html>` that swaps a block of CSS
//! custom properties, and persists a `tv-*` key that `index.html` re-applies before the next
//! first paint. Selecting a default clears the override so the stylesheet's own value wins.

use super::PanelCard;
use crate::i18n::{use_i18n, LOCALES};
use crate::icons::Icon;
use crate::state::prefs::{Knob, ACCENT, COVER, DENSITY, THEME};
use dioxus::prelude::*;

#[component]
pub(crate) fn AppearancePanel() -> Element {
    let i18n = use_i18n();
    let theme = use_signal(|| THEME.default.to_owned());
    let accent = use_signal(|| ACCENT.default.to_owned());
    let density = use_signal(|| DENSITY.default.to_owned());
    let cover = use_signal(|| COVER.default.to_owned());

    // Seed each control from what is actually applied, so the panel opens showing the
    // reader's real settings rather than the defaults.
    use_effect(move || {
        THEME.load(theme);
        ACCENT.load(accent);
        DENSITY.load(density);
        COVER.load(cover);
    });

    rsx! {
        PanelCard { icon: Icon::Settings, title: i18n.t("account.appearance.title"),
            p { class: "ik-muted", style: "font-size:13px;margin-top:0;",
                {i18n.t("account.appearance.intro")}
            }
            LanguageGroup {}
            KnobGroup {
                title: i18n.t("account.appearance.theme"),
                knob: THEME,
                value: theme,
                options: vec![
                    ("dark", i18n.t("account.appearance.themeOption.dark")),
                    ("light", i18n.t("account.appearance.themeOption.light")),
                ],
            }
            KnobGroup {
                title: i18n.t("account.appearance.accent"),
                knob: ACCENT,
                value: accent,
                options: vec![
                    ("vermilion", i18n.t("account.appearance.accentOption.vermilion")),
                    ("amber", i18n.t("account.appearance.accentOption.amber")),
                    ("jade", i18n.t("account.appearance.accentOption.jade")),
                    ("azure", i18n.t("account.appearance.accentOption.azure")),
                    ("amethyst", i18n.t("account.appearance.accentOption.amethyst")),
                ],
            }
            KnobGroup {
                title: i18n.t("account.appearance.density"),
                knob: DENSITY,
                value: density,
                options: vec![
                    ("cozy", i18n.t("account.appearance.densityOption.cozy")),
                    ("standard", i18n.t("account.appearance.densityOption.standard")),
                    ("compact", i18n.t("account.appearance.densityOption.compact")),
                ],
            }
            KnobGroup {
                title: i18n.t("account.appearance.cover"),
                knob: COVER,
                value: cover,
                options: vec![
                    ("ink", i18n.t("account.appearance.coverOption.ink")),
                    ("duotone", i18n.t("account.appearance.coverOption.duotone")),
                    ("vivid", i18n.t("account.appearance.coverOption.vivid")),
                ],
            }
        }
    }
}

/// The language picker.
///
/// Sits alongside the appearance knobs because it is the same kind of choice — a per-device
/// presentation preference — but it is not a [`Knob`]: the value is persisted by the `i18nrs`
/// provider rather than by a `data-*` attribute on `<html>`, and the options are whichever
/// catalogues are shipped rather than a fixed list.
#[component]
fn LanguageGroup() -> Element {
    let i18n = use_i18n();
    let current = i18n.language();
    rsx! {
        div { style: "margin-top:16px;",
            div { class: "ik-subhead", style: "margin-bottom:8px;",
                {i18n.t("account.appearance.language")}
            }
            div { class: "ik-chips", style: "margin-bottom:0;",
                for locale in LOCALES {
                    button {
                        key: "{locale.code}",
                        class: if current == locale.code { "ik-chip active" } else { "ik-chip" },
                        "aria-pressed": current == locale.code,
                        // Each option is written in its own language, so tag it as such: the
                        // page's `lang` cannot be right for all of them at once.
                        lang: locale.code,
                        onclick: move |_| i18n.set_language(locale.code),
                        "{locale.endonym}"
                    }
                }
            }
        }
    }
}

/// One labelled row of mutually-exclusive appearance chips. Both `title` and the option labels
/// arrive already resolved.
#[component]
fn KnobGroup(
    title: String,
    knob: Knob,
    value: Signal<String>,
    options: Vec<(&'static str, String)>,
) -> Element {
    let current = value.read().clone();
    rsx! {
        div { style: "margin-top:16px;",
            div { class: "ik-subhead", style: "margin-bottom:8px;", "{title}" }
            div { class: "ik-chips", style: "margin-bottom:0;",
                for (option , label) in options {
                    button {
                        key: "{option}",
                        class: if current == option { "ik-chip active" } else { "ik-chip" },
                        "aria-pressed": current == option,
                        onclick: move |_| knob.apply(value, option),
                        "{label}"
                    }
                }
            }
        }
    }
}
