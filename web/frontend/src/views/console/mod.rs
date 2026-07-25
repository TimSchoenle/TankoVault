//! Operator Console (§17.2.7, RBAC) — the admin control surface, one module per tab:
//!
//! - [`overview`] + [`stats`] — system KPIs and the per-provider statistics table;
//! - [`scans`] — trigger runs, watch progress, triage task failures;
//! - [`providers`] — full provider lifecycle (create, edit, state, scan, dry-run, delete);
//! - [`solver`] — challenge/solver health and the standalone adapter test;
//! - [`merge`] — the canonicalisation review queue;
//! - [`sync`] — linked accounts, per-series mappings and the matching backlogs;
//! - [`users`] — the registered-user directory;
//! - [`audit`] — the privileged-action trail.
//!
//! The read-only panels all refetch from one shared, pausable [`RefreshTick`]. Every mutating
//! call is RBAC-gated server-side (create/delete require Admin; the rest require Operator) —
//! hiding a control here is a convenience, never the security boundary.

mod audit;
mod controls;
mod merge;
mod overview;
mod providers;
mod scans;
mod solver;
mod stats;
mod sync;
mod users;

use crate::i18n::use_i18n;
use crate::icons::{Ic, Icon};
use crate::models::*;
use crate::state::use_session;
use dioxus::prelude::*;

/// Auto-refresh cadence for the read-only dashboard panels.
const REFRESH_MS: u32 = 4000;

/// The shared refetch signal for every auto-refreshing console panel.
///
/// One tick drives them all, so the whole dashboard is consistent at each cadence instead of
/// each panel drifting on its own timer — and pausing is a single switch rather than nine.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct RefreshTick(Signal<u32>);

impl RefreshTick {
    /// Subscribe the calling reactive scope, so it refetches on the next tick.
    pub(super) fn track(self) {
        let _ = self.0.read();
    }

    /// Advance the tick, refetching every panel that tracks it.
    pub(super) fn bump(mut self) {
        self.0 += 1;
    }
}

/// The operator console's top-level tabs (`DESIGN_SPEC` §7.8), in order.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ConsoleTab {
    Overview,
    LiveScans,
    Providers,
    Solver,
    AdapterTest,
    Merge,
    Sync,
    Users,
    Audit,
}

impl ConsoleTab {
    const ALL: [ConsoleTab; 9] = [
        Self::Overview,
        Self::LiveScans,
        Self::Providers,
        Self::Solver,
        Self::AdapterTest,
        Self::Merge,
        Self::Sync,
        Self::Users,
        Self::Audit,
    ];
    /// The catalogue key of this tab's label (see [`crate::i18n`]).
    fn label_key(self) -> &'static str {
        match self {
            Self::Overview => "console.tab.overview",
            Self::LiveScans => "console.tab.liveScans",
            Self::Providers => "console.tab.providers",
            Self::Solver => "console.tab.solver",
            Self::AdapterTest => "console.tab.adapterTest",
            Self::Merge => "console.tab.merge",
            Self::Sync => "console.tab.sync",
            Self::Users => "console.tab.users",
            Self::Audit => "console.tab.audit",
        }
    }
    fn icon(self) -> Icon {
        match self {
            Self::Overview => Icon::Dashboard,
            Self::LiveScans => Icon::Radar,
            Self::Providers => Icon::Public,
            Self::Solver => Icon::ShieldLock,
            Self::AdapterTest => Icon::Code,
            Self::Merge => Icon::Merge,
            Self::Sync => Icon::CloudSync,
            Self::Users => Icon::Group,
            Self::Audit => Icon::History,
        }
    }
}

/// Selectable adapter implementations: the wire token and the catalogue key wording it.
/// Mirrors `AdapterKind`.
pub(super) const ADAPTER_KINDS: &[(&str, &str)] = &[
    ("generic_config", "console.adapterKind.genericConfig"),
    ("madara", "console.adapterKind.madara"),
    ("custom", "console.adapterKind.custom"),
];

/// The wire token for a loaded provider's adapter kind (matches the SQL enum / `ADAPTER_KINDS`).
pub(super) fn adapter_token(a: AdapterKind) -> &'static str {
    match a {
        AdapterKind::GenericConfig => "generic_config",
        AdapterKind::Madara => "madara",
        AdapterKind::Custom => "custom",
    }
}

