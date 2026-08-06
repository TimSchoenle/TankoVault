//! The runtime tuning snapshot: resolving [`Tunable`] values and handing them to the pipeline
//! already clamped.
//!
//! Sits beside [`crate::flags::FeatureGate`] and refreshes on the same timer, with the same
//! rule: **a failed refresh keeps the previous snapshot** rather than reverting to compiled
//! defaults, which would silently discard everything an operator had tuned.
//!
//! Every read clamps to the registry's range. The API refuses an out-of-range write, so a row
//! outside it should not exist — but "should not exist" is not "cannot exist", and one of these
//! bounds (`recsys.cooccurrence.min_support`) is a k-anonymity threshold. Clamping here is what
//! makes that bound hold against a hand-edited database as well as against a bad request.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use tankovault_domain::Tunable;
use tokio_util::sync::CancellationToken;

/// Resolved values, keyed by tunable. Cheap to replace wholesale.
#[derive(Debug, Clone, Default)]
struct Snapshot {
    values: HashMap<Tunable, f64>,
}

impl Snapshot {
    /// Every tunable at its compiled default. The state before any successful load.
    fn from_defaults() -> Self {
        Self {
            values: Tunable::all()
                .iter()
                .map(|&t| (t, t.default_value()))
                .collect(),
        }
    }

    /// Apply stored overrides on top of the compiled defaults.
    ///
    /// An unknown key is ignored, not fatal (schema and binary can disagree across a rollback).
    /// An out-of-range value is clamped rather than dropped: the operator's *intent* was to move
    /// in that direction, and honouring it as far as the range allows is closer to that intent
    /// than silently reverting to the shipped value.
    fn resolve(overrides: &[(String, f64)]) -> Self {
        let mut snapshot = Self::from_defaults();
        for (key, value) in overrides {
            let Ok(tunable) = key.parse::<Tunable>() else {
                tracing::warn!(%key, "ignoring override for unknown tunable");
                continue;
            };
            let spec = tunable.spec();
            let clamped = spec.clamp(*value);
            if (clamped - *value).abs() > f64::EPSILON {
                tracing::warn!(
                    tunable = %tunable,
                    stored = *value,
                    used = clamped,
                    "stored tunable is outside its permitted range; clamping"
                );
            }
            snapshot.values.insert(tunable, clamped);
        }
        snapshot
    }
}

/// Where a [`TunableSet`] reads overrides from.
#[async_trait::async_trait]
pub trait TunableSource: Send + Sync + 'static {
    /// The currently stored `(key, value)` overrides.
    ///
    /// # Errors
    /// Any failure is returned as a message; the set logs it and keeps its previous snapshot.
    async fn overrides(&self) -> Result<Vec<(String, f64)>, String>;
}

/// A source with no overrides, so every tunable sits at its compiled default.
///
/// Used by services that consume tuning but hold no database handle, and by tests.
pub struct TunableDefaultsOnly;

#[async_trait::async_trait]
impl TunableSource for TunableDefaultsOnly {
    async fn overrides(&self) -> Result<Vec<(String, f64)>, String> {
        Ok(Vec::new())
    }
}

/// The real source: the `tunable_overrides` table.
#[cfg(feature = "db")]
pub struct PostgresTunableSource {
    pool: tankovault_db::PgPool,
}

#[cfg(feature = "db")]
impl PostgresTunableSource {
    #[must_use]
    pub fn new(pool: tankovault_db::PgPool) -> Self {
        Self { pool }
    }
}

#[cfg(feature = "db")]
#[async_trait::async_trait]
impl TunableSource for PostgresTunableSource {
    async fn overrides(&self) -> Result<Vec<(String, f64)>, String> {
        tankovault_db::repo::tunables::effective_overrides(&self.pool)
            .await
            .map_err(|e| e.to_string())
    }
}

/// The shared, refreshable answer to "what is this value right now?".
///
/// `Clone` is cheap (two `Arc`s) so it can live in application state, in the request path and in
/// the builder loop at once, all observing the same snapshot.
#[derive(Clone)]
pub struct TunableSet {
    snapshot: Arc<RwLock<Snapshot>>,
    source: Arc<dyn TunableSource>,
}

