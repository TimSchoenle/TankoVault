//! The data-subject request queue (GDPR Chapter III).
//!
//! For what the self-service export and erase endpoints cannot handle: rectification,
//! restriction, objection, and the Art. 12(3) deadline and compliance record those require.
//!
//! No copy of the subject's email/username is stored; `user_id` is `ON DELETE SET NULL`, so a
//! completed erasure's row degrades into an accountability record holding no personal data.

use crate::error::{DbError, DbResult};
use sqlx::{FromRow, PgExecutor};
use tankovault_domain::UserId;
use time::OffsetDateTime;
use uuid::Uuid;

/// What the subject is exercising. Mirrors the `gdpr_request_kind` SQL enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, sqlx::Type)]
#[sqlx(type_name = "gdpr_request_kind", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum RequestKind {
    /// Art. 15: what is held about the subject.
    Access,
    /// Art. 20: the subject's data in a machine-readable form.
    Portability,
    /// Art. 16: correct it.
    Rectification,
    /// Art. 17: delete it.
    Erasure,
    /// Art. 18: stop processing it, but keep it.
    Restriction,
    /// Art. 21: stop processing it on legitimate-interest grounds.
    Objection,
}

impl RequestKind {
    /// Returns whether fulfilling this request runs the erasure cascade, which decides the
    /// operator action a queue entry offers.
    #[must_use]
    pub fn is_erasure(self) -> bool {
        matches!(self, Self::Erasure)
    }

    /// Returns whether fulfilling this request means disclosing the subject's data export.
    #[must_use]
    pub fn needs_export(self) -> bool {
        matches!(self, Self::Access | Self::Portability)
    }
}

/// Where a request is in its lifecycle. Mirrors the `gdpr_request_status` SQL enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, sqlx::Type)]
#[sqlx(type_name = "gdpr_request_status", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum RequestStatus {
    /// Filed, and nobody has claimed it.
    Pending,
    /// An operator has claimed it and is working it.
    InProgress,
    /// Fulfilled.
    Completed,
    /// Refused, with the Art. 12(4) reasons in `resolution_note`.
    Rejected,
    /// Withdrawn by the subject before it was resolved.
    Cancelled,
}

impl RequestStatus {
    /// Returns whether the request is still open, which is when the Art. 12(3) clock runs.
    #[must_use]
    pub fn is_open(self) -> bool {
        matches!(self, Self::Pending | Self::InProgress)
    }
}

/// A request as the subject sees it, and the shape the operator queue extends.
#[derive(Debug, Clone, serde::Serialize)]
pub struct RequestRow {
    /// The request, which is what a resolve or a withdrawal names.
    pub id: Uuid,
    /// Which right is being exercised.
    pub kind: RequestKind,
    /// Where it is in its lifecycle.
    pub status: RequestStatus,
    /// What the subject wrote when filing, `None` when they wrote nothing.
    pub detail: Option<String>,
    /// When it was filed, which is what the Art. 12(3) deadline runs from.
    #[serde(with = "time::serde::rfc3339")]
    pub requested_at: OffsetDateTime,
    /// The Art. 12(3) deadline. Past it with the request open is what `overdue` reports.
    #[serde(with = "time::serde::rfc3339")]
    pub due_at: OffsetDateTime,
    /// When it settled, `None` while it is still open.
    #[serde(with = "time::serde::rfc3339::option")]
    pub resolved_at: Option<OffsetDateTime>,
    /// How it was resolved, or for a rejection why, since Art. 12(4) requires reasons.
    pub resolution_note: Option<String>,
}

