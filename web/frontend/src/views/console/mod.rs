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
/// One tick drives them all, so the whole dashboard is consistent at each cadence instead of
/// each panel drifting on its own timer — and pausing is a single switch rather than nine.
///
/// A newtype over [`Reload`], not a second `Signal<u32>`. The two were structurally identical
/// but not interchangeable, so a tick-driven panel had nothing to hand [`ErrorBox`] as its
/// retry action — which is why five of them open-coded their error state as muted grey body
/// text with no retry at all, quietly breaking the "a failed fetch is always visible and always
/// retryable" invariant the helpers exist to hold. The distinct type is still worth keeping:
/// in prop position it says "the shared console cadence", not "this panel's own reload".
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

    /// This entity's rail glyph (`DESIGN_SPEC` §6-7). The rail shipped without any, which is
    /// why a quarter of the icon inventory was unreferenced.
    fn icon(self) -> Icon {
        match self {
            Self::Overview => Icon::Dashboard,
            Self::Merge => Icon::Merge,
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

/// Selectable adapter implementations, in the order the create form offers them.
///
/// This was a `&[(&str, &str)]` table of hand-written wire tokens beside the real enum
/// (FRONTEND F10), and `create.rs` parsed the token back with a `_ => AdapterKind::Custom`
/// arm — so a typo in the table registered every provider as `Custom`, silently and with a
/// perfectly plausible-looking picker. The tokens are now the generated `Display`, the parse
/// is the generated `FromStr`, and this array carries only the *order*.
///
/// Still hand-listed, because the generated client offers no way to enumerate a schema enum's
/// variants. What stops it drifting is [`adapter_label_key`]: its `match` is exhaustive, so a
/// variant added to `AdapterKind` fails to compile until it is worded, and
/// `the_picker_offers_every_adapter_kind` fails until it reaches this array too.
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

    // The gate every other protected route has, and this one did not. `/console` is a public
    // route (the rail link is merely hidden while signed out), so a bookmark, a shared link,
    // a session expiry with the page open, or signing out from here all land on it — and
    // capabilities are cleared to `Loading` whenever there is no session, so `is_ready()`
    // below was permanently false and the skeleton was permanent with it.
    if !session.is_authenticated() {
        return rsx! { crate::components::AuthRequired { title: i18n.t("nav.console") } };
    }

    // Held back until the capability fetch lands: rendering "operators only" first and the
    // console a moment later reads as a permission error to anyone who blinks. Reachable only
    // for a signed-in reader now, so it is a genuine in-flight fetch rather than a dead end.
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

    // One tick drives every read-only panel's refetch: the background loop bumps it on a
    // cadence while `auto` is on, and the Refresh control bumps it on demand.
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
