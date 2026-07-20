//! Append-only audit log for privileged actions (design §16).

use crate::error::DbResult;
use tankovault_domain::UserId;
use serde_json::Value as Json;
use sqlx::{FromRow, PgExecutor};
use time::OffsetDateTime;
use uuid::Uuid;

/// Record a privileged action. `actor_id` is `None` for system-originated actions.
pub async fn record<'e, E: PgExecutor<'e>>(
    exec: E,
    actor_id: Option<UserId>,
    action: &str,
    target: Option<&str>,
    detail: &Json,
) -> DbResult<()> {
    sqlx::query(
        "INSERT INTO audit_log (id, actor_id, action, target, detail) VALUES ($1,$2,$3,$4,$5)",
    )
    .bind(Uuid::now_v7())
    .bind(actor_id.map(UserId::as_uuid))
    .bind(action)
    .bind(target)
    .bind(detail)
    .execute(exec)
    .await?;
    Ok(())
}

/// One privileged-action record enriched with the actor's username, for the console feed.
#[derive(Debug, Clone, serde::Serialize, FromRow)]
pub struct AuditView {
    pub id: Uuid,
    /// Actor username (`None` for system-originated actions or a since-deleted user).
    pub actor: Option<String>,
    pub action: String,
    pub target: Option<String>,
    pub detail: Json,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

/// The most recent privileged actions, newest first (design §16 audit trail surfaced in the
/// operator console).
pub async fn list_recent<'e, E: PgExecutor<'e>>(exec: E, limit: i64) -> DbResult<Vec<AuditView>> {
    let rows: Vec<AuditView> = sqlx::query_as(
        "SELECT a.id, u.username AS actor, a.action, a.target, a.detail, a.created_at \
         FROM audit_log a \
         LEFT JOIN users u ON u.id = a.actor_id \
         ORDER BY a.created_at DESC \
         LIMIT $1",
    )
    .bind(limit)
    .fetch_all(exec)
    .await?;
    Ok(rows)
}
