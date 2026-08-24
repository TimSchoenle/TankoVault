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
    /// The audit row.
    pub id: Uuid,
    /// Actor username (`None` for system-originated actions or a since-deleted user).
    pub actor: Option<String>,
    /// What was done, as a stable action key.
    pub action: String,
    /// What it was done to, `None` for an action with no single subject.
    pub target: Option<String>,
    /// Action-specific detail. The action owns that shape, not this type.
    pub detail: Json,
    /// When the action was recorded.
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

/// What narrows a page of the audit trail.
///
/// Every field is a SQL predicate, never a client-side filter: the point of paging the trail is
/// that the console no longer holds it, so it cannot filter what it does not have.
#[derive(Debug, Clone, Default)]
pub struct AuditFilter<'a> {
    /// Records attributed to one actor. `None` matches every actor *and* the system.
    pub actor_id: Option<UserId>,
    /// Exact action key, e.g. `admin.user.update`.
    pub action: Option<&'a str>,
    /// Substring of the target, since a target is an opaque identifier the operator pastes.
    pub target: Option<&'a str>,
    /// Inclusive lower bound on the record's timestamp.
    pub since: Option<OffsetDateTime>,
    /// Exclusive upper bound on it.
    pub until: Option<OffsetDateTime>,
}

/// One page of the audit trail, plus how many records the filter matches in total.
#[derive(Debug, Clone)]
pub struct AuditPage {
    /// The page, newest record first.
    pub items: Vec<AuditView>,
    /// Records the filter matched, ignoring `limit` and `offset`.
    pub total: i64,
}

/// A filtered, paged window on the audit trail, newest first.
///
/// Every predicate is `$n IS NULL OR …`, so one prepared statement serves every combination of
/// filters — and each one is a *SQL* predicate, which is the whole point: with the trail paged,
/// the console no longer holds it and cannot filter what it does not have.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only. A filter matching nothing is an empty page with `total: 0`,
/// not [`crate::DbError::NotFound`]; as in [`crate::repo::user_admin::directory`], `total` is
/// read off the first row, so it is only meaningful because every row carries the same value —
/// and an offset past the end reports `0`.
pub async fn list_filtered<'e, E: PgExecutor<'e>>(
    exec: E,
    filter: &AuditFilter<'_>,
    limit: i64,
    offset: i64,
) -> DbResult<AuditPage> {
    #[derive(FromRow)]
    struct Row {
        id: Uuid,
        actor: Option<String>,
        action: String,
        target: Option<String>,
        detail: Json,
        created_at: OffsetDateTime,
        total: i64,
    }
    // `target` matches as a substring because it is an opaque identifier an operator pastes a
    // fragment of; `action` matches exactly, because it comes from a closed vocabulary the
    // filter offers as a list.
    let rows: Vec<Row> = sqlx::query_as!(
        Row,
        "WITH matched AS ( \
             SELECT a.* FROM audit_log a \
             WHERE ($1::uuid IS NULL OR a.actor_id = $1) \
               AND ($2::text IS NULL OR a.action = $2) \
               AND ($3::text IS NULL OR a.target ILIKE '%' || $3 || '%') \
               AND ($4::timestamptz IS NULL OR a.created_at >= $4) \
               AND ($5::timestamptz IS NULL OR a.created_at < $5) \
         ) \
         SELECT m.id, u.username AS \"actor?\", m.action, m.target, m.detail, m.created_at, \
                (SELECT count(*) FROM matched) AS \"total!\" \
         FROM matched m \
         LEFT JOIN users u ON u.id = m.actor_id \
         ORDER BY m.created_at DESC \
         LIMIT $6 OFFSET $7",
        filter.actor_id.map(UserId::as_uuid),
        filter.action,
        filter.target,
        filter.since,
        filter.until,
        limit,
        offset,
    )
    .fetch_all(exec)
    .await?;

    let total = rows.first().map_or(0, |row| row.total);
    let items = rows
        .into_iter()
        .map(|row| AuditView {
            id: row.id,
            actor: row.actor,
            action: row.action,
            target: row.target,
            detail: row.detail,
            created_at: row.created_at,
        })
        .collect();
    Ok(AuditPage { items, total })
}

/// Every distinct action key present in the trail, for the filter's picker.
///
/// Read from the data rather than from a hand-written list: the vocabulary is whatever handlers
/// have recorded, and a list here would go stale the first time one is added.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only.
pub async fn distinct_actions<'e, E: PgExecutor<'e>>(exec: E) -> DbResult<Vec<String>> {
    let rows = sqlx::query_scalar!("SELECT DISTINCT action FROM audit_log ORDER BY action")
        .fetch_all(exec)
        .await?;
    Ok(rows)
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
