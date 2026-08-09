//! The runtime feature gate: resolving [`Feature`] state and enforcing it on HTTP routes.
//!
//! A failed refresh keeps the previous snapshot rather than reverting to compiled defaults,
//! which would silently re-enable anything an operator had switched off.

use axum::extract::{MatchedPath, Request, State};
use axum::http::{Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use std::collections::HashSet;
use std::str::FromStr;
use std::sync::{Arc, RwLock};
use tankovault_domain::Feature;
use tokio_util::sync::CancellationToken;

/// The resolved set of enabled features, cheap to test and cheap to replace wholesale.
#[derive(Debug, Clone, Default)]
struct Snapshot {
    enabled: HashSet<Feature>,
}

impl Snapshot {
    /// Every feature at its compiled default. The state before any successful load.
    fn from_defaults() -> Self {
        Self {
            enabled: Feature::all()
                .iter()
                .copied()
                .filter(|f| f.default_enabled())
                .collect(),
        }
    }

    /// Apply stored overrides on top of the compiled defaults.
    ///
    /// An unknown key is ignored, not fatal (schema/binary can disagree across a rollback).
    /// A locked feature's override is also ignored: it must not remove the only recovery path.
    fn resolve(overrides: &[(String, bool)]) -> Self {
        let mut snapshot = Self::from_defaults();
        for (key, enabled) in overrides {
            let Ok(feature) = Feature::from_str(key) else {
                tracing::warn!(%key, "ignoring override for unknown feature");
                continue;
            };
            if feature.is_locked() {
                tracing::warn!(
                    feature = %feature,
                    "ignoring override for a locked feature; it cannot be switched off"
                );
                continue;
            }
            if *enabled {
                snapshot.enabled.insert(feature);
            } else {
                snapshot.enabled.remove(&feature);
            }
        }
        snapshot
    }
}

/// Where a [`FeatureGate`] reads overrides from.
#[async_trait::async_trait]
pub trait FlagSource: Send + Sync + 'static {
    /// The currently stored `(feature_key, enabled)` overrides.
    ///
    /// # Errors
    /// Any failure is returned as a message; the gate logs it and keeps its previous snapshot.
    async fn overrides(&self) -> Result<Vec<(String, bool)>, String>;
}

/// A source that never has any overrides, so every feature sits at its compiled default.
///
/// Used by services that enforce flags but hold no database handle, and by tests.
pub struct DefaultsOnly;

#[async_trait::async_trait]
impl FlagSource for DefaultsOnly {
    async fn overrides(&self) -> Result<Vec<(String, bool)>, String> {
        Ok(Vec::new())
    }
}

/// The real source: the `feature_flag_overrides` table.
#[cfg(feature = "db")]
pub struct PostgresFlagSource {
    pool: tankovault_db::PgPool,
}

#[cfg(feature = "db")]
impl PostgresFlagSource {
    #[must_use]
    pub fn new(pool: tankovault_db::PgPool) -> Self {
        Self { pool }
    }
}

#[cfg(feature = "db")]
#[async_trait::async_trait]
impl FlagSource for PostgresFlagSource {
    async fn overrides(&self) -> Result<Vec<(String, bool)>, String> {
        tankovault_db::repo::flags::effective_overrides(&self.pool)
            .await
            .map_err(|e| e.to_string())
    }
}

/// The shared, refreshable answer to "is this feature on?".
///
/// `Clone` is cheap (two `Arc`s) so it can live in application state, in a middleware layer and
/// in every background loop at once, all observing the same snapshot.
#[derive(Clone)]
pub struct FeatureGate {
    snapshot: Arc<RwLock<Snapshot>>,
    source: Arc<dyn FlagSource>,
}

impl FeatureGate {
    /// A gate at the compiled defaults, reading from `source` on each [`Self::refresh`].
    ///
    /// Does not load: construction is synchronous and infallible, so a boot-time database
    /// hiccup cannot prevent the process from starting with a sane (defaults) view.
    #[must_use]
    pub fn new(source: Arc<dyn FlagSource>) -> Self {
        Self {
            snapshot: Arc::new(RwLock::new(Snapshot::from_defaults())),
            source,
        }
    }

