//! Operator Console — a master–detail application surface: an entity rail, an entity list, and
//! a deep inspector.
//!
//! ```text
//! ┌──────────────────────────────────────────────────────────┐
//! │ Console  / providers        ⌘K jump      Live · 4s       │  56px bar
//! ├────────────┬───────────────┬─────────────────────────────┤
//! │ entity     │ entity list   │ inspector                   │
//! │ rail 186px │ 328px         │ 1fr, two columns            │
//! └────────────┴───────────────┴─────────────────────────────┘
//! ```
//!
//! One module per entity:
//!
//! - [`overview`] + [`stats`] — system KPIs and the per-provider statistics table;
//! - [`scans`] — trigger runs, watch progress, triage task failures;
//! - [`providers`] — provider lifecycle, as a list + inspector;
//! - [`solver`] — challenge/solver health and the standalone adapter test;
//! - [`merge`] — the canonicalisation review queue;
//! - [`sync`] — linked accounts, per-series mappings and the matching backlogs;
//! - [`users`] — user administration, as a list + inspector;
//! - [`flags`] — the feature-flag control plane;
//! - [`privacy`] — the GDPR data-subject request queue;
//! - [`audit`] — the privileged-action trail.
//!
//! **Providers** and **Users** are the two entities redesigned into the list+inspector shape.
//! The rest render their existing panel across the inspector column (`.wide`), which is why
//! the rail is the shell and the panes are not.
//!
//! # Which entities a reader sees
//!
//! Each entity declares the permission that opens it *and* the feature that has to be on for
//! it to be worth opening, so someone granted nothing but `merge.read` gets a rail with one
//! entry rather than eleven that mostly 403 or 404. Every one of those checks is a courtesy:
//! the server authorizes each call independently and hiding a control has never been the
//! boundary. What it buys is not showing a reader work they cannot do.
//!
//! # What refreshes
//!
//! The read-only entities share one pausable 4s [`RefreshTick`]. Providers, Users and Flags are
//! deliberately **off** it: they are work surfaces where someone is mid-edit, and a background
//! refetch landing on a half-filled form discards it. Those say so in the header and offer a
//! manual reload instead.

mod audit;
mod controls;
mod flags;
mod merge;
mod overview;
mod privacy;
mod providers;
mod scans;
mod shell;
mod solver;
mod stats;
mod sync;
mod users;

use crate::api;
use crate::i18n::use_i18n;
use crate::icons::{Ic, Icon};
use crate::models::*;
use crate::state::capabilities::{use_capabilities, CapabilitySet};
use crate::util::thousands;
use crate::wire::types::{Feature, Permission};
use dioxus::prelude::*;
use progenitor_client::ResponseValue;

/// Auto-refresh cadence for the read-only entities.
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

/// The console's entities, in rail order within their group.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Entity {
    Overview,
    Merge,
    Providers,
    Scans,
    Solver,
    AdapterTest,
    Users,
    Sync,
    Flags,
    Privacy,
    Audit,
}

/// The rail's groups, in order, each with the entities it holds.
///
/// The design's `CATALOGUE` group also lists Series and Chapters; neither has a console surface
/// yet, and an entry that opens nothing is worse than no entry, so they are absent rather than
/// stubbed. `Overview` gains its own leading group because it is a real surface the design's
/// rail does not account for.
const RAIL: &[(&str, &[Entity])] = &[
    ("console.group.system", &[Entity::Overview]),
    ("console.group.catalogue", &[Entity::Merge]),
    (
        "console.group.pipeline",
        &[
            Entity::Providers,
            Entity::Scans,
            Entity::Solver,
            Entity::AdapterTest,
        ],
    ),
    (
        "console.group.people",
        &[
            Entity::Users,
            Entity::Sync,
            Entity::Flags,
            Entity::Privacy,
            Entity::Audit,
        ],
    ),
];

