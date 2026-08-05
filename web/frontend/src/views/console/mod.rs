//! Operator Console: a master–detail surface with an entity rail, an entity list, and a deep
//! inspector, one module per entity.
//!
//! Providers, Users and Flags are deliberately off the shared [`RefreshTick`]: they are mid-edit
//! work surfaces, and a background refetch landing on a half-filled form would discard it.

mod audit;
mod controls;
mod flags;
mod merge;
mod overview;
mod privacy;
mod providers;
mod recommendations;
mod scans;
mod solver;
mod stats;
mod sync;
mod users;

use crate::api;
use crate::components::EmptyBox;
use crate::hooks::{use_reload, Reload};
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
/// A newtype over [`Reload`] rather than reusing it directly: in prop position it says "the
/// shared console cadence", not "this panel's own reload", so a tick-driven panel still has
/// something distinct to hand [`ErrorBox`] as its retry action.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct RefreshTick(Reload);

impl RefreshTick {
    /// Subscribe the calling reactive scope, so it refetches on the next tick.
    pub(super) fn track(self) {
        self.0.track();
    }

    /// Advance the tick, refetching every panel that tracks it.
    pub(super) fn bump(self) {
        self.0.bump();
    }

    /// The underlying handle, for passing to `async_view` and friends as the retry action.
    pub(super) fn reload(self) -> Reload {
        self.0
    }
}

