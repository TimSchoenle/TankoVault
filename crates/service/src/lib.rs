//! # tankovault-service
//!
//! The production runtime every `TankoVault` service shares: process bootstrap, graceful
//! shutdown, health probes, the HTTP middleware stack, rate limiting, the audit trail, and
//! metrics. Each of auditing, metrics and rate limiting is switchable from configuration as
//! a wiring decision, not a call-site branch.
//!
//! ## Composition
//!
//! ```no_run
//! # use std::sync::Arc;
//! # use tankovault_service::{Health, HttpStack, MetricsRegistry, ServiceError};
//! # async fn example() -> Result<(), ServiceError> {
//! # let router = axum::Router::new();
//! # let security = Default::default();
//! let metrics = MetricsRegistry::install(&Default::default(), "example")?;
//! let shutdown = tankovault_service::install_shutdown();
//! let health = Health::builder().build();
//!
//! let app = HttpStack::new(&security, metrics.clone())
//!     .apply(router)
//!     .merge(tankovault_service::ops_router(health, metrics));
//!
//! tankovault_service::serve("0.0.0.0:8080", app, shutdown).await?;
//! # Ok(())
//! # }
//! ```

pub mod audit;
pub mod flags;
pub mod health;
pub mod healthcheck;
pub mod http;
pub mod internal_auth;
pub mod metrics;
pub mod problem;
pub mod ratelimit;
pub mod reload;
pub mod shutdown;
pub mod telemetry;
pub mod tunables;

pub use audit::{AuditEvent, AuditOutcome, AuditSink, NoopAuditSink};
pub use flags::{FeatureGate, FeatureLayer, FlagSource, RouteFeatures};
pub use health::{Health, HealthBuilder, HealthReport, HealthStatus};
pub use healthcheck::{HEALTHCHECK_FLAG, run_and_exit as run_healthcheck_and_exit};
pub use http::{HttpStack, metrics_router, ops_router, serve, spawn_metrics_server};
pub use internal_auth::{INTERNAL_TOKEN_HEADER, InternalToken};
pub use metrics::MetricsRegistry;
pub use problem::{IntoProblem, Problem};
pub use ratelimit::{RateLimiter, RouteClass, RouteClassifier};
pub use reload::run as run_reloading;
pub use shutdown::install_shutdown;
// Re-exported so a service can name the token `run_reloading` hands its runtime without
// taking a direct `tokio-util` dependency for one type.
pub use telemetry::init_tracing;
pub use tokio_util::sync::CancellationToken;
pub use tunables::{TunableDefaultsOnly, TunableSet, TunableSource};

#[cfg(feature = "db")]
pub use audit::PostgresAuditSink;
#[cfg(feature = "db")]
pub use flags::PostgresFlagSource;
#[cfg(feature = "db")]
pub use tunables::PostgresTunableSource;

/// Failures that prevent a service from starting. Every variant is fatal by construction;
/// runtime failures of *optional* dependencies degrade elsewhere rather than abort here.
#[derive(Debug, thiserror::Error)]
pub enum ServiceError {
    /// The global `tracing` subscriber could not be installed (usually: called twice).
    #[error("failed to install tracing subscriber: {0}")]
    Tracing(String),
    /// The process-wide metrics recorder could not be installed.
    #[error("failed to install metrics recorder: {0}")]
    Metrics(String),
    /// The listener could not be bound, or the server exited with an I/O error.
    #[error("http server error: {0}")]
    Server(#[from] std::io::Error),
}