    /// A gate pinned to the compiled defaults with no source at all.
    #[must_use]
    pub fn defaults() -> Self {
        Self::new(Arc::new(DefaultsOnly))
    }

    /// A gate with an explicit enabled set, for tests that need a feature off.
    #[must_use]
    pub fn with_disabled(disabled: &[Feature]) -> Self {
        let gate = Self::defaults();
        {
            let mut snapshot = gate.write_snapshot();
            for feature in disabled {
                snapshot.enabled.remove(feature);
            }
        }
        gate
    }

    /// A gate with named features forced on, for tests that need one that ships off.
    ///
    /// The counterpart to [`Self::with_disabled`], and not redundant with it: three features
    /// default to off, so a test exercising what they do cannot get there from the defaults.
    #[must_use]
    pub fn with_enabled(enabled: &[Feature]) -> Self {
        let gate = Self::defaults();
        {
            let mut snapshot = gate.write_snapshot();
            for feature in enabled {
                snapshot.enabled.insert(*feature);
            }
        }
        gate
    }

    /// Whether `feature` is currently enabled.
    ///
    /// Recovers from a poisoned lock rather than propagating it: the snapshot is plain data
    /// with no invariant a panic could break half-way, and refusing to answer would take the
    /// whole service down over a bookkeeping detail.
    #[must_use]
    pub fn is_enabled(&self, feature: Feature) -> bool {
        match self.snapshot.read() {
            Ok(guard) => guard.enabled.contains(&feature),
            Err(poisoned) => poisoned.into_inner().enabled.contains(&feature),
        }
    }

    /// The enabled features, for the `/v1/me/capabilities` payload the frontend gates its UI on.
    #[must_use]
    pub fn enabled_features(&self) -> Vec<Feature> {
        let guard = match self.snapshot.read() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        // Declaration order, so the serialised list is stable between calls and diffs cleanly.
        Feature::all()
            .iter()
            .copied()
            .filter(|f| guard.enabled.contains(f))
            .collect()
    }

    fn write_snapshot(&self) -> std::sync::RwLockWriteGuard<'_, Snapshot> {
        match self.snapshot.write() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    /// Reload the snapshot from the source now.
    ///
    /// Called by the flag-write endpoint so the operator's own change is live immediately,
    /// and by the refresh loop. On failure the previous snapshot stands (see module docs).
    pub async fn refresh(&self) {
        match self.source.overrides().await {
            Ok(overrides) => {
                let next = Snapshot::resolve(&overrides);
                let changed = {
                    let mut current = self.write_snapshot();
                    let changed = current.enabled != next.enabled;
                    *current = next;
                    changed
                };
                if changed {
                    // Worth a log line at info: this is a deployment-wide behaviour change and
                    // the first thing to correlate against when something starts 404ing.
                    tracing::info!("feature flag snapshot changed");
                }
            }
            Err(error) => {
                tracing::warn!(%error, "feature flag refresh failed; keeping previous snapshot");
            }
        }
    }

    /// Load once, then refresh on `interval` until `shutdown`.
    ///
    /// The initial load is awaited so the listener never accepts traffic against the
    /// compiled defaults, briefly re-enabling anything an operator had switched off.
    pub async fn spawn_refresh(&self, interval: std::time::Duration, shutdown: CancellationToken) {
        self.refresh().await;

        let gate = self.clone();
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(interval);
            // The first tick of a tokio interval completes immediately; skip it, the initial
            // load above already happened.
            ticker.tick().await;
            loop {
                tokio::select! {
                    () = shutdown.cancelled() => {
                        tracing::debug!("feature flag refresh stopping");
                        return;
                    }
                    _ = ticker.tick() => gate.refresh().await,
                }
            }
        });
    }
}

/// One route→feature declaration.
#[derive(Debug, Clone)]
struct Rule {
    prefix: String,
    feature: Feature,
    /// When set, only mutating methods (`POST`/`PUT`/`PATCH`/`DELETE`) are gated; safe reads
    /// under the same prefix fall through, so a surface stays readable while its write side
    /// is switched off.
    writes_only: bool,
    /// When set, the route pattern must equal `prefix` rather than start with it. Needed
    /// where a route's own path is a prefix of an unrelated family (e.g. `DELETE /v1/me`
    /// above `/v1/me/watchlist`), or a prefix rule would gate the whole family with it.
    exact: bool,
}

