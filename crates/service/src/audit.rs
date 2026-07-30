//! The audit trail for privileged and privacy-relevant actions (design §16).
//!
//! Auditing is expressed as a trait object rather than a direct repository call so that
//! *whether* auditing happens is decided once, at wiring time, and never at a call site.
//! Turning it off swaps in [`NoopAuditSink`]; handlers are identical either way.
//!
//! Recording is **best-effort and non-blocking on the caller's critical path**: a failure
//! to write the trail is logged at `error` but never fails the audited action. The
//! alternative — refusing a legitimate privileged action because a logging table is
//! unavailable — trades an availability incident for a record-keeping one.

use async_trait::async_trait;
use serde_json::Value as Json;
use std::borrow::Cow;
use tankovault_domain::UserId;

/// How the audited action ended.
///
/// A denied action is the most interesting record an audit trail holds, and the previous
/// implementation could not express it at all: handlers returned `403` before reaching the
/// recording call, so failed privilege escalation left no trace.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuditOutcome {
    /// The action was permitted and completed.
    Success,
    /// The action was permitted but failed while executing.
    Failure,
    /// The action was refused (authorization, validation, rate limit).
    Denied,
}

impl AuditOutcome {
    /// The stored discriminant. Matches the `audit_log_outcome_check` constraint.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Failure => "failure",
            Self::Denied => "denied",
        }
    }
}

/// One auditable action.
///
/// Built with [`AuditEvent::new`] and refined with the builder methods, so adding a field
/// later does not break every construction site.
#[derive(Debug, Clone)]
pub struct AuditEvent {
    /// Who acted. `None` for system-originated actions (schedulers, sweeps).
    pub actor: Option<UserId>,
    /// Dotted action name, e.g. `provider.update`, `account.export`, `auth.login`.
    ///
    /// `Cow` because the overwhelming majority are compile-time literals and should not
    /// allocate; the few dynamic ones still fit.
    pub action: Cow<'static, str>,
    /// The affected entity, as an id or short description.
    pub target: Option<String>,
    /// Structured, action-specific context. Must never contain credentials or tokens.
    pub detail: Json,
    /// How the action ended.
    pub outcome: AuditOutcome,
    /// Client IP. Populated only when the operator enabled `audit.record_ip`; it is
    /// personal data under GDPR Art. 4(1).
    pub ip: Option<String>,
    /// Client `User-Agent`, likewise gated behind `audit.record_user_agent`.
    pub user_agent: Option<String>,
}

impl AuditEvent {
    /// A successful, system-originated action. Refine with the builder methods.
    #[must_use]
    pub fn new(action: impl Into<Cow<'static, str>>) -> Self {
        Self {
            actor: None,
            action: action.into(),
            target: None,
            detail: Json::Object(serde_json::Map::new()),
            outcome: AuditOutcome::Success,
            ip: None,
            user_agent: None,
        }
    }

    /// Attribute the action to a user.
    #[must_use]
    pub fn actor(mut self, actor: UserId) -> Self {
        self.actor = Some(actor);
        self
    }

    /// Name the affected entity.
    #[must_use]
    pub fn target(mut self, target: impl Into<String>) -> Self {
        self.target = Some(target.into());
        self
    }

    /// Attach structured context.
    #[must_use]
    pub fn detail(mut self, detail: Json) -> Self {
        self.detail = detail;
        self
    }

    /// Mark the outcome.
    #[must_use]
    pub fn outcome(mut self, outcome: AuditOutcome) -> Self {
        self.outcome = outcome;
        self
    }

    /// Mark the action as refused.
    #[must_use]
    pub fn denied(self) -> Self {
        self.outcome(AuditOutcome::Denied)
    }

    /// Mark the action as attempted but failed.
    #[must_use]
    pub fn failed(self) -> Self {
        self.outcome(AuditOutcome::Failure)
    }