#[component]
pub(crate) fn Console() -> Element {
    let session = use_session();
    let i18n = use_i18n();

    if !session.role.read().is_operator() {
        return rsx! {
            h1 { class: "ik-page-title", {i18n.t("nav.console")} }
            div { class: "ik-empty", {i18n.t("console.operatorsOnly")} }
        };
    }

    // One tick drives every read-only panel's refetch: the background loop bumps it on a
    // cadence while `auto` is on, and the Refresh control bumps it on demand.
    let tick = RefreshTick(use_signal(|| 0u32));
    let auto = use_signal(|| true);
    let mut tab = use_signal(|| ConsoleTab::Overview);
    use_future(move || async move {
        loop {
            gloo_timers::future::TimeoutFuture::new(REFRESH_MS).await;
            // `peek`, not `read`: this loop must not re-subscribe itself to `auto`, or
            // toggling the pause switch would restart the timer rather than gate it.
            if *auto.peek() {
                tick.bump();
            }
        }
    });

    let current = *tab.read();
    let panel = match current {
        ConsoleTab::Overview => rsx! {
            overview::SystemOverview { tick }
            stats::ProviderStatsTable { tick }
        },
        ConsoleTab::LiveScans => rsx! { scans::ScanQueue { tick } },
        ConsoleTab::Providers => rsx! { providers::ProvidersPanel {} },
        ConsoleTab::Solver => rsx! { solver::SolverPanel { tick } },
        ConsoleTab::AdapterTest => rsx! { solver::AdapterTestTab {} },
        ConsoleTab::Merge => rsx! { merge::MergeQueue {} },
        ConsoleTab::Sync => rsx! { sync::SyncAdminPanel {} },
        ConsoleTab::Users => rsx! { users::UsersPanel { tick } },
        ConsoleTab::Audit => rsx! { audit::AuditPanel { tick } },
    };

    rsx! {
        div { class: "ik-flex", style: "justify-content:space-between;align-items:center;flex-wrap:wrap;",
            div { class: "ik-flex", style: "gap:9px;",
                Ic { icon: Icon::Dashboard, size: 22 }
                h1 { class: "ik-page-title", style: "margin:0;", {i18n.t("console.title")} }
            }
            controls::LiveControls { tick, auto }
        }
        div { class: "ik-tabs", style: "margin-top:14px;",
            for t in ConsoleTab::ALL {
                button {
                    class: if current == t { "ik-tab active" } else { "ik-tab" },
                    style: "display:inline-flex;align-items:center;gap:6px;",
                    onclick: move |_| tab.set(t),
                    Ic { icon: t.icon(), size: 15 }
                    span { {i18n.t(t.label_key())} }
                }
            }
        }
        {panel}
    }
}

/// The pill class encoding a run state. The wording comes from
/// [`RunStateExt::label_key`](crate::models::RunStateExt::label_key), so the colour and the
/// text cannot drift apart into two different enumerations.
pub(super) fn run_state_pill(state: RunState) -> &'static str {
    match state {
        RunState::Completed => "ik-pill jade",
        RunState::Running => "ik-pill run",
        RunState::Queued | RunState::Cancelled => "ik-pill",
        RunState::Failed => "ik-pill vermilion",
    }
}

/// Build the politeness JSON payload from the editor's string fields, or the catalogue key of
/// the field that would not parse.
pub(super) fn politeness_json(
    rps: &str,
    concurrency: &str,
    crawl_delay_ms: &str,
    user_agent: &str,
) -> Result<serde_json::Value, &'static str> {
    let rps: f64 = rps.trim().parse().map_err(|_| "console.providers.badRps")?;
    let concurrency: u32 = concurrency
        .trim()
        .parse()
        .map_err(|_| "console.providers.badConcurrency")?;
    let crawl_delay_ms: u64 = crawl_delay_ms
        .trim()
        .parse()
        .map_err(|_| "console.providers.badCrawlDelay")?;
    Ok(serde_json::json!({
        "rps": rps,
        "concurrency": concurrency,
        "crawl_delay_ms": crawl_delay_ms,
        "user_agent": user_agent,
    }))
}

/// Pretty-print a stored adapter config for the editor textarea (empty / null → `{}`).
pub(super) fn config_editor_text(v: &serde_json::Value) -> String {
    if v.is_null() {
        "{}".to_owned()
    } else {
        serde_json::to_string_pretty(v).unwrap_or_else(|_| "{}".to_owned())
    }
}