/// A queue entry as the operator sees it: the subject (while they exist) plus who's handling it.
///
/// Nested `request`, not `#[serde(flatten)]`, matching
/// `tankovault_contracts::admin::AdminPrivacyRequestView`.
#[derive(Debug, Clone, serde::Serialize)]
pub struct AdminRequestRow {
    /// The subject-facing record, exactly as the subject sees it.
    pub request: RequestRow,
    /// The subject's id, or `None` once erased.
    pub user_id: Option<Uuid>,
    /// `None` once the account is gone, which is expected after a completed erasure.
    pub username: Option<String>,
    /// The subject's address, `None` once they have been erased.
    pub email: Option<String>,
    /// Operator who claimed it, if any.
    pub claimed_by: Option<String>,
    /// Operator who resolved it, if resolved.
    pub resolved_by: Option<String>,
    /// Past the Art. 12(3) deadline while still open; computed in SQL against `now()`.
    pub overdue: bool,
}

/// Files a request on behalf of `user_id`, returning the row so the subject sees the due date
/// at once (Art. 12(3)).
///
/// [`has_open_of_kind`] is the duplicate guard; this does not refuse a second request.
///
/// # Errors
/// `Sqlx` only; a second request of the same kind is not [`DbError::Conflict`].
pub async fn create<'e, E: PgExecutor<'e>>(
    exec: E,
    user_id: UserId,
    kind: RequestKind,
    detail: Option<&str>,
) -> DbResult<RequestRow> {
    let row = sqlx::query_as!(
        RawRequest,
        "INSERT INTO gdpr_requests (id, user_id, kind, detail) VALUES ($1,$2,$3,$4) \
         RETURNING id, kind AS \"kind: RequestKind\", status AS \"status: RequestStatus\", \
                   detail, requested_at, due_at, resolved_at, resolution_note",
        Uuid::now_v7(),
        user_id.as_uuid(),
        kind as RequestKind,
        detail,
    )
    .fetch_one(exec)
    .await?;
    Ok(row.into())
}

/// Row shape shared by every query that returns the subject-facing fields.
#[derive(FromRow)]
struct RawRequest {
    id: Uuid,
    kind: RequestKind,
    status: RequestStatus,
    detail: Option<String>,
    requested_at: OffsetDateTime,
    due_at: OffsetDateTime,
    resolved_at: Option<OffsetDateTime>,
    resolution_note: Option<String>,
}

impl From<RawRequest> for RequestRow {
    fn from(r: RawRequest) -> Self {
        Self {
            id: r.id,
            kind: r.kind,
            status: r.status,
            detail: r.detail,
            requested_at: r.requested_at,
            due_at: r.due_at,
            resolved_at: r.resolved_at,
            resolution_note: r.resolution_note,
        }
    }
}

/// Returns a subject's own requests, newest first.
///
/// # Errors
/// `Sqlx` only; nothing filed is `Ok(vec![])`.
pub async fn list_for_user<'e, E: PgExecutor<'e>>(
    exec: E,
    user_id: UserId,
) -> DbResult<Vec<RequestRow>> {
    let rows = sqlx::query_as!(
        RawRequest,
        "SELECT id, kind AS \"kind: RequestKind\", status AS \"status: RequestStatus\", \
                detail, requested_at, due_at, resolved_at, resolution_note \
         FROM gdpr_requests WHERE user_id = $1 ORDER BY requested_at DESC",
        user_id.as_uuid(),
    )
    .fetch_all(exec)
    .await?;
    Ok(rows.into_iter().map(Into::into).collect())
}

/// Returns whether the subject already has an unresolved request of this kind.
///
/// The duplicate guard: a second filing would start its own Art. 12(3) clock. A caller must
/// propagate an error, since defaulting to `false` admits the duplicate.
///
/// # Errors
/// `Sqlx` only; `EXISTS` always returns a row, so "none open" is `Ok(false)`.
pub async fn has_open_of_kind<'e, E: PgExecutor<'e>>(
    exec: E,
    user_id: UserId,
    kind: RequestKind,
) -> DbResult<bool> {
    let exists = sqlx::query_scalar!(
        "SELECT EXISTS(SELECT 1 FROM gdpr_requests \
         WHERE user_id = $1 AND kind = $2 AND status IN ('pending','in_progress')) \
         AS \"exists!\"",
        user_id.as_uuid(),
        kind as RequestKind,
    )
    .fetch_one(exec)
    .await?;
    Ok(exists)
}

