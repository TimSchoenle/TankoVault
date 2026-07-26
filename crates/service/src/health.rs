//! Liveness and readiness probes.
//!
//! The distinction matters operationally and was previously collapsed: every service
//! answered both `/health` and `/ready` with a literal `"ok"`, so an orchestrator kept
//! routing traffic to a replica whose database had gone away, and a wedged replica was
//! never restarted.
//!
//! - **`/health` (liveness)** — the process is running and its executor is responsive.
//!   Deliberately checks nothing external: a failing dependency must not cause a restart
//!   loop that makes the outage worse.
//! - **`/ready` (readiness)** — every registered dependency is reachable *right now*.
//!   Failing here removes the replica from the load balancer without killing it.

use async_trait::async_trait;
use std::sync::Arc;
use std::time::Duration;

/// How long a single dependency check may take before it is treated as failed. A probe
/// that hangs is indistinguishable from one that fails, and the orchestrator's own probe
/// timeout is the only thing that would otherwise bound it.
const CHECK_TIMEOUT: Duration = Duration::from_secs(2);

/// One readiness dependency.
#[async_trait]
pub trait HealthCheck: Send + Sync + 'static {
    /// Stable identifier reported in the probe body (`postgres`, `nats`, ...).
    fn name(&self) -> &'static str;

    /// Probe the dependency. `Err` carries a short operator-facing reason.
    async fn check(&self) -> Result<(), String>;
}

/// Outcome of a readiness probe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum HealthStatus {
    /// Every dependency answered.
    Up,
    /// At least one dependency failed or timed out.
    Down,
}

/// The result of one dependency's probe, as rendered into the `/ready` body.
#[derive(Debug, Clone, serde::Serialize)]
pub struct DependencyReport {
    /// Dependency name, from [`HealthCheck::name`].
    pub name: &'static str,
    /// Whether this dependency answered.
    pub status: HealthStatus,
    /// Failure reason; absent when the dependency is up.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// Aggregate readiness across every registered dependency.
#[derive(Debug, Clone, serde::Serialize)]
pub struct HealthReport {
    /// `Down` if any dependency is down.
    pub status: HealthStatus,
    /// Per-dependency detail, so an operator sees *which* dependency is at fault without
    /// correlating logs.
    pub checks: Vec<DependencyReport>,
}

impl HealthReport {
    /// Whether the replica should receive traffic.
    #[must_use]
    pub fn is_ready(&self) -> bool {
        self.status == HealthStatus::Up
    }
}

/// A cheap, cloneable set of readiness checks.
#[derive(Clone, Default)]
pub struct Health {
    checks: Arc<Vec<Arc<dyn HealthCheck>>>,
}

impl Health {
    /// Start registering dependencies.
    #[must_use]
    pub fn builder() -> HealthBuilder {
        HealthBuilder::default()
    }

    /// Probe every dependency concurrently and aggregate the results.
    ///
    /// Checks run in parallel and each is bounded by [`CHECK_TIMEOUT`], so total probe
    /// latency is that timeout rather than its sum over dependencies.
    pub async fn report(&self) -> HealthReport {
        let probes = self.checks.iter().map(|check| async move {
            let name = check.name();
            let detail = match tokio::time::timeout(CHECK_TIMEOUT, check.check()).await {
                Ok(Ok(())) => None,
                Ok(Err(reason)) => Some(reason),
                Err(_) => Some(format!("timed out after {}s", CHECK_TIMEOUT.as_secs())),
            };
            DependencyReport {
                name,
                status: if detail.is_none() {
                    HealthStatus::Up
                } else {
                    HealthStatus::Down
                },
                detail,
            }
        });

        let checks: Vec<DependencyReport> = futures::future::join_all(probes).await;
        let status = if checks.iter().all(|c| c.status == HealthStatus::Up) {
            HealthStatus::Up
        } else {
            HealthStatus::Down
        };
        HealthReport { status, checks }
    }
}

/// Accumulates readiness dependencies.
#[derive(Default)]
pub struct HealthBuilder {
    checks: Vec<Arc<dyn HealthCheck>>,
}

impl HealthBuilder {
    /// Register a dependency implementing [`HealthCheck`].
    #[must_use]
    pub fn check(mut self, check: impl HealthCheck) -> Self {
        self.checks.push(Arc::new(check));
        self
    }

    /// Register a dependency from a closure, for one-off probes that do not warrant a
    /// named type (a NATS ping, an upstream `HEAD` request).
    #[must_use]
    pub fn check_fn<F, Fut>(self, name: &'static str, probe: F) -> Self
    where
        F: Fn() -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<(), String>> + Send + 'static,
    {
        self.check(FnCheck { name, probe })
    }

    /// Finish registration.
    #[must_use]
    pub fn build(self) -> Health {
        Health {
            checks: Arc::new(self.checks),
        }
    }
}

/// Closure-backed [`HealthCheck`], built by [`HealthBuilder::check_fn`].
struct FnCheck<F> {
    name: &'static str,
    probe: F,
}

#[async_trait]
impl<F, Fut> HealthCheck for FnCheck<F>
where
    F: Fn() -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<(), String>> + Send + 'static,
{
    fn name(&self) -> &'static str {
        self.name
    }

    async fn check(&self) -> Result<(), String> {
        (self.probe)().await
    }
}

/// Readiness check for the Postgres pool.
///
/// Runs `SELECT 1` rather than inspecting pool counters: an idle pool reports healthy
/// even when the server behind it is gone, which is precisely the failure this exists to
/// catch. Acquiring a connection also exercises the pool's own timeout.
#[cfg(feature = "db")]
pub struct PostgresCheck {
    pool: tankovault_db::PgPool,
}

#[cfg(feature = "db")]
impl PostgresCheck {
    /// Probe `pool` on every readiness request.
    #[must_use]
    pub fn new(pool: tankovault_db::PgPool) -> Self {
        Self { pool }
    }
}

#[cfg(feature = "db")]
#[async_trait]
impl HealthCheck for PostgresCheck {
    fn name(&self) -> &'static str {
        "postgres"
    }

    async fn check(&self) -> Result<(), String> {
        sqlx::query_scalar::<_, i32>("SELECT 1")
            .fetch_one(&self.pool)
            .await
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn empty_registry_is_ready() {
        // A service with no external dependencies (challenge-solver) is ready as soon as
        // it is listening.
        let report = Health::builder().build().report().await;
        assert!(report.is_ready());
        assert!(report.checks.is_empty());
    }

    #[tokio::test]
    async fn one_failing_dependency_fails_the_probe() {
        let health = Health::builder()
            .check_fn("up", || async { Ok(()) })
            .check_fn("down", || async { Err("connection refused".to_owned()) })
            .build();

        let report = health.report().await;
        assert!(!report.is_ready());
        assert_eq!(report.checks.len(), 2);

        let failed = report
            .checks
            .iter()
            .find(|c| c.name == "down")
            .expect("the failing check is reported");
        assert_eq!(failed.status, HealthStatus::Down);
        assert_eq!(failed.detail.as_deref(), Some("connection refused"));
    }

    #[tokio::test(start_paused = true)]
    async fn a_hanging_dependency_times_out_rather_than_hanging_the_probe() {
        let health = Health::builder()
            .check_fn("stuck", || async {
                tokio::time::sleep(Duration::from_secs(600)).await;
                Ok(())
            })
            .build();

        let report = health.report().await;
        assert!(!report.is_ready());
        assert!(
            report.checks[0]
                .detail
                .as_deref()
                .is_some_and(|d| d.contains("timed out"))
        );
    }
}