impl TunableSet {
    /// A set at the compiled defaults, reading from `source` on each [`Self::refresh`].
    ///
    /// Does not load: construction is synchronous and infallible, so a boot-time database
    /// hiccup cannot stop the process starting with a sane (defaults) view.
    #[must_use]
    pub fn new(source: Arc<dyn TunableSource>) -> Self {
        Self {
            snapshot: Arc::new(RwLock::new(Snapshot::from_defaults())),
            source,
        }
    }

    /// A set pinned to the compiled defaults with no source at all.
    #[must_use]
    pub fn defaults() -> Self {
        Self::new(Arc::new(TunableDefaultsOnly))
    }

    /// A set with explicit values, for tests that need a knob moved.
    ///
    /// Values are clamped exactly as a stored override would be, so a test cannot assert against
    /// a value the production reader would never produce.
    #[must_use]
    pub fn with_values(values: &[(Tunable, f64)]) -> Self {
        let set = Self::defaults();
        {
            let mut snapshot = set.write_snapshot();
            for (tunable, value) in values {
                snapshot
                    .values
                    .insert(*tunable, tunable.spec().clamp(*value));
            }
        }
        set
    }

    /// The current value of `tunable`, clamped to its registry range.
    ///
    /// Recovers from a poisoned lock rather than propagating it: the snapshot is plain data with
    /// no invariant a panic could break half-way, and refusing to answer would take the service
    /// down over a bookkeeping detail.
    #[must_use]
    pub fn get(&self, tunable: Tunable) -> f64 {
        let guard = match self.snapshot.read() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        let raw = guard
            .values
            .get(&tunable)
            .copied()
            .unwrap_or_else(|| tunable.default_value());
        // Clamped again on the way out, not only on the way in: this is the single choke point
        // every consumer goes through, so a future path that writes the snapshot without
        // resolving still cannot leak a value past a bound.
        tunable.spec().clamp(raw)
    }

    /// [`Self::get`] as an `f32`, for the ranking maths, which is `f32` throughout.
    #[must_use]
    #[expect(
        clippy::cast_possible_truncation,
        reason = "every tunable's range is far inside f32; the narrowing is the point"
    )]
    pub fn get_f32(&self, tunable: Tunable) -> f32 {
        self.get(tunable) as f32
    }

    /// [`Self::get`] rounded to a count, saturating at both ends.
    #[must_use]
    pub fn get_usize(&self, tunable: Tunable) -> usize {
        let rounded = self.get(tunable).round();
        if rounded <= 0.0 {
            return 0;
        }
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "clamped to the registry range and checked positive immediately above"
        )]
        let value = rounded as u64;
        usize::try_from(value).unwrap_or(usize::MAX)
    }

    /// [`Self::get`] rounded to an `i64`, for query limits.
    #[must_use]
    pub fn get_i64(&self, tunable: Tunable) -> i64 {
        #[expect(
            clippy::cast_possible_truncation,
            reason = "no tunable's range approaches i64, and the value is clamped to it"
        )]
        let value = self.get(tunable).round() as i64;
        value
    }

    /// [`Self::get`] rounded to an `i32`, for index parameters and day counts.
    #[must_use]
    pub fn get_i32(&self, tunable: Tunable) -> i32 {
        i32::try_from(self.get_i64(tunable)).unwrap_or(i32::MAX)
    }

    fn write_snapshot(&self) -> std::sync::RwLockWriteGuard<'_, Snapshot> {
        match self.snapshot.write() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    /// Reload the snapshot from the source now.
    ///
    /// Called by the tuning-write endpoint so the operator's own change is live immediately, and
    /// by the refresh loop. On failure the previous snapshot stands (see module docs).
    pub async fn refresh(&self) {
        match self.source.overrides().await {
            Ok(overrides) => {
                let next = Snapshot::resolve(&overrides);
                let changed = {
                    let mut current = self.write_snapshot();
                    let changed = current.values != next.values;
                    *current = next;
                    changed
                };
                if changed {
                    tracing::info!("recommendation tuning snapshot changed");
                }
            }
            Err(error) => {
                tracing::warn!(%error, "tunable refresh failed; keeping previous snapshot");
            }
        }
    }

    /// Load once, then refresh on `interval` until `shutdown`.
    ///
    /// The initial load is awaited so the listener never serves a shelf built from compiled
    /// defaults while the operator's stored tuning sits unread.
    pub async fn spawn_refresh(&self, interval: std::time::Duration, shutdown: CancellationToken) {
        self.refresh().await;

        let set = self.clone();
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(interval);
            // The first tick of a tokio interval completes immediately; skip it, the initial
            // load above already happened.
            ticker.tick().await;
            loop {
                tokio::select! {
                    () = shutdown.cancelled() => {
                        tracing::debug!("tunable refresh stopping");
                        return;
                    }
                    _ = ticker.tick() => set.refresh().await,
                }
            }
        });
    }
}

