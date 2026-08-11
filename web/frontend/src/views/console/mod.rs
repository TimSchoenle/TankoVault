//! Operator Console: a master–detail surface with an entity rail, an entity list, and a deep
//! inspector, one module per entity.
//!
//! Every filter and selection lives in the URL (see [`query`]), so a console view can be linked
//! to. Panels read it through [`ConsoleNav`] and never keep a second copy: a signal shadowing a
//! parameter is a filter that reverts on the back button.
//!
//! Providers, Users and Flags are deliberately off the shared [`RefreshTick`]: they are mid-edit
//! work surfaces, and a background refetch landing on a half-filled form would discard it.

mod audit;
mod catalogue;
mod controls;
mod decisions;
mod flags;
mod live;
mod merge;
mod overview;
mod privacy;
mod providers;
pub(crate) mod query;
mod recommendations;
mod scans;
mod solver;
mod stats;
mod sync;
mod users;

pub(crate) use query::ConsoleQuery;

use crate::api;
use crate::app::Route;
use crate::components::EmptyBox;
use crate::hooks::{use_reload, Reload};
use crate::i18n::use_i18n;
use crate::icons::{Ic, Icon};
use crate::models::*;
use crate::state::capabilities::{use_capabilities, CapabilitySet};
use crate::state::prefs;
use crate::util::thousands;
use crate::wire::types::{Feature, Permission};
use dioxus::prelude::*;
use dioxus::router::Navigator;
use progenitor_client::ResponseValue;
use std::fmt;
use std::str::FromStr;

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
///
/// Public because it is a route segment: [`ConsoleEntity::slug`] *is* the URL, so
/// `/console/providers` and `/console/feature-flags` are addresses rather than parameters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ConsoleEntity {
    Overview,
    Catalogue,
    Merge,
    Decisions,
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
const RAIL: &[(&str, &[ConsoleEntity])] = &[
    ("console.group.system", &[ConsoleEntity::Overview]),
    (
        "console.group.catalogue",
        &[
            ConsoleEntity::Catalogue,
            ConsoleEntity::Merge,
            ConsoleEntity::Decisions,
            ConsoleEntity::Recommendations,
        ],
    ),
    (
        "console.group.pipeline",
        &[
            ConsoleEntity::Providers,
            ConsoleEntity::Scans,
            ConsoleEntity::Solver,
            ConsoleEntity::AdapterTest,
        ],
    ),
    (
        "console.group.people",
        &[
            ConsoleEntity::Users,
            ConsoleEntity::Sync,
            ConsoleEntity::Flags,
            ConsoleEntity::Privacy,
            ConsoleEntity::Audit,
        ],
    ),
];

impl ConsoleEntity {
    /// Every entity, in rail order. Kept in step with [`RAIL`] by
    /// `the_rail_and_the_entity_list_hold_the_same_entities`.
    pub(crate) const ALL: [ConsoleEntity; 14] = [
        Self::Overview,
        Self::Catalogue,
        Self::Merge,
        Self::Decisions,
        Self::Recommendations,
        Self::Providers,
        Self::Scans,
        Self::Solver,
        Self::AdapterTest,
        Self::Users,
        Self::Sync,
        Self::Flags,
        Self::Privacy,
        Self::Audit,
    ];