/// Maps matched route patterns to the [`Feature`] they belong to, by longest-prefix match on
/// axum's [`MatchedPath`], so a gate cannot be dodged by varying a path parameter.
///
/// A route with no rule is **ungated** — the opposite default from the rate limiter, since
/// here an unlisted route is a missing feature, not a security gap.
#[derive(Debug, Clone, Default)]
pub struct RouteFeatures {
    /// Sorted longest-prefix-first, so the first match is the most specific.
    rules: Vec<Rule>,
}

impl RouteFeatures {
    /// An empty table: nothing is gated.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Gate every request under `prefix` behind `feature`.
    #[must_use]
    pub fn gate(self, prefix: impl Into<String>, feature: Feature) -> Self {
        self.rule(prefix, feature, false, false)
    }

    /// Gate only mutating requests under `prefix`, leaving reads ungated by this rule.
    #[must_use]
    pub fn gate_writes(self, prefix: impl Into<String>, feature: Feature) -> Self {
        self.rule(prefix, feature, true, false)
    }

    /// Gate exactly `path` — not the routes beneath it.
    ///
    /// Use where a route's path is also the prefix of an unrelated family; see [`Rule::exact`].
    #[must_use]
    pub fn gate_path(self, path: impl Into<String>, feature: Feature) -> Self {
        self.rule(path, feature, false, true)
    }

    /// Gate only mutating requests to exactly `path`, leaving both its reads and every route
    /// beneath it ungated by this rule.
    ///
    /// The combination exists for the case where a family's *own* write is a feature but the
    /// operations beneath it must survive the feature being switched off — cancelling a scan run
    /// while manual scanning is disabled being the example: an operator who has just switched
    /// triggering off has usually done so because the queue is the problem, and a rule that took
    /// the stop button away with it would be the worst possible time to lose it.
    #[must_use]
    pub fn gate_path_writes(self, path: impl Into<String>, feature: Feature) -> Self {
        self.rule(path, feature, true, true)
    }

    fn rule(
        mut self,
        prefix: impl Into<String>,
        feature: Feature,
        writes_only: bool,
        exact: bool,
    ) -> Self {
        self.rules.push(Rule {
            prefix: prefix.into(),
            feature,
            writes_only,
            exact,
        });
        // Longest first, so a specific rule beats a broader one regardless of the order they
        // were registered in. Exact rules break the tie ahead of prefix rules of the same
        // length: an exact rule is by definition the more specific of the two.
        self.rules.sort_by(|a, b| {
            b.prefix
                .len()
                .cmp(&a.prefix.len())
                .then_with(|| b.exact.cmp(&a.exact))
                .then_with(|| a.prefix.cmp(&b.prefix))
        });
        self
    }

    /// The feature governing a matched route pattern and method, if any.
    #[must_use]
    pub fn required(&self, method: &Method, matched_path: &str) -> Option<Feature> {
        let is_write = !method.is_safe();
        self.rules
            .iter()
            .find(|rule| {
                let path_matches = if rule.exact {
                    matched_path == rule.prefix
                } else {
                    matched_path.starts_with(rule.prefix.as_str())
                };
                path_matches && (!rule.writes_only || is_write)
            })
            .map(|rule| rule.feature)
    }

    /// Every feature named by at least one rule.
    ///
    /// Lets a service assert at start-up that its route table and feature registry have not
    /// drifted — a feature nobody gates is a switch that silently does nothing.
    #[must_use]
    pub fn declared_features(&self) -> HashSet<Feature> {
        self.rules.iter().map(|r| r.feature).collect()
    }

    /// Every rule as a `(prefix, feature)` pair, longest prefix first.
    ///
    /// Companion to [`Self::declared_features`] for drift in the other direction: rules are
    /// keyed on path strings, so a route renamed elsewhere leaves a stale rule behind that
    /// silently ungates the route while looking present. `services/api/tests/feature_gating.rs`
    /// checks the pairs against the published document.
    pub fn rules(&self) -> impl Iterator<Item = (&str, Feature)> {
        self.rules.iter().map(|r| (r.prefix.as_str(), r.feature))
    }
}

/// The mounted gate: the flag snapshot plus the route table.
#[derive(Clone)]
pub struct FeatureLayer {
    gate: FeatureGate,
    routes: Arc<RouteFeatures>,
}

