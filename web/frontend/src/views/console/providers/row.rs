//! One provider in the list pane: name, state, a mono meta line and the healthy-links meter.

use crate::components::HealthPill;
use crate::i18n::use_i18n;
use crate::models::{Provider, ProviderId, ProviderStat, ProviderState};
use crate::util::{rel_time, thousands};
use dioxus::prelude::*;

/// One provider in the list: name, state, a mono meta line and the healthy-links meter.
#[component]
pub(super) fn ProviderRow(
    provider: Provider,
    stat: Option<ProviderStat>,
    selected: bool,
    on_pick: EventHandler<ProviderId>,
) -> Element {
    let i18n = use_i18n();
    let id = provider.id;
    let disabled = provider.state == ProviderState::Disabled;
    let healthy = stat.as_ref().and_then(healthy_percent);

    let meta = match &stat {
        Some(stat) => i18n.args(
            "console.providers.rowMeta",
            &[
                ("series", &thousands(stat.series_count)),
                (
                    "healthy",
                    &healthy.map_or_else(|| "—".to_owned(), |p| format!("{p:.0}")),
                ),
                ("when", &rel_time(i18n, stat.last_scanned_at.as_deref())),
            ],
        ),
        None => provider.base_url.clone(),
    };

    let class = match (selected, disabled) {
        (true, _) => "ik-cons-row selected",
        (false, true) => "ik-cons-row dim",
        (false, false) => "ik-cons-row",
    };

    rsx! {
        button {
            class: "{class}",
            "aria-current": if selected { "true" } else { "false" },
            onclick: move |_| on_pick.call(id),
            div { class: "ik-flex", style: "gap:8px;",
                span { style: "font-weight:600;font-size:13.5px;", "{provider.name}" }
                HealthPill { state: Some(provider.state) }
            }
            div { class: "ik-mono", style: "font-size:10.5px;color:var(--muted);margin-top:3px;word-break:break-word;",
                "{meta}"
            }
            if let Some(percent) = healthy {
                div { class: "ik-bar",
                    span {
                        class: if percent >= 95.0 { "" } else { "warn" },
                        style: "width:{percent}%;",
                    }
                }
            }
        }
    }
}

/// Share of this provider's source links that are in a serving state.
///
/// This is what the API actually measures. The design calls the meter "solve %"; there is no
/// challenge-solve ratio on the wire, and inventing one would put a number on the screen that
/// nothing computes.
pub(super) fn healthy_percent(stat: &ProviderStat) -> Option<f64> {
    if stat.source_count <= 0 {
        return None;
    }
    let serving = (stat.source_count - stat.blocked_sources).max(0);
    // Both counts are row totals, far inside `f64`'s exact integer range.
    #[allow(clippy::cast_precision_loss)]
    Some((serving as f64 / stat.source_count as f64) * 100.0)
}