/// Returns the operator queue. `only_open` restricts it to what still needs work; the full list is
/// the compliance record.
///
/// # Errors
/// `Sqlx` only; an empty queue is `Ok(vec![])`.
pub async fn list_admin<'e, E: PgExecutor<'e>>(
    exec: E,
    only_open: bool,
    limit: i64,
) -> DbResult<Vec<AdminRequestRow>> {
    #[derive(FromRow)]
    struct Row {
        id: Uuid,
        kind: RequestKind,
        status: RequestStatus,
        detail: Option<String>,
        requested_at: OffsetDateTime,
        due_at: OffsetDateTime,
        resolved_at: Option<OffsetDateTime>,
        resolution_note: Option<String>,
        user_id: Option<Uuid>,
        username: Option<String>,
        email: Option<String>,
        claimed_by: Option<String>,
        resolved_by: Option<String>,
        overdue: bool,
    }
    // Open requests sort by deadline; resolved ones by recency.
    let rows = sqlx::query_as!(
        Row,
        // The `!` overrides name `gdpr_requests`' own `NOT NULL` columns. sqlx derives nullability
        // from the plan, and with outer joins a populated database can make every column
        // nullable, which does not compile. Pinning what the schema guarantees makes the type
        // the same whichever plan the planner picks.
        "SELECT r.id AS \"id!\", r.kind AS \"kind!: RequestKind\", \
                r.status AS \"status!: RequestStatus\", \
                r.detail, r.requested_at AS \"requested_at!\", r.due_at AS \"due_at!\", \
                r.resolved_at, r.resolution_note, \
                r.user_id, s.username AS \"username?: String\", s.email AS \"email?: String\", \
                c.username AS \"claimed_by?: String\", v.username AS \"resolved_by?: String\", \
                (r.status IN ('pending','in_progress') AND r.due_at < now()) AS \"overdue!\" \
         FROM gdpr_requests r \
         LEFT JOIN users s ON s.id = r.user_id \
         LEFT JOIN users c ON c.id = r.claimed_by \
         LEFT JOIN users v ON v.id = r.resolved_by \
         WHERE NOT $1 OR r.status IN ('pending','in_progress') \
         ORDER BY r.status IN ('pending','in_progress') DESC, \
                  CASE WHEN r.status IN ('pending','in_progress') THEN r.due_at END ASC, \
                  r.requested_at DESC \
         LIMIT $2",
        only_open,
        limit,
    )
    .fetch_all(exec)
    .await?;

    Ok(rows
        .into_iter()
        .map(|r| AdminRequestRow {
            request: RequestRow {
                id: r.id,
                kind: r.kind,
                status: r.status,
                detail: r.detail,
                requested_at: r.requested_at,
                due_at: r.due_at,
                resolved_at: r.resolved_at,
                resolution_note: r.resolution_note,
            },
            user_id: r.user_id,
            username: r.username,
            email: r.email,
            claimed_by: r.claimed_by,
            resolved_by: r.resolved_by,
            overdue: r.overdue,
        })
        .collect())
}

/// Returns one request by id, with its subject, so a caller can check an action is legal for
/// its kind and status.
///
/// # Errors
/// [`DbError::NotFound`] (404) when no request has that id; otherwise `Sqlx`.
pub async fn get_admin<'e, E: PgExecutor<'e>>(exec: E, id: Uuid) -> DbResult<AdminRequestRow> {
    let rows = list_admin_by_id(exec, id).await?;
    rows.into_iter().next().ok_or(DbError::NotFound)
}

