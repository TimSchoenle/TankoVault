//! # tankovault-service
//!
//! The production runtime every `TankoVault` service shares: process bootstrap, graceful
//! shutdown, health probes, the HTTP middleware stack, inbound rate limiting, the audit
//! trail, and metrics.
//!
//! ## Why this crate exists
//!
//! Each service used to open-code the same boot sequence (load config → init telemetry →
//! connect the pool → bind → `axum::serve`) and each drifted: some installed a metrics
//! recorder and some did not, none had graceful shutdown, and every `/ready` probe was a
//! literal `"ok"` that reported healthy while its database was unreachable. Cross-cutting
//! concerns belong in one place where they can be reviewed once and are correct everywhere.
//!
//! ## Toggles
//!
//! Auditing, metrics and rate limiting are each switchable from configuration, and each
//! switch is a *wiring* decision rather than a branch at the call site:
//!
//! - **Metrics off** ([`tankovault_config::MetricsConfig::enabled`]) means the Prometheus
//!   recorder is never installed, so no measurement is retained and the scrape route
//!   answers `404`. Domain code still calls `metrics::counter!` unchanged.
//! - **Audit off** ([`tankovault_config::AuditConfig::enabled`]) installs [`NoopAuditSink`]
//!   behind the same [`AuditSink`] trait object, so handlers never test a flag.
//! - **Rate limiting off** ([`tankovault_config::RateLimitConfig::enabled`]) leaves the
//!   layer unmounted entirely, costing nothing per request.
//!
//! ## Composition
//!
//! ```no_run
//! # use std::sync::Arc;
//! # use tankovault_service::{Health, HttpStack, MetricsRegistry, ServiceError};
//! # async fn example() -> Result<(), ServiceError> {
//! # let router = axum::Router::new();
//! # let security = Default::default();
//! let metrics = MetricsRegistry::install(&Default::default())?;
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
pub mod health;
pub mod http;
pub mod metrics;
pub mod ratelimit;
pub mod shutdown;
pub mod telemetry;

pub use audit::{AuditEvent, AuditOutcome, AuditSink, NoopAuditSink};
pub use health::{Health, HealthBuilder, HealthReport, HealthStatus};
pub use http::{HttpStack, ops_router, serve};
pub use metrics::MetricsRegistry;
pub use ratelimit::{RateLimiter, RouteClass, RouteClassifier};
pub use shutdown::install_shutdown;
pub use telemetry::init_tracing;

#[cfg(feature = "db")]
pub use audit::PostgresAuditSink;

/// Failures that prevent a service from starting.
///
/// Every variant is fatal by construction: a service that cannot install its telemetry or
/// bind its listener has no degraded mode worth running in. Runtime failures of *optional*
/// dependencies (an unreachable Redis, a down NATS) are deliberately not modelled here —
/// those are handled by the component that owns them and degrade rather than abort.
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
