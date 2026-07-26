//! The runtime feature gate: resolving [`Feature`] state and enforcing it on HTTP routes.
//!
//! # How this differs from the other toggles in this crate
//!
//! Metrics, audit and rate limiting are *wiring* decisions, fixed at boot (see the crate
//! docs). A feature flag is the opposite by requirement: an operator flips it in the control
//! plane and the running fleet has to follow, so it is necessarily consulted per request.
//!
//! What keeps that from becoming `if flag { }` sprinkled through handlers is that the
//! consultation is **declarative**. [`RouteFeatures`] is a table, sitting next to the route
//! registration, that says which feature each route family belongs to; [`enforce`] is the one
//! place that reads it. A handler contains no flag logic at all. Background loops, which have
//! no route to declare against, call [`FeatureGate::is_enabled`] once per iteration — there the
//! loop *is* the feature, so the check is the same declaration in a different shape.
//!
//! # Snapshot, not a query per request
//!
//! [`FeatureGate`] holds an immutable snapshot behind an `RwLock` and refreshes it on a timer
//! ([`FeatureGate::spawn_refresh`]). Reads take the lock for long enough to test set membership
//! and nothing else. The service that *serves* a flag change refreshes immediately
//! ([`FeatureGate::refresh`]), so an operator sees their own change take effect at once; other
//! replicas converge within one refresh interval. That staleness window is the deliberate cost
//! of not putting a database round trip in front of every request, and it is bounded by
//! [`FeaturesConfig::refresh_secs`](tankovault_config::FeaturesConfig::refresh_secs).
//!
//! # Failure behaviour
//!
//! A refresh that cannot reach the database keeps the previous snapshot and logs. It does not
//! fall back to the compiled defaults, because that would silently *re-enable* whatever an
//! operator had switched off — a database blip must not undo a deliberate decision. The only
//! time defaults apply is at construction, before any successful load.

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
///
/// Stores the *enabled* set rather than the overrides, so a lookup is one hash probe with no
/// default-resolution logic on the hot path. Resolution happens once, at refresh.
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
    /// An override naming a feature this build does not have is ignored with a warning: the
    /// schema and the binary disagree, which happens across a rollback and must not be fatal.
    /// A **locked** feature's override is also ignored — the API refuses to write one, but a
    /// row could exist from a hand-edited database or an older build, and honouring it would
    /// remove the deployment's only recovery path.
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
///
/// A trait rather than a `PgPool` so the gate is constructible in a test and in a service
/// without a database, and so `tankovault-service` does not need the `db` feature just to
/// enforce flags. The API service supplies the Postgres-backed implementation.
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
    /// Does not load: construction is synchronous and infallible so a service can build its
    /// state before it has an async context, and a boot-time database hiccup cannot prevent
    /// the process from starting with a sane (defaults) view.
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

    /// Whether `feature` is currently enabled.
    ///
    /// A poisoned lock means a previous holder panicked while replacing the snapshot. Rather
    /// than propagate that, this recovers the inner value: the snapshot is plain data with no
    /// invariant a panic could have broken half-way, and refusing to answer would take the
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
    /// Called by the flag-write endpoint so the operator's own change is live before their
    /// request returns, and by the refresh loop. On failure the previous snapshot stands — see
    /// the module docs on why a database outage must not revert to defaults.
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
    /// The initial load is awaited so the caller can be sure the gate reflects stored overrides
    /// before the listener starts accepting traffic — otherwise the first requests after a
    /// deploy would be served against the defaults, briefly re-enabling anything switched off.
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
    /// under the same prefix fall through. This is what lets a surface stay *readable* while
    /// its write side is switched off — an operator disabling manual scans should still be able
    /// to look at the scan history.
    writes_only: bool,
    /// When set, the route pattern must equal `prefix` rather than start with it.
    ///
    /// Needed wherever a route's own path is a prefix of an unrelated family: `DELETE /v1/me`
    /// (self-service erasure) sits above `/v1/me/watchlist`, `/v1/me/feed` and a dozen others,
    /// so a prefix rule there would take the entire user surface down with it.
    exact: bool,
}

/// Maps matched route patterns to the [`Feature`] they belong to, by longest-prefix match.
///
/// Matching is on axum's [`MatchedPath`] — the route *pattern*, `/v1/series/{id}` — so a gate
/// cannot be dodged by varying a path parameter. Mirrors
/// [`RouteClassifier`](crate::RouteClassifier) deliberately: two tables with the same matching
/// semantics are two tables a reader only has to learn once.
///
/// A route with no rule is **ungated**. That is the opposite default from the rate limiter, and
/// it is the right one here: an unlisted route being unlimited is a security gap, whereas an
/// unlisted route being un-flaggable is merely a missing feature. Making the default
/// "disabled unless declared" would take the whole API down the moment someone added an
/// endpoint.
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
    /// Exists so a service can assert at start-up that its route table and the feature registry
    /// have not drifted — a feature nobody gates is a switch that does nothing, which is worse
    /// than no switch at all because an operator will believe it worked.
    #[must_use]
    pub fn declared_features(&self) -> HashSet<Feature> {
        self.rules.iter().map(|r| r.feature).collect()
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
/// part of this deployment's API, and `403` would tell the caller they lack permission — which
/// is false and would send a user to an administrator who cannot help them. The body names the
/// feature so an operator debugging "why is this 404ing" gets the answer from the response
/// rather than from the flag page.
pub async fn enforce(State(layer): State<FeatureLayer>, req: Request, next: Next) -> Response {
    let method = req.method().clone();
    let required = req
        .extensions()
        .get::<MatchedPath>()
        .and_then(|path| layer.routes.required(&method, path.as_str()));

    if let Some(feature) = required {
        if !layer.gate.is_enabled(feature) {
            metrics::counter!("http_feature_disabled_total", "feature" => feature.key())
                .increment(1);
            tracing::debug!(feature = %feature, "refusing request for a disabled feature");
            return feature_disabled(feature);
        }
    }

    next.run(req).await
}

/// `404` with the same RFC 9457 `problem+json` shape the API's own error type produces, so a
/// client parses one error format everywhere.
fn feature_disabled(feature: Feature) -> Response {
    let body = axum::Json(serde_json::json!({
        "type": "about:blank#feature_disabled",
        "title": "feature_disabled",
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
}