impl FeatureLayer {
    #[must_use]
    pub fn new(gate: FeatureGate, routes: RouteFeatures) -> Self {
        Self {
            gate,
            routes: Arc::new(routes),
        }
    }
}

/// The axum middleware. Mount with
/// `axum::middleware::from_fn_with_state(layer, tankovault_service::flags::enforce)`.
///
/// A disabled feature answers **`404 Not Found`**, not `403`: the resource genuinely is not
/// part of this deployment's API, and `403` would falsely imply the caller lacks permission.
/// The body names the feature so "why is this 404ing" has an answer in the response.
pub async fn enforce(State(layer): State<FeatureLayer>, req: Request, next: Next) -> Response {
    let method = req.method().clone();
    let required = req
        .extensions()
        .get::<MatchedPath>()
        .and_then(|path| layer.routes.required(&method, path.as_str()));

    if let Some(feature) = required
        && !layer.gate.is_enabled(feature)
    {
        metrics::counter!(
            crate::metrics::names::FEATURE_DISABLED,
            "feature" => feature.key()
        )
        .increment(1);
        tracing::debug!(feature = %feature, "refusing request for a disabled feature");
        return feature_disabled(feature);
    }

    next.run(req).await
}

/// The [`crate::problem::Problem::kind`] this layer answers with. Also produced by
/// `services/api`'s own error enum, which reconciles its published vocabulary against this.
pub const FEATURE_DISABLED_KIND: &str = "feature_disabled";

