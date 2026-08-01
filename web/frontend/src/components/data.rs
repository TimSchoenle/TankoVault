//! Small read-only data displays: the KPI tile and the provider health pill.

use crate::i18n::use_i18n;
use crate::models::ProviderState;
use dioxus::prelude::*;

/// A single KPI tile: label, big value, and an optional supporting sub-line.
#[component]
pub(crate) fn Kpi(
    label: String,
    value: String,
    #[props(default)] sub: String,
    #[props(default)] accent: String,
    #[props(default = false)] large: bool,
) -> Element {
    let value_class = if large {
        format!("ik-kpi-value lg {accent}")
    } else {
        format!("ik-kpi-value {accent}")
    };
    rsx! {
        div { class: "ik-kpi",
            div { class: "ik-kpi-label", "{label}" }
            div { class: "{value_class}", "{value}" }
            if !sub.is_empty() {
                div { class: "ik-kpi-sub", "{sub}" }
            }
        }
    }
}

/// A provider's health as a coloured pill. `None` renders as "unknown" — the wire returned a
/// state this build has no word for.
#[component]
pub(crate) fn HealthPill(state: Option<ProviderState>) -> Element {
    let i18n = use_i18n();
    let class = match state {
        Some(ProviderState::Active) => "ik-pill jade",
        Some(ProviderState::Blocked | ProviderState::Disabled) => "ik-pill vermilion",
        _ => "ik-pill",
    };
    // The wire token doubles as the catalogue key, so the colour and the word cannot drift into
    // two separate enumerations.
    let label = state.map_or_else(
        || i18n.t("console.providerState.unknown"),
        |state| i18n.t(&format!("console.providerState.{state}")),
    );
    rsx! {
        span { class: "{class}", style: "font-size:9.5px;", "{label}" }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    /// `HealthPill` derives its catalogue key from `Display`, and `stats.rs` recovers the enum
    /// from the wire string with `FromStr`. Both are generated, so this pins that they agree —
    /// a mismatch would render `Key 'console.providerState.…' not found` to an operator.
    #[test]
    fn provider_state_tokens_round_trip() {
        for state in [
            ProviderState::Active,
            ProviderState::Degraded,
            ProviderState::Challenged,
            ProviderState::Solving,
            ProviderState::Blocked,
            ProviderState::Disabled,
        ] {
            assert_eq!(
                ProviderState::from_str(&state.to_string()).ok(),
                Some(state)
            );
        }
    }
}