    /// Attach the request's client context. The sink applies the operator's privacy
    /// toggles, so callers may always pass what they have.
    #[must_use]
    pub fn client(mut self, ip: Option<String>, user_agent: Option<String>) -> Self {
        self.ip = ip;
        self.user_agent = user_agent;
        self
    }
}

/// Destination for audit records.
#[async_trait]
pub trait AuditSink: Send + Sync + 'static {
    /// Record `event`. Implementations must not propagate failures — auditing never fails
    /// the audited action (see the module docs).
    async fn record(&self, event: AuditEvent);
}

/// Discards every event. Installed when `audit.enabled` is `false`.
///
/// Deliberately silent: logging each dropped event would recreate the audit trail in the
/// log stream, defeating the operator's decision to switch auditing off.
pub struct NoopAuditSink;

#[async_trait]
impl AuditSink for NoopAuditSink {
    async fn record(&self, _event: AuditEvent) {}
}

#[cfg(feature = "db")]
pub use postgres::PostgresAuditSink;

#[cfg(feature = "db")]
mod postgres {
    use super::{AuditEvent, AuditSink};
    use async_trait::async_trait;
    use tankovault_config::AuditConfig;
    use tankovault_db::PgPool;

    /// Appends events to the `audit_log` table.
    ///
    /// Holds the [`AuditConfig`] so the privacy toggles are enforced at the single point
    /// where data is persisted, rather than trusted to every call site.
    pub struct PostgresAuditSink {
        pool: PgPool,
        record_ip: bool,
        record_user_agent: bool,
    }

    impl PostgresAuditSink {
        /// Persist events to `pool`, honouring `cfg`'s privacy toggles.
        #[must_use]
        pub fn new(pool: PgPool, cfg: &AuditConfig) -> Self {
            Self {
                pool,
                record_ip: cfg.record_ip,
                record_user_agent: cfg.record_user_agent,
            }
        }
    }

    #[async_trait]
    impl AuditSink for PostgresAuditSink {
        async fn record(&self, event: AuditEvent) {
            // Strip the fields the operator chose not to retain *here*, so no handler can
            // accidentally persist an IP by constructing the event differently.
            let ip = if self.record_ip {
                event.ip.as_deref()
            } else {
                None
            };
            let user_agent = if self.record_user_agent {
                event.user_agent.as_deref()
            } else {
                None
            };

            let result = tankovault_db::repo::audit::record(
                &self.pool,
                &tankovault_db::repo::audit::AuditRecord {
                    actor_id: event.actor,
                    action: &event.action,
                    target: event.target.as_deref(),
                    detail: &event.detail,
                    outcome: event.outcome.as_str(),
                    client_ip: ip,
                    user_agent,
                },
            )
            .await;

            if let Err(e) = result {
                // `error`, not `warn`: a lost audit record is a compliance gap, and the
                // action it describes has already taken effect.
                tracing::error!(
                    error = %e,
                    action = %event.action,
                    "failed to write audit record; the action itself succeeded"
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn noop_sink_accepts_everything() {
        // The contract that makes the toggle safe: swapping the sink never changes handler
        // behaviour, so `record` cannot fail or panic regardless of the event.
        let sink = NoopAuditSink;
        sink.record(AuditEvent::new("provider.delete").denied())
            .await;
    }

    #[test]
    fn builder_defaults_to_a_successful_system_action() {
        let event = AuditEvent::new("scan.trigger");
        assert!(event.actor.is_none());
        assert_eq!(event.outcome, AuditOutcome::Success);
        assert_eq!(event.detail, serde_json::json!({}));
    }

    #[test]
    fn outcome_discriminants_match_the_check_constraint() {
        // These strings are enforced by `audit_log_outcome_check` in migration 0017; a
        // mismatch would only surface as a runtime insert failure.
        assert_eq!(AuditOutcome::Success.as_str(), "success");
        assert_eq!(AuditOutcome::Failure.as_str(), "failure");
        assert_eq!(AuditOutcome::Denied.as_str(), "denied");
    }
}