    /// The catalogue key of this entity's rail label.
    fn label_key(self) -> &'static str {
        match self {
            Self::Overview => "console.tab.overview",
            Self::Catalogue => "console.tab.catalogue",
            Self::Merge => "console.tab.merge",
            Self::Decisions => "console.tab.decisions",
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
            // The catalogue itself, as opposed to the queues that groom it.
            Self::Catalogue => Icon::MenuBook,
            Self::Merge => Icon::Merge,
            // A judgement surface for two automatic engines, not a queue to work — and
            // distinct from Audit, which records what *people* did.
            Self::Decisions => Icon::Gavel,
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

    /// This entity's URL segment, and the breadcrumb shown beside the wordmark.
    pub(crate) fn slug(self) -> &'static str {
        match self {
            Self::Overview => "overview",
            Self::Catalogue => "catalogue",
            Self::Merge => "merge-queue",
            Self::Decisions => "decisions",
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
            Self::Catalogue => (Permission::CatalogueRead, Feature::AdminCatalogue),
            Self::Scans => (Permission::ScansRead, Feature::ScanningManual),
            // Solver health is provider health from the fetch pipeline's side: same data and permission.
            Self::Providers | Self::Solver => (Permission::ProvidersRead, Feature::AdminProviders),
            Self::AdapterTest => (Permission::ProvidersTest, Feature::AdminAdapterTest),
            Self::Merge => (Permission::MergeRead, Feature::ScanningMergeQueue),
            // The *merge* half of the pair; `is_visible` widens it to either journal.
            Self::Decisions => (Permission::MergeAudit, Feature::AdminAudit),
            Self::Recommendations => (Permission::RecsysRead, Feature::AdminRecommendations),
            Self::Sync => (Permission::SyncAdminRead, Feature::AdminSync),
            Self::Users => (Permission::UsersRead, Feature::AdminUsers),
            Self::Flags => (Permission::FlagsRead, Feature::AdminFeatureFlags),
            Self::Privacy => (Permission::PrivacyRead, Feature::PrivacyRequests),
            Self::Audit => (Permission::AuditRead, Feature::AdminAudit),
        }
    }

    /// Whether this reader should be offered this entity at all.
    ///
    /// [`Self::Decisions`] is the one entity backed by two independent journals behind two
    /// independent permissions, so holding either opens it and the panel shows whichever halves
    /// the reader may see. Folding that into `requires` would mean an operator granted only
    /// `sync.audit` could not reach the sync journal at all.
    fn is_visible(self, caps: &CapabilitySet) -> bool {
        let (permission, feature) = self.requires();
        let permitted =
            caps.can(permission) || (self == Self::Decisions && caps.can(Permission::SyncAudit));
        permitted && caps.has_feature(feature)
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
            Self::Providers | Self::Users | Self::Flags | Self::Recommendations | Self::Catalogue
        )
    }
}

impl fmt::Display for ConsoleEntity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.slug())
    }
}

/// An unrecognised slug. The route table turns this into a redirect to `/console` rather than a
/// 404: a link to an entity this build has dropped should still land the operator in the
/// console.
#[derive(Debug)]
pub(crate) struct UnknownEntity;

impl fmt::Display for UnknownEntity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("no console entity by that name")
    }
}

impl FromStr for ConsoleEntity {
    type Err = UnknownEntity;

    fn from_str(slug: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|entity| entity.slug() == slug)
            .ok_or(UnknownEntity)
    }
}

/// The console's addressable state, and the two ways a panel changes it.
///
/// Handed down as context rather than threaded as props: the inspector tabs sit four components
/// below the route, and every handle here is `Copy`, so a lookup is cheaper than the clone chain
/// would be.
#[derive(Clone, Copy)]
pub(super) struct ConsoleNav {
    entity: Memo<ConsoleEntity>,
    query: Memo<ConsoleQuery>,
    nav: Navigator,
}

impl ConsoleNav {
    /// The entity currently open.
    pub(super) fn entity(self) -> ConsoleEntity {
        *self.entity.read()
    }

    /// The current view state. Cloned, because callers build the next state from it.
    pub(super) fn query(self) -> ConsoleQuery {
        self.query.read().clone()
    }

    /// A **selection** change: pushes, so the back button moves between rows.
    pub(super) fn select(self, next: ConsoleQuery) {
        self.nav.push(Route::ConsoleSection {
            entity: self.entity(),
            query: next,
        });
    }

    /// A **filter** change: replaces, so a debounced search box does not leave one history
    /// entry per keystroke.
    pub(super) fn filter(self, next: ConsoleQuery) {
        self.nav.replace(Route::ConsoleSection {
            entity: self.entity(),
            query: next,
        });
    }