/// The console's entities, in rail order within their group.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Entity {
    Overview,
    Merge,
    Recommendations,
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
const RAIL: &[(&str, &[Entity])] = &[
    ("console.group.system", &[Entity::Overview]),
    (
        "console.group.catalogue",
        &[Entity::Merge, Entity::Recommendations],
    ),
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
            Self::Recommendations => "console.tab.recommendations",
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

    /// This entity's rail glyph.
    fn icon(self) -> Icon {
        match self {
            Self::Overview => Icon::Dashboard,
            Self::Merge => Icon::Merge,
            // A tuning surface, not a discovery one: the operator's view of it is the knobs.
            Self::Recommendations => Icon::Tune,
            Self::Providers => Icon::Layers,
            Self::Scans => Icon::Radar,
            Self::Solver => Icon::ShieldLock,
            Self::AdapterTest => Icon::Code,
            Self::Users => Icon::Group,
            Self::Sync => Icon::CloudSync,
            Self::Flags => Icon::Flag,
            // Data-subject requests are one person's records, not a policy surface.
            Self::Privacy => Icon::Person,
            Self::Audit => Icon::History,
        }
    }

    /// The breadcrumb segment shown beside the wordmark.
    fn slug(self) -> &'static str {
        match self {
            Self::Overview => "overview",
            Self::Merge => "merge-queue",
            Self::Recommendations => "recommendations",
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
    fn requires(self) -> (Permission, Feature) {
        match self {
            Self::Overview => (Permission::SystemStats, Feature::AdminStats),
            Self::Scans => (Permission::ScansRead, Feature::ScanningManual),
            // Solver health is provider health from the fetch pipeline's side: same data and permission.
            Self::Providers | Self::Solver => (Permission::ProvidersRead, Feature::AdminProviders),
            Self::AdapterTest => (Permission::ProvidersTest, Feature::AdminAdapterTest),
            Self::Merge => (Permission::MergeRead, Feature::ScanningMergeQueue),
            Self::Recommendations => (Permission::RecsysRead, Feature::AdminRecommendations),
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
        !matches!(
            self,
            Self::Providers | Self::Users | Self::Flags | Self::Recommendations
        )
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

/// Selectable adapter implementations, in the order the create form offers them.
///
/// Hand-listed because the generated client offers no way to enumerate a schema enum's
/// variants. Kept in sync by [`adapter_label_key`]'s exhaustive match and by
/// `the_picker_offers_every_adapter_kind`.
pub(super) const ADAPTER_KINDS: &[AdapterKind] = &[
    AdapterKind::GenericConfig,
    AdapterKind::Madara,
    AdapterKind::Custom,
];

/// The catalogue key wording this adapter kind for the reader (see [`crate::i18n`]).
pub(super) fn adapter_label_key(a: AdapterKind) -> &'static str {
    match a {
        AdapterKind::GenericConfig => "console.adapterKind.genericConfig",
        AdapterKind::Madara => "console.adapterKind.madara",
        AdapterKind::Custom => "console.adapterKind.custom",
    }
}

#[component]
pub(crate) fn Console() -> Element {
    let i18n = use_i18n();
    let api = api::use_api();
    let caps = use_capabilities();
    let session = crate::state::use_session();

    // `/console` is a public route (the rail link is merely hidden while signed out), so a
    // bookmark, a shared link, or a session expiry can land here unauthenticated.
    if !session.is_authenticated() {
        return rsx! { crate::components::AuthRequired { title: i18n.t("nav.console") } };
    }

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
            EmptyBox { message: i18n.t("console.operatorsOnly") }
        };
    };

    let tick = RefreshTick(use_reload());
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

    // A reader without `system.stats` simply gets a rail with no numbers on it.
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

    // Fall back to the first still-visible entity if the selected one loses its permission
    // or feature under the reader's feet.
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
    //
    // Memoised because this component re-renders on the shared 4s tick, and the rail's shape
    // depends on nothing that changes between ticks — only on the capability set.
    let groups = use_memo(move || {
        RAIL.iter()
            .map(|(key, entities)| {
                let shown = entities
                    .iter()
                    .copied()
                    .filter(|entity| entity.is_visible(&caps))
                    .collect::<Vec<_>>();
                (*key, shown)
            })
            .filter(|(_, shown)| !shown.is_empty())
            .collect::<Vec<(&str, Vec<Entity>)>>()
    });

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
        Entity::Recommendations => rsx! {
            div { class: "ik-cons-pane",
                recommendations::RecommendationsPanel {}
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
                    for (group_key , entities) in groups.read().clone() {
                        div { key: "{group_key}", class: "grp", {i18n.t(group_key)} }
                        for entity in entities {
                            button {
                                key: "{entity.slug()}",
                                class: if entity == current { "ik-cons-entry active" } else { "ik-cons-entry" },
                                "aria-current": if entity == current { "page" } else { "false" },
                                onclick: move |_| selected.set(entity),
                                Ic { icon: entity.icon(), size: 15 }
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
                crate::browser::focus_and_select("tv-search");
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

#[cfg(test)]
mod tests {
    use super::{adapter_label_key, ADAPTER_KINDS};
    use crate::models::AdapterKind;

    /// The provider-registration picker must offer every adapter the API accepts.
    ///
    /// `ADAPTER_KINDS` is the last hand-maintained part of this vocabulary — the tokens and the
    /// parse are generated now — so this is what keeps it in step. Read out of the committed
    /// `openapi.json`, the artefact `crates/api-client` is generated from and the only thing
    /// that connects these two workspaces (`web/frontend` is outside the host workspace, so no
    /// compiler relates a table here to an enum there).
    ///
    /// The defect this closes: the picker used to carry hand-written token strings, and
    /// `create.rs` parsed them back with a `_ => AdapterKind::Custom` arm, so one wrong
    /// character registered every new provider as `Custom` — a working-looking form producing
    /// a provider that scans nothing.
    #[test]
    fn the_picker_offers_every_adapter_kind() {
        const SPEC: &str = include_str!("../../../../../openapi.json");
        let spec: serde_json::Value = serde_json::from_str(SPEC).expect("openapi.json parses");

        let mut published: Vec<String> = spec["components"]["schemas"]["AdapterKind"]["enum"]
            .as_array()
            .expect("the document declares the AdapterKind vocabulary")
            .iter()
            .map(|v| v.as_str().expect("adapter tokens are strings").to_owned())
            .collect();
        let mut offered: Vec<String> = ADAPTER_KINDS.iter().map(ToString::to_string).collect();

        published.sort();
        offered.sort();
        assert_eq!(
            offered, published,
            "the provider-registration picker offers a different set of adapters than the API \
             publishes; add the missing variant to `ADAPTER_KINDS` and word it in \
             `adapter_label_key`"
        );
    }

    /// Every offered kind survives the round trip the create form actually performs: rendered
    /// into the `<option value>` by `Display`, read back out by `FromStr`.
    #[test]
    fn every_offered_adapter_kind_round_trips_through_its_option_value() {
        for kind in ADAPTER_KINDS.iter().copied() {
            assert_eq!(
                kind.to_string().parse::<AdapterKind>().ok(),
                Some(kind),
                "`{kind}` does not survive the picker's own value round trip"
            );
        }
    }

    /// Wording is a separate axis from membership, and a missing catalogue key renders as the
    /// key itself rather than as an error — so a kind added to the picker without a label ships
    /// an option reading `console.adapterKind.…` to the operator.
    #[test]
    fn every_offered_adapter_kind_is_worded() {
        for kind in ADAPTER_KINDS.iter().copied() {
            let key = adapter_label_key(kind);
            assert!(
                crate::i18n::has_key(key),
                "`{kind}` is offered in the picker but `{key}` is not in the catalogue"
            );
        }
    }
}