/// `404` with the same RFC 9457 `problem+json` shape the API's own error type produces, so a
/// client parses one error format everywhere.
fn feature_disabled(feature: Feature) -> Response {
    let body = axum::Json(serde_json::json!({
        "type": format!("about:blank#{FEATURE_DISABLED_KIND}"),
        "title": FEATURE_DISABLED_KIND,
        "status": StatusCode::NOT_FOUND.as_u16(),
        "detail": format!("the \"{}\" feature is switched off on this deployment", feature.title()),
        "feature": feature.key(),
    }));
    (StatusCode::NOT_FOUND, body).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_come_from_the_registry() {
        let gate = FeatureGate::defaults();
        assert!(gate.is_enabled(Feature::CatalogueBrowse));
        // The two third-party-egress features ship off.
        assert!(!gate.is_enabled(Feature::NotificationsDiscord));
    }

    #[test]
    fn overrides_apply_over_defaults_in_both_directions() {
        let snapshot = Snapshot::resolve(&[
            ("catalogue.browse".to_owned(), false),
            ("notifications.discord".to_owned(), true),
        ]);
        assert!(!snapshot.enabled.contains(&Feature::CatalogueBrowse));
        assert!(snapshot.enabled.contains(&Feature::NotificationsDiscord));
    }

    #[test]
    fn a_locked_feature_cannot_be_switched_off_by_a_stored_override() {
        // The API refuses to write this, but a row could exist from a hand-edited database.
        // Honouring it would remove the only way to turn anything back on.
        let snapshot = Snapshot::resolve(&[("admin.feature_flags".to_owned(), false)]);
        assert!(snapshot.enabled.contains(&Feature::AdminFeatureFlags));
    }

    #[test]
    fn an_unknown_override_key_is_ignored_not_fatal() {
        let snapshot = Snapshot::resolve(&[
            ("catalogue.teleport".to_owned(), false),
            ("catalogue.search".to_owned(), false),
        ]);
        assert!(!snapshot.enabled.contains(&Feature::CatalogueSearch));
        assert_eq!(
            snapshot.enabled.len(),
            Snapshot::from_defaults().enabled.len() - 1,
            "only the recognised override should have taken effect"
        );
    }

    #[test]
    fn unlisted_routes_are_ungated() {
        let routes = RouteFeatures::new().gate("/v1/me/watchlist", Feature::TrackingWatchlist);
        assert_eq!(routes.required(&Method::GET, "/v1/series"), None);
        assert_eq!(
            routes.required(&Method::GET, "/v1/me/watchlist"),
            Some(Feature::TrackingWatchlist)
        );
    }

    #[test]
    fn the_longest_matching_prefix_wins_regardless_of_registration_order() {
        let routes = RouteFeatures::new()
            .gate("/v1/me", Feature::TrackingFeed)
            .gate("/v1/me/sync", Feature::SyncExternal);
        assert_eq!(
            routes.required(&Method::GET, "/v1/me/sync/anilist/status"),
            Some(Feature::SyncExternal)
        );
        assert_eq!(
            routes.required(&Method::GET, "/v1/me/feed"),
            Some(Feature::TrackingFeed)
        );
    }

    #[test]
    fn write_only_rules_leave_reads_reachable() {
        // Switching off manual scans must not hide the scan history an operator needs in
        // order to understand why they switched it off.
        let routes = RouteFeatures::new().gate_writes("/v1/admin/scans", Feature::ScanningManual);
        assert_eq!(routes.required(&Method::GET, "/v1/admin/scans"), None);
        assert_eq!(
            routes.required(&Method::POST, "/v1/admin/scans"),
            Some(Feature::ScanningManual)
        );
    }

    #[test]
    fn an_exact_rule_does_not_swallow_the_routes_beneath_it() {
        // `DELETE /v1/me` is self-service erasure; `/v1/me/watchlist` is not. A prefix rule
        // here would take the entire user surface down with one flag.
        let routes = RouteFeatures::new()
            .gate_path("/v1/me", Feature::PrivacySelfErasure)
            .gate("/v1/me/watchlist", Feature::TrackingWatchlist);
        assert_eq!(
            routes.required(&Method::DELETE, "/v1/me"),
            Some(Feature::PrivacySelfErasure)
        );
        assert_eq!(
            routes.required(&Method::GET, "/v1/me/watchlist"),
            Some(Feature::TrackingWatchlist)
        );
        assert_eq!(routes.required(&Method::GET, "/v1/me/feed"), None);
    }

    #[test]
    fn an_exact_rule_beats_a_prefix_rule_of_the_same_length() {
        let routes = RouteFeatures::new()
            .gate("/v1/me", Feature::TrackingFeed)
            .gate_path("/v1/me", Feature::PrivacySelfErasure);
        assert_eq!(
            routes.required(&Method::DELETE, "/v1/me"),
            Some(Feature::PrivacySelfErasure)
        );
        // …while everything below it still falls to the broader rule.
        assert_eq!(
            routes.required(&Method::GET, "/v1/me/feed"),
            Some(Feature::TrackingFeed)
        );
    }

    #[test]
    fn with_disabled_turns_exactly_the_named_features_off() {
        let gate = FeatureGate::with_disabled(&[Feature::TrackingWatchlist]);
        assert!(!gate.is_enabled(Feature::TrackingWatchlist));
        assert!(gate.is_enabled(Feature::TrackingProgress));
    }

    #[test]
    fn enabled_features_are_listed_in_registry_order() {
        let gate = FeatureGate::defaults();
        let listed = gate.enabled_features();
        let expected: Vec<Feature> = Feature::all()
            .iter()
            .copied()
            .filter(|f| f.default_enabled())
            .collect();
        assert_eq!(listed, expected);
    }

    struct Failing;

    #[async_trait::async_trait]
    impl FlagSource for Failing {
        async fn overrides(&self) -> Result<Vec<(String, bool)>, String> {
            Err("connection refused".to_owned())
        }
    }

    #[tokio::test]
    async fn a_failed_refresh_keeps_the_previous_snapshot() {
        // The important case: an operator has switched something off, then the database
        // becomes briefly unreachable. Reverting to defaults would silently turn it back on.
        let gate = FeatureGate::new(Arc::new(Failing));
        {
            let mut snapshot = gate.write_snapshot();
            snapshot.enabled.remove(&Feature::CatalogueSearch);
        }
        gate.refresh().await;
        assert!(!gate.is_enabled(Feature::CatalogueSearch));
    }

    struct Fixed(Vec<(String, bool)>);

    #[async_trait::async_trait]
    impl FlagSource for Fixed {
        async fn overrides(&self) -> Result<Vec<(String, bool)>, String> {
            Ok(self.0.clone())
        }
    }

    #[tokio::test]
    async fn refresh_adopts_the_sources_overrides() {
        let gate = FeatureGate::new(Arc::new(Fixed(vec![("tracking.stats".to_owned(), false)])));
        assert!(gate.is_enabled(Feature::TrackingStats));
        gate.refresh().await;
        assert!(!gate.is_enabled(Feature::TrackingStats));
    }

    /// A `DefaultsOnly` that returned one override would silently move the whole fleet off
    /// its defaults, with nothing to notice.
    #[tokio::test]
    async fn defaults_only_contributes_no_overrides() {
        assert_eq!(DefaultsOnly.overrides().await, Ok(Vec::new()));
    }

    /// `spawn_refresh` awaits the first load before returning — otherwise the first requests
    /// after a deploy would be served against compiled defaults. Shutdown is cancelled up
    /// front so only the initial load can have run by the time the assertion executes.
    #[tokio::test]
    async fn spawn_refresh_loads_once_before_it_returns() {
        let gate = FeatureGate::new(Arc::new(Fixed(vec![("tracking.stats".to_owned(), false)])));
        let shutdown = CancellationToken::new();
        shutdown.cancel();
        gate.spawn_refresh(std::time::Duration::from_secs(3600), shutdown)
            .await;
        assert!(!gate.is_enabled(Feature::TrackingStats));
    }

    /// The two drift accessors answer opposite questions and both must report the real table.
    #[test]
    fn the_drift_accessors_report_the_whole_table() {
        let routes = RouteFeatures::new()
            .gate("/v1/me/watchlist", Feature::TrackingWatchlist)
            .gate_writes("/v1/admin/scans", Feature::ScanningManual)
            .gate_path("/v1/me", Feature::PrivacySelfErasure);

        assert_eq!(
            routes.declared_features(),
            HashSet::from([
                Feature::TrackingWatchlist,
                Feature::ScanningManual,
                Feature::PrivacySelfErasure,
            ])
        );

        let mut pairs: Vec<_> = routes.rules().collect();
        pairs.sort_unstable();
        assert_eq!(
            pairs,
            [
                ("/v1/admin/scans", Feature::ScanningManual),
                ("/v1/me", Feature::PrivacySelfErasure),
                ("/v1/me/watchlist", Feature::TrackingWatchlist),
            ]
        );
    }

    /// The middleware's own contract: a disabled route answers `404` (see [`enforce`] for why
    /// not `403`) with an RFC 9457 body naming the feature; an enabled or ungated route reaches
    /// its handler.
    #[tokio::test]
    async fn a_disabled_route_answers_404_naming_the_feature() {
        use axum::Router;
        use axum::body::Body;
        use axum::routing::get;
        use tower::ServiceExt as _;

        async fn get_path(app: &Router, path: &str) -> Response {
            app.clone()
                .oneshot(
                    Request::builder()
                        .uri(path)
                        .body(Body::empty())
                        .expect("a well-formed request"),
                )
                .await
                .expect("the router is infallible")
        }

        let layer = FeatureLayer::new(
            FeatureGate::with_disabled(&[Feature::TrackingStats]),
            RouteFeatures::new()
                .gate("/v1/me/stats", Feature::TrackingStats)
                .gate("/v1/me/watchlist", Feature::TrackingWatchlist),
        );
        let app = Router::new()
            .route("/v1/me/stats", get(|| async { "stats" }))
            .route("/v1/me/watchlist", get(|| async { "watchlist" }))
            .route("/v1/series", get(|| async { "series" }))
            .layer(axum::middleware::from_fn_with_state(layer, enforce));

        let refused = get_path(&app, "/v1/me/stats").await;
        assert_eq!(refused.status(), StatusCode::NOT_FOUND);
        let body = axum::body::to_bytes(refused.into_body(), 4096)
            .await
            .expect("a bounded body");
        let problem: serde_json::Value =
            serde_json::from_slice(&body).expect("an RFC 9457 problem document");
        assert_eq!(problem["title"], "feature_disabled");
        assert_eq!(problem["status"], 404);
        assert_eq!(problem["feature"], Feature::TrackingStats.key());

        // The inverse leg: a gate that answered 404 unconditionally would pass everything
        // above. An enabled gated route and an ungated one both reach their handler.
        for enabled in ["/v1/me/watchlist", "/v1/series"] {
            assert_eq!(get_path(&app, enabled).await.status(), StatusCode::OK);
        }
    }
}
