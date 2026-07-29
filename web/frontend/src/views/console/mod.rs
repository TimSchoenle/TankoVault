//! Operator Console (§17.2.7) — the admin control surface, one module per tab:
//!
//! - [`overview`] + [`stats`] — system KPIs and the per-provider statistics table;
//! - [`scans`] — trigger runs, watch progress, triage task failures;
//! - [`providers`] — full provider lifecycle (create, edit, state, scan, dry-run, delete);
//! - [`solver`] — challenge/solver health and the standalone adapter test;
//! - [`merge`] — the canonicalisation review queue;
//! - [`sync`] — linked accounts, per-series mappings and the matching backlogs;
//! - [`users`] — user administration: directory, identity, suspension, permission grants;
//! - [`flags`] — the feature-flag control plane;
//! - [`privacy`] — the GDPR data-subject request queue;
//! - [`audit`] — the privileged-action trail.
//!
//! The read-only panels all refetch from one shared, pausable [`RefreshTick`].
//!
//! # Which tabs a reader sees
//!
//! Each tab declares the permission that opens it, and only the tabs a reader actually holds
//! are rendered — so someone granted nothing but `merge.read` gets a console with one tab in it
//! rather than nine that mostly 403. Tabs whose *feature* is switched off are hidden too, for
//! the same reason: a tab whose endpoints answer 404 is worse than no tab.
//!
//! Every one of those checks is a convenience. The server authorizes each call independently
//! and hiding a control has never been the boundary; what this does buy is that a reader is not
//! shown work they cannot do.

mod audit;
mod controls;
mod flags;
mod merge;
mod overview;
mod privacy;
mod providers;
mod scans;
mod solver;
mod stats;
mod sync;
mod users;

use crate::i18n::use_i18n;
use crate::icons::{Ic, Icon};
use crate::models::*;
use crate::state::capabilities::{use_capabilities, CapabilitySet};
use crate::wire::types::{Feature, Permission};
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
    Flags,
    Privacy,
    Audit,
}

impl ConsoleTab {
    const ALL: [ConsoleTab; 11] = [
        Self::Overview,
        Self::LiveScans,
        Self::Providers,
        Self::Solver,
        Self::AdapterTest,
        Self::Merge,
        Self::Sync,
        Self::Users,
        Self::Flags,
        Self::Privacy,
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
            Self::Flags => "console.tab.flags",
            Self::Privacy => "console.tab.privacy",
            Self::Audit => "console.tab.audit",
        }
    }
    fn icon(self) -> Icon {
        match self {
            Self::Overview => Icon::Dashboard,
            Self::LiveScans => Icon::Radar,
            Self::Providers => Icon::Public,
            Self::Solver | Self::Privacy => Icon::ShieldLock,
            Self::AdapterTest => Icon::Code,
            Self::Merge => Icon::Merge,
            Self::Sync => Icon::CloudSync,
            Self::Users => Icon::Group,
            Self::Flags => Icon::Settings,
            Self::Audit => Icon::History,
        }
    }

    /// The permission that opens this tab, and the feature that has to be switched on for it to
    /// be worth opening.
    ///
    /// Both stated in one place so adding a tab means answering both questions at once — the
    /// alternative is a tab that renders for everyone, or one whose panel loads and then 404s.
    fn requires(self) -> (Permission, Feature) {
        match self {
            Self::Overview => (Permission::SystemStats, Feature::AdminStats),
            Self::LiveScans => (Permission::ScansRead, Feature::ScanningManual),
            // Solver health is provider health seen from the fetch pipeline's side: same data,
            // same permission, same feature — it is a second view rather than a second surface.
            Self::Providers | Self::Solver => (Permission::ProvidersRead, Feature::AdminProviders),
            Self::AdapterTest => (Permission::ProvidersTest, Feature::AdminAdapterTest),
            Self::Merge => (Permission::MergeRead, Feature::ScanningMergeQueue),
            Self::Sync => (Permission::SyncAdminRead, Feature::AdminSync),
            Self::Users => (Permission::UsersRead, Feature::AdminUsers),
            Self::Flags => (Permission::FlagsRead, Feature::AdminFeatureFlags),
            Self::Privacy => (Permission::PrivacyRead, Feature::PrivacyRequests),
            Self::Audit => (Permission::AuditRead, Feature::AdminAudit),
        }
    }

    /// Whether this reader should be offered this tab at all.
    fn is_visible(self, caps: &CapabilitySet) -> bool {
        let (permission, feature) = self.requires();
        caps.can(permission) && caps.has_feature(feature)
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
    let i18n = use_i18n();
    let caps = use_capabilities();

    // Held back until the capability fetch lands: rendering "operators only" first and the
    // console a moment later reads as a permission error to anyone who blinks.
    if !caps.is_ready() {
        return rsx! {
            h1 { class: "ik-page-title", {i18n.t("nav.console")} }
            crate::components::SkeletonBlock { height: 220 }
        };
    }

    let visible: Vec<ConsoleTab> = ConsoleTab::ALL
        .into_iter()
        .filter(|t| t.is_visible(&caps))
        .collect();
    let Some(&first) = visible.first() else {
        return rsx! {
            h1 { class: "ik-page-title", {i18n.t("nav.console")} }
            div { class: "ik-empty", {i18n.t("console.operatorsOnly")} }
        };
    };

    // One tick drives every read-only panel's refetch: the background loop bumps it on a
    // cadence while `auto` is on, and the Refresh control bumps it on demand.
    let tick = RefreshTick(use_signal(|| 0u32));
    let auto = use_signal(|| true);
    let mut tab = use_signal(|| first);
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

    // The selected tab can stop being visible under the reader's feet — a permission revoked, a
    // feature switched off — in which case fall back to the first one they still have rather
    // than rendering a panel whose every call now fails.
    let current = {
        let selected = *tab.read();
        if visible.contains(&selected) {
            selected
        } else {
            first
        }
    };

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
        ConsoleTab::Flags => rsx! { flags::FeatureFlagsPanel {} },
        ConsoleTab::Privacy => rsx! { privacy::PrivacyQueuePanel { tick } },
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
            for t in visible.iter().copied() {
                button {
                    key: "{t.label_key()}",
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
///
/// `emulation` is the wire token of a browser profile, or empty for "no emulation" — which
/// must be sent as an explicit `null`, since omitting the key would let the server-side
/// serde default put the provider back on Chrome.
pub(super) fn politeness_json(
    rps: &str,
    concurrency: &str,
    crawl_delay_ms: &str,
    user_agent: &str,
    emulation: &str,
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
        "emulation": if emulation.is_empty() {
            serde_json::Value::Null
        } else {
            serde_json::Value::String(emulation.to_owned())
        },
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