#[cfg(test)]
#[expect(
    clippy::float_cmp,
    reason = "these compare against exact registry bounds and clamp results, not \n              computed values: a clamp returns its bound bit-for-bit, and a default \n              is the literal in the registry"
)]
mod tests {
    use super::*;

    #[test]
    fn defaults_come_from_the_registry() {
        let set = TunableSet::defaults();
        for &t in Tunable::all() {
            assert!(
                (set.get(t) - t.default_value()).abs() < f64::EPSILON,
                "{t} did not default to its registry value"
            );
        }
    }

    #[test]
    fn overrides_apply_over_defaults() {
        let snapshot = Snapshot::resolve(&[("recsys.diversity.lambda".to_owned(), 0.2)]);
        assert_eq!(snapshot.values[&Tunable::DiversityLambda], 0.2);
        assert_eq!(
            snapshot.values[&Tunable::ServeShelfSize],
            Tunable::ServeShelfSize.default_value()
        );
    }

    /// **The k-anonymity floor must survive a row the API would never have written.**
    ///
    /// The bug this pins: enforcing `min_support >= 5` only in the request handler. A row
    /// written by a hand-edited database, a restored backup or a rollback to a build with a
    /// wider range would then publish co-occurrence edges backed by a single reader — a
    /// membership disclosure with nothing anywhere reporting a problem.
    #[test]
    fn a_stored_value_below_the_privacy_floor_still_reads_as_the_floor() {
        let set = TunableSet::new(Arc::new(Fixed(vec![(
            "recsys.cooccurrence.min_support".to_owned(),
            1.0,
        )])));
        let snapshot = Snapshot::resolve(&[("recsys.cooccurrence.min_support".to_owned(), 1.0)]);
        assert_eq!(snapshot.values[&Tunable::CooccurrenceMinSupport], 5.0);
        // And through the accessor a caller would actually use.
        assert_eq!(set.get_usize(Tunable::CooccurrenceMinSupport), 5);
    }

    #[test]
    fn an_unknown_override_key_is_ignored_not_fatal() {
        let snapshot = Snapshot::resolve(&[
            ("recsys.diversity.teleport".to_owned(), 3.0),
            ("recsys.diversity.lambda".to_owned(), 0.1),
        ]);
        assert_eq!(snapshot.values[&Tunable::DiversityLambda], 0.1);
        assert_eq!(snapshot.values.len(), Tunable::all().len());
    }

    #[test]
    fn out_of_range_stored_values_are_clamped_in_both_directions() {
        let snapshot = Snapshot::resolve(&[
            ("recsys.diversity.lambda".to_owned(), 42.0),
            ("recsys.affinity.dropped.floor".to_owned(), -9.0),
        ]);
        assert_eq!(snapshot.values[&Tunable::DiversityLambda], 1.0);
        assert_eq!(snapshot.values[&Tunable::AffinityDroppedFloor], -1.0);
    }