impl Entity {
    /// The catalogue key of this entity's rail label.
    fn label_key(self) -> &'static str {
        match self {
            Self::Overview => "console.tab.overview",
            Self::Merge => "console.tab.merge",
            Self::Providers => "console.tab.providers",
            Self::Scans => "console.tab.liveScans",
            Self::Solver => "console.tab.solver",
            Self::AdapterTest => "console.tab.adapterTest",
            Self::Users => "console.tab.users",
            Self::Sync => "console.tab.sync",
            Self::Flags => "console.tab.flags",
            Self::Privacy => "console.tab.privacy",
            Self::Audit => "console.tab.audit",
        }
    }

    /// The breadcrumb segment shown beside the wordmark.
    fn slug(self) -> &'static str {
        match self {
            Self::Overview => "overview",
            Self::Merge => "merge-queue",
            Self::Providers => "providers",
            Self::Scans => "scan-runs",
            Self::Solver => "solver",
            Self::AdapterTest => "adapter-test",
            Self::Users => "users",
            Self::Sync => "sync-links",
            Self::Flags => "feature-flags",
            Self::Privacy => "privacy",
            Self::Audit => "audit",
        }
    }

    /// The permission that opens this entity, and the feature that has to be switched on for it
    /// to be worth opening.
    ///
    /// Both stated in one place so adding an entity means answering both questions at once —
    /// the alternative is an entry that renders for everyone, or one whose panel 404s.
    fn requires(self) -> (Permission, Feature) {
        match self {
            Self::Overview => (Permission::SystemStats, Feature::AdminStats),
            Self::Scans => (Permission::ScansRead, Feature::ScanningManual),
            // Solver health is provider health seen from the fetch pipeline's side: same data,
            // same permission, same feature — a second view rather than a second surface.
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

    /// Whether this reader should be offered this entity at all.
    fn is_visible(self, caps: &CapabilitySet) -> bool {
        let (permission, feature) = self.requires();
        caps.can(permission) && caps.has_feature(feature)
    }

    /// Whether this entity owns both the list and the inspector pane, or renders one wide panel.
    fn is_master_detail(self) -> bool {
        matches!(self, Self::Providers | Self::Users)
    }

    /// Whether this entity refetches from the shared tick. Work surfaces opt out — see the
    /// module docs.
    fn auto_refreshes(self) -> bool {
        !matches!(self, Self::Providers | Self::Users | Self::Flags)
    }
}

/// One rail count: the number, and the tone that says whether it needs attention.
#[derive(Clone, Copy, PartialEq, Eq)]
enum CountTone {
    Plain,
    /// Work is queued and someone has to do it.
    Attention,
    /// Something is running right now.
    Live,
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
    let api = api::use_api();
    let caps = use_capabilities();

    // Held back until the capability fetch lands: rendering "operators only" first and the
    // console a moment later reads as a permission error to anyone who blinks.
    if !caps.is_ready() {
        return rsx! {
            h1 { class: "ik-page-title", {i18n.t("nav.console")} }
            crate::components::SkeletonBlock { height: 220 }
        };
    }

    let visible: Vec<Entity> = RAIL
        .iter()
        .flat_map(|(_, entities)| entities.iter().copied())
        .filter(|entity| entity.is_visible(&caps))
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
    let mut selected = use_signal(|| first);
    use_future(move || async move {
        loop {
            gloo_timers::future::TimeoutFuture::new(REFRESH_MS).await;
            // `peek`, not `read`: this loop must not re-subscribe itself to `auto`, or toggling
            // the pause switch would restart the timer rather than gate it.
            if *auto.peek() {
                tick.bump();
            }
        }
    });

    // Rail counts come from the one endpoint that already aggregates them. A reader without
    // `system.stats` simply gets a rail with no numbers on it.
    let can_count = caps.can(Permission::SystemStats) && caps.has_feature(Feature::AdminStats);
    let stats = use_resource(move || {
        tick.track();
        let client = api.client();
        async move {
            if !can_count {
                return None;
            }
            client
                .system_stats()
                .send()
                .await
                .map(ResponseValue::into_inner)
                .ok()
        }
    });

    // The selected entity can stop being visible under the reader's feet — a permission
    // revoked, a feature switched off — in which case fall back to the first one they still
    // have rather than rendering a panel whose every call now fails.
    let current = {
        let choice = *selected.read();
        if visible.contains(&choice) {
            choice
        } else {
            first
        }
    };

    // Groups with nothing visible in them are dropped whole: a kicker over an empty stretch of
    // rail reads as a broken list.
    let groups: Vec<(&str, Vec<Entity>)> = RAIL
        .iter()
        .map(|(key, entities)| {
            let shown = entities
                .iter()
                .copied()
                .filter(|entity| entity.is_visible(&caps))
                .collect::<Vec<_>>();
            (*key, shown)
        })
        .filter(|(_, shown)| !shown.is_empty())
        .collect();

    let counts = stats.read_unchecked().clone().flatten();
    let body_class = if current.is_master_detail() {
        "ik-cons-body"
    } else {
        "ik-cons-body wide"
    };

    let panel = match current {
        Entity::Overview => rsx! {
            div { class: "ik-cons-pane",
                overview::SystemOverview { tick }
                stats::ProviderStatsTable { tick }
            }
        },
        Entity::Scans => rsx! {
            div { class: "ik-cons-pane",
                scans::ScanQueue { tick }
            }
        },
        Entity::Providers => rsx! { providers::ProvidersEntity {} },
        Entity::Solver => rsx! {
            div { class: "ik-cons-pane",
                solver::SolverPanel { tick }
            }
        },
        Entity::AdapterTest => rsx! {
            div { class: "ik-cons-pane",
                solver::AdapterTestTab {}
            }
        },
        Entity::Merge => rsx! {
            div { class: "ik-cons-pane",
                merge::MergeQueue {}
            }
        },
        Entity::Sync => rsx! {
            div { class: "ik-cons-pane",
                sync::SyncAdminPanel {}
            }
        },
        Entity::Users => rsx! { users::UsersEntity {} },
        Entity::Flags => rsx! {
            div { class: "ik-cons-pane",
                flags::FeatureFlagsPanel {}
            }
        },
        Entity::Privacy => rsx! {
            div { class: "ik-cons-pane",
                privacy::PrivacyQueuePanel { tick }
            }
        },
        Entity::Audit => rsx! {
            div { class: "ik-cons-pane",
                audit::AuditPanel { tick }
            }
        },
    };

    rsx! {
        div { class: "ik-cons",
            div { class: "ik-cons-bar",
                div { class: "ik-cons-brand",
                    span { class: "ik-cons-tile",
                        Ic { icon: Icon::MenuBook, size: 15 }
                    }
                    span { class: "nm", {i18n.t("console.title")} }
                    span { class: "ik-cons-crumb", "/ {current.slug()}" }
                }
                JumpField {}
                div { class: "ik-flex", style: "margin-left:auto;gap:9px;flex-wrap:wrap;",
                    if current.auto_refreshes() {
                        controls::LiveControls { tick, auto }
                    } else {
                        span { class: "ik-mono", style: "font-size:11.5px;color:var(--faint);",
                            {i18n.t("console.noAutoRefresh")}
                        }
                    }
                }
            }
            div { class: "{body_class}",
                nav { class: "ik-cons-rail", "aria-label": i18n.t("console.title"),
                    for (group_key , entities) in groups {
                        div { key: "{group_key}", class: "grp", {i18n.t(group_key)} }
                        for entity in entities {
                            button {
                                key: "{entity.slug()}",
                                class: if entity == current { "ik-cons-entry active" } else { "ik-cons-entry" },
                                "aria-current": if entity == current { "page" } else { "false" },
                                onclick: move |_| selected.set(entity),
                                span { {i18n.t(entity.label_key())} }
                                RailCount { entity, counts: counts.clone() }
                            }
                        }
                    }
                }
                {panel}
            }
        }
    }
}

/// The rail's right-aligned count for an entity, when the stats endpoint supplies one.
#[component]
fn RailCount(entity: Entity, counts: Option<SystemStats>) -> Element {
    let Some(stats) = counts else {
        return rsx! {};
    };
    let (value, tone) = match entity {
        Entity::Merge => (
            stats.pending_merges,
            if stats.pending_merges > 0 {
                CountTone::Attention
            } else {
                CountTone::Plain
            },
        ),
        Entity::Providers => (stats.providers_total, CountTone::Plain),
        Entity::Scans => (
            stats.runs_active,
            if stats.runs_active > 0 {
                CountTone::Live
            } else {
                CountTone::Plain
            },
        ),
        Entity::Users => (stats.users_total, CountTone::Plain),
        _ => return rsx! {},
    };

    let class = match tone {
        CountTone::Plain => "cnt",
        CountTone::Attention => "cnt acc",
        CountTone::Live => "cnt live",
    };
    rsx! {
        span { class: "{class}",
            if tone == CountTone::Live {
                span { class: "ik-live-dot", style: "width:6px;height:6px;background:currentColor;" }
            }
            "{thousands(value)}"
        }
    }
}

/// The jump field. `⌘K` is already bound to the top bar's search box (`index.html`), so this
/// focuses that rather than advertising a command palette the app does not have.
#[component]
fn JumpField() -> Element {
    let i18n = use_i18n();
    rsx! {
        button {
            class: "ik-cons-jump",
            onclick: move |_| {
                let _ = document::eval(
                    "const el = document.getElementById('tv-search'); if (el) { el.focus(); el.select(); }",
                );
            },
            span { style: "display:flex;flex:none;",
                Ic { icon: Icon::Search, size: 15 }
            }
            span { {i18n.t("console.jump")} }
            span { class: "kbd", "⌘K" }
        }
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

/// Build the politeness JSON payload from the editor's fields, or the catalogue key of the
/// field that would not parse.
///
/// `emulation` is the wire token of a browser profile, or empty for "no emulation" — which
/// must be sent as an explicit `null`, since omitting the key would let the server-side serde
/// default put the provider back on Chrome.
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

/// Pretty-print a stored adapter config for the editor (empty / null → `{}`).
pub(super) fn config_editor_text(v: &serde_json::Value) -> String {
    if v.is_null() {
        "{}".to_owned()
    } else {
        serde_json::to_string_pretty(v).unwrap_or_else(|_| "{}".to_owned())
    }
}
