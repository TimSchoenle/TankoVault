//! Owns the audit trail for privileged and privacy-relevant actions (`docs/ARCHITECTURE.md` §16).
//!
//! Recording is best-effort and non-blocking: a write failure is logged but never fails
//! the audited action, since refusing a privileged action over a logging outage would trade
//! an availability incident for a record-keeping one.

use async_trait::async_trait;
use serde_json::Value as Json;
use std::borrow::Cow;
use tankovault_domain::UserId;

/// How the audited action ended.
///
/// A denied action must still be recorded: refuse it after calling `record`, not before, or the
/// attempt leaves no trace.
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
    /// Returns the stored discriminant, which matches the `audit_log_outcome_check` constraint.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Failure => "failure",
            Self::Denied => "denied",
        }
    }
}

/// One auditable action. Built with [`AuditEvent::new`] and refined with the builder methods.
#[derive(Debug, Clone)]
pub struct AuditEvent {
    /// Who acted. `None` for system-originated actions (schedulers, sweeps).
    pub actor: Option<UserId>,
    /// Dotted action name, e.g. `provider.update`, `account.export`, `auth.login`.
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
    /// Creates a successful, system-originated action; refine it with the builder methods.
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

    /// Attributes the action to a user.
    #[must_use]
    pub fn actor(mut self, actor: UserId) -> Self {
        self.actor = Some(actor);
        self
    }

    /// Names the affected entity, cut to [`MAX_FIELD_LEN`] bytes.
    #[must_use]
    pub fn target(mut self, target: impl Into<String>) -> Self {
        self.target = Some(bounded(target.into()));
        self
    }

    /// Attaches structured context.
    #[must_use]
    pub fn detail(mut self, detail: Json) -> Self {
        self.detail = detail;
        self
    }

    /// Sets the outcome.
    #[must_use]
    pub fn outcome(mut self, outcome: AuditOutcome) -> Self {
        self.outcome = outcome;
        self
    }

    /// Marks the action as refused.
    #[must_use]
    pub fn denied(self) -> Self {
        self.outcome(AuditOutcome::Denied)
    }

    /// Marks the action as attempted but failed.
    #[must_use]
    pub fn failed(self) -> Self {
        self.outcome(AuditOutcome::Failure)
    }

    /// Attaches the request's client context. The sink applies the operator's privacy
    /// toggles, so callers may always pass what they have.
    #[must_use]
    pub fn client(mut self, ip: Option<String>, user_agent: Option<String>) -> Self {
        self.ip = ip.map(bounded);
        self.user_agent = user_agent.map(bounded);
        self
    }
}

/// Longest string an audit record keeps in a free-text column.
///
/// Unauthenticated callers choose `target` (a sign-in identifier) and the user agent, and every
/// failed attempt is a row; unbounded, each one could write up to the request body cap.
pub const MAX_FIELD_LEN: usize = 512;

/// Returns `value` cut to at most [`MAX_FIELD_LEN`] bytes, on a character boundary.
fn bounded(mut value: String) -> String {
    if value.len() > MAX_FIELD_LEN {
        let mut end = MAX_FIELD_LEN;
        while !value.is_char_boundary(end) {
            end -= 1;
        }
        value.truncate(end);
    }
    value
}

/// Destination for audit records.
#[async_trait]
pub trait AuditSink: Send + Sync + 'static {
    /// Records `event`.
    ///
    /// Implementations must not propagate failures: auditing never fails the audited action (see
    /// the module docs).
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
    /// where data is persisted, not trusted to every call site.
    pub struct PostgresAuditSink {
        pool: PgPool,
        record_ip: bool,
        record_user_agent: bool,
    }

    impl PostgresAuditSink {
        /// Creates a sink that persists events to `pool`, honouring `cfg`'s privacy toggles.
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

    /// Caller-chosen fields are cut to `MAX_FIELD_LEN` bytes on a character boundary.
    ///
    /// The bug: a failed sign-in stored the presented identifier whole, up to the 1 MiB body cap
    /// per unauthenticated request.
    #[test]
    fn caller_chosen_fields_are_bounded_on_a_character_boundary() {
        let event = AuditEvent::new("auth.login")
            .target("é".repeat(MAX_FIELD_LEN))
            .client(Some("x".repeat(10_000)), Some("y".repeat(10_000)));
        let target = event.target.expect("set");
        assert!(target.len() <= MAX_FIELD_LEN);
        assert!(target.chars().all(|c| c == 'é'));
        assert_eq!(event.user_agent.expect("set").len(), MAX_FIELD_LEN);
    }

    #[tokio::test]
    async fn noop_sink_accepts_everything() {
        // Swapping the sink must never change handler behaviour.
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