    /// Move to another entity, dropping the filters — they belong to the panel being left.
    pub(super) fn open(self, entity: ConsoleEntity) {
        self.nav.push(Route::ConsoleSection {
            entity,
            query: ConsoleQuery::fresh(),
        });
    }
}

/// The console's nav handle, from any component below the route.
pub(super) fn use_console_nav() -> ConsoleNav {
    use_context::<ConsoleNav>()
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
    AdapterKind::Mangathemesia,
    AdapterKind::Manganato,
    AdapterKind::Keyoapp,
    AdapterKind::Custom,
];

/// The catalogue key wording this adapter kind for the reader (see [`crate::i18n`]).
pub(super) fn adapter_label_key(a: AdapterKind) -> &'static str {
    match a {
        AdapterKind::GenericConfig => "console.adapterKind.genericConfig",
        AdapterKind::Madara => "console.adapterKind.madara",
        AdapterKind::Mangathemesia => "console.adapterKind.mangathemesia",
        AdapterKind::Manganato => "console.adapterKind.manganato",
        AdapterKind::Keyoapp => "console.adapterKind.keyoapp",
        AdapterKind::Custom => "console.adapterKind.custom",
    }
}

/// The entity a bare `/console` opens: where the operator left off, if they may still see it,
/// else the first entity their capabilities allow.
fn landing_entity(caps: &CapabilitySet) -> Option<ConsoleEntity> {
    prefs::console_entity()
        .filter(|entity| entity.is_visible(caps))
        .or_else(|| {
            ConsoleEntity::ALL
                .into_iter()
                .find(|entity| entity.is_visible(caps))
        })
}

/// `/console` — the way in, not a place. Resolves the landing entity and rewrites the address
/// bar to it, so every console view an operator can reach is one they can also send.
#[component]
pub(crate) fn Console() -> Element {
    let i18n = use_i18n();
    let caps = use_capabilities();
    let session = crate::state::use_session();
    let nav = navigator();

    use_effect(move || {
        if !caps.is_ready() {
            return;
        }
        if let Some(entity) = landing_entity(&caps) {
            nav.replace(Route::ConsoleSection {
                entity,
                query: ConsoleQuery::fresh(),
            });
        }
    });

    // `/console` is a public route (the rail link is merely hidden while signed out), so a
    // bookmark, a shared link, or a session expiry can land here unauthenticated.
    if !session.is_authenticated() {
        return rsx! { crate::components::AuthRequired { title: i18n.t("nav.console") } };
    }
    if caps.is_ready() && landing_entity(&caps).is_none() {
        return rsx! {
            h1 { class: "ik-page-title", {i18n.t("nav.console")} }
            EmptyBox { message: i18n.t("console.operatorsOnly") }
        };
    }

    // Held back until the capability fetch lands: rendering "operators only" first and the
    // console a moment later reads as a permission error to anyone who blinks.
    rsx! {
        h1 { class: "ik-page-title", {i18n.t("nav.console")} }
        crate::components::SkeletonBlock { height: 220 }
    }
}