async fn list_admin_by_id<'e, E: PgExecutor<'e>>(
    exec: E,
    id: Uuid,
) -> DbResult<Vec<AdminRequestRow>> {
    #[derive(FromRow)]
    struct Row {
        id: Uuid,
        kind: RequestKind,
        status: RequestStatus,
        detail: Option<String>,
        requested_at: OffsetDateTime,
        due_at: OffsetDateTime,
        resolved_at: Option<OffsetDateTime>,
        resolution_note: Option<String>,
        user_id: Option<Uuid>,
        username: Option<String>,
        email: Option<String>,
        claimed_by: Option<String>,
        resolved_by: Option<String>,
        overdue: bool,
    }
    let rows = sqlx::query_as!(
        Row,
        // The `!` overrides name `gdpr_requests`' own `NOT NULL` columns. sqlx derives nullability
        // from the plan, and with outer joins a populated database can make every column
        // nullable, which does not compile. Pinning what the schema guarantees makes the type
        // the same whichever plan the planner picks.
        "SELECT r.id AS \"id!\", r.kind AS \"kind!: RequestKind\", \
                r.status AS \"status!: RequestStatus\", \
                r.detail, r.requested_at AS \"requested_at!\", r.due_at AS \"due_at!\", \
                r.resolved_at, r.resolution_note, \
                r.user_id, s.username AS \"username?: String\", s.email AS \"email?: String\", \
                c.username AS \"claimed_by?: String\", v.username AS \"resolved_by?: String\", \
                (r.status IN ('pending','in_progress') AND r.due_at < now()) AS \"overdue!\" \
         FROM gdpr_requests r \
         LEFT JOIN users s ON s.id = r.user_id \
         LEFT JOIN users c ON c.id = r.claimed_by \
         LEFT JOIN users v ON v.id = r.resolved_by \
         WHERE r.id = $1",
        id,
    )
    .fetch_all(exec)
    .await?;

    Ok(rows
        .into_iter()
        .map(|r| AdminRequestRow {
            request: RequestRow {
                id: r.id,
                kind: r.kind,
                status: r.status,
                detail: r.detail,
                requested_at: r.requested_at,
                due_at: r.due_at,
                resolved_at: r.resolved_at,
                resolution_note: r.resolution_note,
            },
            user_id: r.user_id,
            username: r.username,
            email: r.email,
            claimed_by: r.claimed_by,
            resolved_by: r.resolved_by,
            overdue: r.overdue,
        })
        .collect())
}

/// Takes ownership of a pending request. The `status = 'pending'` guard stops two operators
/// claiming the same request at once.
///
/// Losing the race, an unknown id and an already-claimed request all return `false`; the
/// caller decides between 404 and 409.
///
/// # Errors
/// `Sqlx` only; losing the race, an unknown id, and an already-claimed request are all `Ok(false)`,
/// not [`DbError::Conflict`].
pub async fn claim<'e, E: PgExecutor<'e>>(exec: E, id: Uuid, operator: UserId) -> DbResult<bool> {
    let result = sqlx::query!(
        "UPDATE gdpr_requests SET status = 'in_progress', claimed_by = $2, claimed_at = now() \
         WHERE id = $1 AND status = 'pending'",
        id,
        operator.as_uuid(),
    )
    .execute(exec)
    .await?;
    Ok(result.rows_affected() > 0)
}

/// Resolves a request as completed, rejected or cancelled.
///
/// Only an open request moves, so a resolution cannot be recorded twice. `false` means this
/// call did **not** record the resolution the caller believes it made.
///
/// # Errors
/// `Sqlx` only; an already-resolved request is `Ok(false)`.
pub async fn resolve<'e, E: PgExecutor<'e>>(
    exec: E,
    id: Uuid,
    status: RequestStatus,
    operator: Option<UserId>,
    note: Option<&str>,
) -> DbResult<bool> {
    let result = sqlx::query!(
        "UPDATE gdpr_requests SET status = $2, resolved_by = $3, resolved_at = now(), \
                resolution_note = $4 \
         WHERE id = $1 AND status IN ('pending','in_progress')",
        id,
        status as RequestStatus,
        operator.map(UserId::as_uuid),
        note,
    )
    .execute(exec)
    .await?;
    Ok(result.rows_affected() > 0)
}

