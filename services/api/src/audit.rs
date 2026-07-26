//! Call-site helpers for the audit trail.
//!
//! The sink itself lives in `tankovault-service`; this module is the thin, ergonomic
//! surface handlers use so that recording an action is a single line and impossible to get
//! wrong. Whether anything is actually written is decided once at boot by which sink was
//! installed — see [`tankovault_service::audit`].

use crate::state::{AppState, AuthUser, ClientContext};
use serde_json::Value;
use tankovault_domain::UserId;
use tankovault_service::{AuditEvent, AuditOutcome};

/// Record a successful privileged action.
///
/// Best-effort: the sink swallows its own failures, so an audit problem never fails the
/// action the user asked for.
pub(crate) async fn audit(
    state: &AppState,
    user: &AuthUser,
    action: &'static str,
    target: &str,
    detail: &Value,
) {
    record(state, user, action, target, detail, AuditOutcome::Success).await;
}

/// Record a privileged action that was attempted and failed.
///
/// Distinct from a denial: the caller was entitled to act, and the action did not take
/// effect. Both matter after an incident, and collapsing them loses the distinction
/// between "was refused" and "tried and broke".
pub(crate) async fn audit_failure(
    state: &AppState,
    user: &AuthUser,
    action: &'static str,
    target: &str,
    detail: &Value,
) {
    record(state, user, action, target, detail, AuditOutcome::Failure).await;
}

/// Record an action on an unauthenticated route.
///
/// The credential endpoints have no [`AuthUser`] to carry the actor or the request origin,
/// yet they are exactly where an audit trail earns its keep: a burst of failed logins, or
/// a refresh-token reuse, is the clearest signal of compromise this system can emit.
///
/// `actor` is the account the attempt *targeted* where that is known (a real user typed
/// the wrong password) and `None` where it is not (the identifier matched nobody).
pub(crate) async fn audit_anonymous(
    state: &AppState,
    client: &ClientContext,
    actor: Option<UserId>,
    action: &'static str,
    target: &str,
    detail: &Value,
    outcome: AuditOutcome,
) {
    let mut event = AuditEvent::new(action)
        .target(target)
        .detail(detail.clone())
        .outcome(outcome)
        .client(client.ip.clone(), client.user_agent.clone());
    if let Some(actor) = actor {
        event = event.actor(actor);
    }
    state.audit.record(event).await;
}

async fn record(
    state: &AppState,
    user: &AuthUser,
    action: &'static str,
    target: &str,
    detail: &Value,
    outcome: AuditOutcome,
) {
    state
        .audit
        .record(
            user.event(action)
                .target(target)
                .detail(detail.clone())
                .outcome(outcome),
        )
        .await;
}
