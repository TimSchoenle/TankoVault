//! Append-only audit log for privileged actions (design §16).

use crate::error::DbResult;
use serde_json::Value as Json;
use sqlx::{FromRow, PgExecutor};
use tankovault_domain::UserId;
use time::OffsetDateTime;
use uuid::Uuid;

/// Append one privileged-action record.
///
/// `actor_id` is `None` for system-originated actions (schedulers, sweeps). `outcome` is
/// one of `success` / `failure` / `denied`, enforced by the `audit_log_outcome_check`
/// constraint. `client_ip` and `user_agent` are personal data and are passed as `None`
/// unless the operator enabled the corresponding privacy toggle — the decision is applied
/// in `tankovault_service::PostgresAuditSink`, not here.
#[allow(clippy::too_many_arguments)]
pub async fn record<'e, E: PgExecutor<'e>>(
    exec: E,
    actor_id: Option<UserId>,
    action: &str,
    target: Option<&str>,
    detail: &Json,
    outcome: &str,
    client_ip: Option<&str>,
    user_agent: Option<&str>,
) -> DbResult<()> {
    sqlx::query!(
        "INSERT INTO audit_log (id, actor_id, action, target, detail, outcome, actor_ip, user_agent) \
         VALUES ($1,$2,$3,$4,$5,$6,$7::text::inet,$8)",
        Uuid::now_v7(),
        actor_id.map(UserId::as_uuid),
        action,
        target,
        detail,
        outcome,
        client_ip,
        user_agent,
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// Delete records older than `retention_days`, returning how many were removed.
///
/// Storage limitation (GDPR Art. 5(1)(e)): an audit trail kept forever is a growing
/// liability, not a stronger control. Deletion is capped per call so a first sweep over a
/// long-neglected table cannot hold a lock long enough to stall the writers appending to
/// it.
pub async fn prune_older_than<'e, E: PgExecutor<'e>>(
    exec: E,
    retention_days: u32,
    max_rows: i64,
) -> DbResult<u64> {
    let cutoff = OffsetDateTime::now_utc() - time::Duration::days(i64::from(retention_days));
    let deleted = sqlx::query!(
        "DELETE FROM audit_log WHERE id IN ( \
           SELECT id FROM audit_log WHERE created_at < $1 ORDER BY created_at LIMIT $2 \
         )",
        cutoff,
        max_rows,
    )
    .execute(exec)
    .await?
    .rows_affected();
    Ok(deleted)
}

/// One privileged-action record enriched with the actor's username, for the console feed.
#[derive(Debug, Clone, serde::Serialize, FromRow, utoipa::ToSchema)]
pub struct AuditView {
    pub id: Uuid,
    /// Actor username (`None` for system-originated actions or a since-deleted user).
    pub actor: Option<String>,
    pub action: String,
    pub target: Option<String>,
    pub detail: Json,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String)]
    pub created_at: OffsetDateTime,
}

/// The most recent privileged actions, newest first (design §16 audit trail surfaced in the
/// operator console).
pub async fn list_recent<'e, E: PgExecutor<'e>>(exec: E, limit: i64) -> DbResult<Vec<AuditView>> {
    let rows: Vec<AuditView> = sqlx::query_as!(
        AuditView,
        "SELECT a.id, u.username AS \"actor?\", a.action, a.target, a.detail, a.created_at \
         FROM audit_log a \
         LEFT JOIN users u ON u.id = a.actor_id \
         ORDER BY a.created_at DESC \
         LIMIT $1",
        limit,
    )
    .fetch_all(exec)
    .await?;
    Ok(rows)
}