/// Withdraws the caller's own open request.
///
/// Scoped to the owner, so an id alone cannot cancel someone else's. Another user's request
/// returns `false`, the same as an unknown or resolved one, so ids cannot be enumerated.
///
/// # Errors
/// `Sqlx` only; another user's request is `Ok(false)`, indistinguishable from unknown/resolved.
pub async fn cancel_own<'e, E: PgExecutor<'e>>(
    exec: E,
    id: Uuid,
    user_id: UserId,
) -> DbResult<bool> {
    let result = sqlx::query!(
        "UPDATE gdpr_requests SET status = 'cancelled', resolved_at = now() \
         WHERE id = $1 AND user_id = $2 AND status IN ('pending','in_progress')",
        id,
        user_id.as_uuid(),
    )
    .execute(exec)
    .await?;
    Ok(result.rows_affected() > 0)
}

/// Extends a request's deadline, which Art. 12(3) allows by two further months.
///
/// Returns `false` for a deadline earlier than the current one, since the subject has been
/// told a date, and for an unknown or resolved id.
///
/// # Errors
/// `Sqlx` only; moving the deadline earlier, or an unknown/resolved id, is `Ok(false)` not an
/// error.
pub async fn extend_due<'e, E: PgExecutor<'e>>(
    exec: E,
    id: Uuid,
    due_at: OffsetDateTime,
) -> DbResult<bool> {
    let result = sqlx::query!(
        "UPDATE gdpr_requests SET due_at = $2 \
         WHERE id = $1 AND status IN ('pending','in_progress') AND $2 > due_at",
        id,
        due_at,
    )
    .execute(exec)
    .await?;
    Ok(result.rows_affected() > 0)
}

/// Returns how many requests are open and how many are past their deadline, for the console
/// overview.
///
/// A caller must render an error as unknown, not zero, or "no overdue requests" and "could not
/// tell" look the same.
///
/// # Errors
/// `Sqlx` only; a clean queue is `Ok((0, 0))`.
pub async fn open_counts<'e, E: PgExecutor<'e>>(exec: E) -> DbResult<(i64, i64)> {
    #[derive(FromRow)]
    struct Row {
        open: i64,
        overdue: i64,
    }
    let row = sqlx::query_as!(
        Row,
        "SELECT count(*) AS \"open!\", \
                count(*) FILTER (WHERE due_at < now()) AS \"overdue!\" \
         FROM gdpr_requests WHERE status IN ('pending','in_progress')",
    )
    .fetch_one(exec)
    .await?;
    Ok((row.open, row.overdue))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_unresolved_statuses_are_open() {
        assert!(RequestStatus::Pending.is_open());
        assert!(RequestStatus::InProgress.is_open());
        for closed in [
            RequestStatus::Completed,
            RequestStatus::Rejected,
            RequestStatus::Cancelled,
        ] {
            assert!(!closed.is_open());
        }
    }

    #[test]
    fn fulfilment_shape_matches_the_right_exercised() {
        assert!(RequestKind::Erasure.is_erasure());
        assert!(!RequestKind::Erasure.needs_export());
        for disclosing in [RequestKind::Access, RequestKind::Portability] {
            assert!(disclosing.needs_export());
            assert!(!disclosing.is_erasure());
        }
        // Decisions, not data movements: neither export nor erasure applies.
        for manual in [
            RequestKind::Rectification,
            RequestKind::Restriction,
            RequestKind::Objection,
        ] {
            assert!(!manual.needs_export());
            assert!(!manual.is_erasure());
        }
    }

    #[test]
    fn wire_tokens_match_the_sql_enum_labels() {
        assert_eq!(
            serde_json::to_string(&RequestKind::Portability).unwrap(),
            "\"portability\""
        );
        assert_eq!(
            serde_json::to_string(&RequestStatus::InProgress).unwrap(),
            "\"in_progress\""
        );
    }
}
