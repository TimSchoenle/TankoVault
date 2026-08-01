//! Append-only audit log for privileged actions (design §16).

use crate::error::DbResult;
use serde_json::Value as Json;
use sqlx::{FromRow, PgExecutor};
use tankovault_domain::UserId;
use time::OffsetDateTime;
use uuid::Uuid;

/// One privileged action, as the audit sink hands it to this repository.
///
/// A struct, not positional params: the string-shaped fields are easiest to transpose, and the
/// pair that would is the one deciding what personal data is retained.
pub struct AuditRecord<'a> {
    /// `None` for system-originated actions (schedulers, sweeps).
    pub actor_id: Option<UserId>,
    /// The action key, e.g. `admin.user.update`.
    pub action: &'a str,
    /// What the action was performed on, when it names a single subject.
    pub target: Option<&'a str>,
    /// Action-specific detail; whatever the handler recorded.
    pub detail: &'a Json,
    /// One of `success`/`failure`/`denied`, enforced by a DB check constraint, not this type.
    pub outcome: &'a str,
    /// Personal data; `None` unless the operator's privacy toggle enabled it.
    pub client_ip: Option<&'a str>,
    /// Personal data, same terms as [`AuditRecord::client_ip`].
    pub user_agent: Option<&'a str>,
}

/// Append one privileged-action record.
///
/// # Errors
/// `Sqlx` only; a bad `outcome` fails the check constraint (500), not
/// [`crate::DbError::Conflict`].
pub async fn record<'e, E: PgExecutor<'e>>(exec: E, entry: &AuditRecord<'_>) -> DbResult<()> {
    sqlx::query!(
        "INSERT INTO audit_log (id, actor_id, action, target, detail, outcome, actor_ip, user_agent) \
         VALUES ($1,$2,$3,$4,$5,$6,$7::text::inet,$8)",
        Uuid::now_v7(),
        entry.actor_id.map(UserId::as_uuid),
        entry.action,
        entry.target,
        entry.detail,
        entry.outcome,
        entry.client_ip,
        entry.user_agent,
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// Delete records older than `retention_days`, returning how many were removed.
///
/// GDPR Art. 5(1)(e) storage limitation. Capped per call so a first sweep over a
/// long-neglected table can't hold a lock long enough to stall concurrent writers.
///
/// # Errors
/// `Sqlx` only; deleting nothing returns `Ok(0)`.
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

/// The most recent privileged actions, newest first, for the operator console.
///
/// # Errors
/// `Sqlx` only; an empty log is `Ok(vec![])`, not [`crate::DbError::NotFound`].
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