/// `/console/:entity?:..query` — the console itself.
#[component]
pub(crate) fn ConsoleSection(entity: ConsoleEntity, query: ConsoleQuery) -> Element {
    let i18n = use_i18n();
    let api = api::use_api();
    let caps = use_capabilities();
    let session = crate::state::use_session();
    let navigator = navigator();

    // The route is the read side of every filter; these memos recompute when it changes rather
    // than mirroring it into signals that could drift out of step with the address bar.
    let entity_memo = use_memo(use_reactive!(|entity| entity));
    let query_memo = use_memo(use_reactive!(|query| query.clone()));
    let nav = use_context_provider(|| ConsoleNav {
        entity: entity_memo,
        query: query_memo,
        nav: navigator,
    });

    // Remember where the operator was, and bail out to somewhere they can see if the entity in
    // the URL loses its permission or feature under their feet.
    use_effect(move || {
        if !caps.is_ready() {
            return;
        }
        let current = *entity_memo.read();
        if current.is_visible(&caps) {
            prefs::set_console_entity(current);
        } else if let Some(fallback) = landing_entity(&caps) {
            navigator.replace(Route::ConsoleSection {
                entity: fallback,
                query: ConsoleQuery::fresh(),
            });
        }
    });

    let tick = RefreshTick(use_reload());
    let auto = use_signal(prefs::console_live);
    // The stream is the cadence now; `RefreshTick` stays for the panels it carries no event for
    // and for the manual refresh button. The timer that used to bump it every four seconds is
    // gone — twelve panels refetching in lockstep is what this replaced.
    let live = live::use_console_live(api, auto.into());

    // A reader without `system.stats` simply gets a rail with no numbers on it. The first paint
    // still needs a fetch: the stream's `stats` event is ten seconds away, and a rail that
    // counts up from nothing reads as an empty deployment.
    let can_count = caps.can(Permission::SystemStats) && caps.has_feature(Feature::AdminStats);
    let seed = use_resource(move || {
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

    // Groups with nothing visible in them are dropped whole: a kicker over an empty stretch of
    // rail reads as a broken list.
    //
    // Memoised because this component re-renders on the shared tick, and the rail's shape
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
            .collect::<Vec<(&str, Vec<ConsoleEntity>)>>()
    });

    if !session.is_authenticated() {
        return rsx! { crate::components::AuthRequired { title: i18n.t("nav.console") } };
    }
    if !caps.is_ready() {
        return rsx! {
            h1 { class: "ik-page-title", {i18n.t("nav.console")} }
            crate::components::SkeletonBlock { height: 220 }
        };
    }
    // The effect above is already navigating away; rendering the panel meanwhile would fire a
    // privileged fetch the reader is not entitled to make.
    if !entity.is_visible(&caps) {
        return rsx! {
            h1 { class: "ik-page-title", {i18n.t("nav.console")} }
            EmptyBox { message: i18n.t("console.operatorsOnly") }
        };
    }
    let current = entity;

    // Pushed first, seeded second: the stream is the newer of the two the moment it lands.
    let counts = live
        .stats
        .read()
        .clone()
        .or_else(|| seed.read_unchecked().clone().flatten());
    let body_class = if current.is_master_detail() {
        "ik-cons-body"
    } else {
        "ik-cons-body wide"
    };

    let panel = match current {
        ConsoleEntity::Overview => rsx! {
            div { class: "ik-cons-pane",
                overview::SystemOverview { tick }
                stats::ProviderStatsTable { tick }
            }
        },
        ConsoleEntity::Scans => rsx! {
            div { class: "ik-cons-pane",
                scans::ScanQueue { tick }
            }
        },
        ConsoleEntity::Providers => rsx! { providers::ProvidersEntity {} },
        ConsoleEntity::Solver => rsx! {
            div { class: "ik-cons-pane",
                solver::SolverPanel { tick }
            }
        },
        ConsoleEntity::AdapterTest => rsx! {
            div { class: "ik-cons-pane",
                solver::AdapterTestTab {}
            }
        },
        ConsoleEntity::Catalogue => rsx! { catalogue::CatalogueEntity {} },
        ConsoleEntity::Merge => rsx! {
            div { class: "ik-cons-pane",
                merge::MergeQueue {}
            }
        },
        ConsoleEntity::Decisions => rsx! {
            div { class: "ik-cons-pane",
                decisions::DecisionsPanel { tick }
            }
        },
        ConsoleEntity::Recommendations => rsx! {
            div { class: "ik-cons-pane",
                recommendations::RecommendationsPanel {}
            }
        },
        ConsoleEntity::Sync => rsx! {
            div { class: "ik-cons-pane",
                sync::SyncAdminPanel {}
            }
        },
        ConsoleEntity::Users => rsx! { users::UsersEntity {} },
        ConsoleEntity::Flags => rsx! {
            div { class: "ik-cons-pane",
                flags::FeatureFlagsPanel {}
            }
        },
        ConsoleEntity::Privacy => rsx! {
            div { class: "ik-cons-pane",
                privacy::PrivacyQueuePanel { tick }
            }
        },
        ConsoleEntity::Audit => rsx! {
            div { class: "ik-cons-pane",
                audit::AuditPanel { tick }
            }
        },
    };

    rsx! {
        div { class: "ik-cons",
            // A page head, not a second app bar. This used to carry the product's own tile and
            // wordmark under the rail that already shows both, and a search box that only moved
            // focus to the one in the top bar — so the console read as an application embedded in
            // the application, with two brands and two search fields on screen at once.
            div { class: "ik-cons-bar",
                div { class: "ik-cons-heading",
                    h1 { class: "ik-page-title", {i18n.t("console.title")} }
                    span { class: "ik-cons-crumb", {i18n.t(current.label_key())} }
                }
                div { class: "ik-flex", style: "margin-left:auto;gap:9px;flex-wrap:wrap;",
                    if current.auto_refreshes() {
                        controls::LiveControls { tick, auto, state: live.state }
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
                                onclick: move |_| nav.open(entity),
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
fn RailCount(entity: ConsoleEntity, counts: Option<SystemStats>) -> Element {
    let Some(stats) = counts else {
        return rsx! {};
    };
    let (value, tone) = match entity {
        ConsoleEntity::Merge => (
            stats.pending_merges,
            if stats.pending_merges > 0 {
                CountTone::Attention
            } else {
                CountTone::Plain
            },
        ),
        ConsoleEntity::Catalogue => (stats.series_total, CountTone::Plain),
        ConsoleEntity::Providers => (stats.providers_total, CountTone::Plain),
        ConsoleEntity::Scans => (
            stats.runs_active,
            if stats.runs_active > 0 {
                CountTone::Live
            } else {
                CountTone::Plain
            },
        ),
        ConsoleEntity::Users => (stats.users_total, CountTone::Plain),
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

// The console's jump field is gone. It was a search-shaped button that searched nothing: it
// moved focus to the top bar's catalogue search, which looks up *series*, on a screen whose rows
// are providers, runs and accounts. `⌘K` still reaches that box from here, as it does everywhere.

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

/// The catalogue wording for a scoring rule or signal slug, falling back to the slug.
///
/// The vocabulary lives in the scorer, and the console must not need a release to display a rule
/// someone has just added — rendering `console.merge.signal.foo` to an operator is worse than
/// rendering `foo`. Shared by the merge queue and the decision journal, which score the same
/// pairs and so name the same signals.
pub(super) fn signal_label(i18n: crate::i18n::Translator, slug: &str) -> String {
    [
        format!("console.merge.signal.{slug}"),
        format!("console.decisions.term.{slug}"),
    ]
    .iter()
    .find_map(|key| i18n.t_opt(key))
    .unwrap_or_else(|| slug.to_owned())
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
    use super::{adapter_label_key, ConsoleEntity, ADAPTER_KINDS, RAIL};
    use crate::models::AdapterKind;

    /// `ConsoleEntity::ALL` is what the palette enumerates and what the slug round-trip test
    /// covers; `RAIL` is what the operator can actually click. An entity in one and not the
    /// other is either an address nothing reaches or a rail entry no link can name.
    #[test]
    fn the_rail_and_the_entity_list_hold_the_same_entities() {
        let mut railed: Vec<&str> = RAIL
            .iter()
            .flat_map(|(_, entities)| entities.iter().map(|entity| entity.slug()))
            .collect();
        let mut listed: Vec<&str> = ConsoleEntity::ALL.iter().map(|e| e.slug()).collect();
        railed.sort_unstable();
        listed.sort_unstable();
        assert_eq!(railed, listed);
    }

    /// A rail entry with no wording renders as `console.tab.…` to the operator, because a
    /// missing catalogue key falls back to the key itself rather than failing.
    #[test]
    fn every_entity_is_worded() {
        for entity in ConsoleEntity::ALL {
            let key = entity.label_key();
            assert!(
                crate::i18n::has_key(key),
                "`{}` is on the rail but `{key}` is not in the catalogue",
                entity.slug()
            );
        }
    }

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