    #[test]
    fn counts_round_rather_than_truncate() {
        let set = TunableSet::with_values(&[
            (Tunable::RetrievalSeeds, 7.6),
            (Tunable::ServeShelfSize, 12.4),
        ]);
        assert_eq!(set.get_usize(Tunable::RetrievalSeeds), 8);
        assert_eq!(set.get_i64(Tunable::ServeShelfSize), 12);
    }

    /// A ratio floored at zero must be readable as zero, not silently lifted to one — that is
    /// how "switch this path off" is expressed.
    #[test]
    fn a_zero_valued_count_reads_as_zero() {
        let set = TunableSet::with_values(&[(Tunable::RetrievalCooccurrenceSeeds, 0.0)]);
        assert_eq!(set.get_usize(Tunable::RetrievalCooccurrenceSeeds), 0);
    }

    struct Failing;

    #[async_trait::async_trait]
    impl TunableSource for Failing {
        async fn overrides(&self) -> Result<Vec<(String, f64)>, String> {
            Err("connection refused".to_owned())
        }
    }

    /// **A failed refresh keeps the previous snapshot.**
    ///
    /// The bug this pins: treating a query error as "no overrides". An operator has narrowed the
    /// diversity lambda, the database blips, and every replica silently reverts to the shipped
    /// value — a deployment-wide behaviour change caused by a transient error, with nothing but
    /// a warning line to say it happened.
    #[tokio::test]
    async fn a_failed_refresh_keeps_the_previous_snapshot() {
        let set = TunableSet::new(Arc::new(Failing));
        {
            let mut snapshot = set.write_snapshot();
            snapshot.values.insert(Tunable::DiversityLambda, 0.15);
        }
        set.refresh().await;
        assert_eq!(
            set.get(Tunable::DiversityLambda),
            0.15,
            "a failed refresh must not revert to the compiled default"
        );
    }

    struct Fixed(Vec<(String, f64)>);

    #[async_trait::async_trait]
    impl TunableSource for Fixed {
        async fn overrides(&self) -> Result<Vec<(String, f64)>, String> {
            Ok(self.0.clone())
        }
    }

    #[tokio::test]
    async fn refresh_adopts_the_sources_overrides() {
        let set = TunableSet::new(Arc::new(Fixed(vec![(
            "recsys.serve.shelf_size".to_owned(),
            25.0,
        )])));
        // Read from the registry rather than written out: what this pins is that a refresh
        // *adopts* the source's value, not what any particular knob ships as. Hard-coding the
        // default made a deliberate change to it look like a broken refresh.
        #[expect(
            clippy::cast_possible_truncation,
            reason = "a `Count` tunable's default is a small whole number by construction"
        )]
        let shipped = Tunable::ServeShelfSize.default_value() as i64;
        assert_ne!(shipped, 25, "the fixture must differ from the default");
        assert_eq!(set.get_i64(Tunable::ServeShelfSize), shipped);
        set.refresh().await;
        assert_eq!(set.get_i64(Tunable::ServeShelfSize), 25);
    }

    /// A `TunableDefaultsOnly` that returned one override would move the whole fleet off its
    /// defaults with nothing to notice.
    #[tokio::test]
    async fn defaults_only_contributes_no_overrides() {
        assert_eq!(TunableDefaultsOnly.overrides().await, Ok(Vec::new()));
    }

    /// `spawn_refresh` awaits the first load before returning, or the first requests after a
    /// deploy would be served against compiled defaults. Shutdown is cancelled up front so only
    /// the initial load can have run by the time the assertion executes.
    #[tokio::test]
    async fn spawn_refresh_loads_once_before_it_returns() {
        let set = TunableSet::new(Arc::new(Fixed(vec![(
            "recsys.serve.shelf_size".to_owned(),
            30.0,
        )])));
        let shutdown = CancellationToken::new();
        shutdown.cancel();
        set.spawn_refresh(std::time::Duration::from_secs(3600), shutdown)
            .await;
        assert_eq!(set.get_i64(Tunable::ServeShelfSize), 30);
    }
}
