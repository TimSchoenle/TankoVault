//! The data-subject request queue (GDPR Chapter III).
//!
//! # Why a queue exists alongside the self-service endpoints
//!
//! `GET /v1/me/export` and `DELETE /v1/me` already satisfy Art. 15/17/20 for the common case,
//! immediately and without a human. They cannot satisfy the rest of the obligation:
//!
//! - **Art. 16 (rectification)** and **Art. 18/21 (restriction, objection)** have no
//!   self-service shape. They are decisions someone has to make.
//! - **Art. 12(3)** requires a response within one month. A duty with a deadline needs a
//!   tracked object with a due date; an HTTP call that either happened or did not cannot be
//!   overdue.
//! - **Art. 5(2)** requires the controller to be able to *demonstrate* compliance. That means
//!   a durable record of what was asked, when, by whom it was handled and how it ended.
//!
//! # Erasure and the record of erasure
//!
//! The table holds no copy of the subject's email or username. `user_id` is
//! `ON DELETE SET NULL`, so while a request is open its subject exists and is reachable by
//! join, and the moment an erasure completes the row degrades by itself into "an erasure
//! request was filed on D1 and completed on D2 by operator O" — an accountability record that
//! is no longer personal data. Snapshotting the email for the operator's convenience would
//! have re-created, in the compliance log, the identifier the erasure was supposed to destroy.

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
    /// Art. 15 — what do you hold about me.
    Access,
    /// Art. 20 — give it to me in a machine-readable form.
    Portability,
    /// Art. 16 — correct it.
    Rectification,
    /// Art. 17 — delete it.
    Erasure,
    /// Art. 18 — stop processing it, but keep it.
    Restriction,
    /// Art. 21 — stop processing it on legitimate-interest grounds.
    Objection,
}

impl RequestKind {
    /// Whether fulfilling this request means running the erasure cascade.
    ///
    /// Used to decide which operator action a queue entry offers, so the destructive button
    /// appears on exactly the requests that call for it.
    #[must_use]
    pub fn is_erasure(self) -> bool {
        matches!(self, Self::Erasure)
    }

    /// Whether fulfilling this request means disclosing the subject's data export.
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
    Pending,
    InProgress,
    Completed,
    Rejected,
    /// Withdrawn by the subject before it was resolved.
    Cancelled,
}

impl RequestStatus {
    /// Whether the request is still awaiting a resolution — the states the Art. 12(3) clock
    /// runs against and the only ones an operator or the subject may still act on.
    #[must_use]
    pub fn is_open(self) -> bool {
        matches!(self, Self::Pending | Self::InProgress)
    }
}

/// A request as the subject sees it, and the shape the operator queue extends.
#[derive(Debug, Clone, serde::Serialize)]
pub struct RequestRow {
    pub id: Uuid,
    pub kind: RequestKind,
    pub status: RequestStatus,
    pub detail: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    pub requested_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub due_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339::option")]
    pub resolved_at: Option<OffsetDateTime>,
    /// How it was resolved, or — for a rejection — why. Art. 12(4) obliges the controller to
    /// give reasons for a refusal, so a rejected request without this is incomplete.
    pub resolution_note: Option<String>,
}

/// A queue entry as the operator sees it: the subject's identity (while they still exist) and
/// who is handling it.
///
/// The subject-facing fields are a nested `request` rather than `#[serde(flatten)]`, matching
/// `tankovault_contracts::admin::AdminPrivacyRequestView` — the wire type this converts to,
/// whose docs record why a flattened field cannot be described in `OpenAPI`.
#[derive(Debug, Clone, serde::Serialize)]
pub struct AdminRequestRow {
    pub request: RequestRow,
    /// The subject's id, or `None` once they have been erased.
    pub user_id: Option<Uuid>,
    /// The subject's username. `None` means the account is gone — for a completed erasure
    /// that is the expected end state, not missing data.
    pub username: Option<String>,
    pub email: Option<String>,
    /// Operator who claimed it, if any.
    pub claimed_by: Option<String>,
    /// Operator who resolved it, if resolved.
    pub resolved_by: Option<String>,
    /// Whether the Art. 12(3) deadline has passed with the request still open. Computed in
    /// SQL against `now()` so the queue cannot disagree with itself about what is late
    /// depending on when a client's clock says it rendered.
    pub overdue: bool,
}

/// File a request on behalf of `user_id`.
///
/// Returns the created row so the caller can show the subject their due date immediately —
/// which is the one piece of information Art. 12(3) makes them entitled to up front.
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

/// A subject's own requests, newest first.
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

/// Whether the subject already has an unresolved request of this kind.
///
/// Guards against a subject filing the same request repeatedly — each duplicate would start
/// its own Art. 12(3) clock and the queue would show several deadlines for one obligation.
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

/// The operator queue. `only_open` restricts it to what still needs work; the full list is
/// the compliance record.
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
    // Open requests sort by deadline (most urgent first); the historical view sorts by recency,
    // because a resolved request's due date is no longer the interesting axis.
    let rows = sqlx::query_as!(
        Row,
        "SELECT r.id, r.kind AS \"kind: RequestKind\", r.status AS \"status: RequestStatus\", \
                r.detail, r.requested_at, r.due_at, r.resolved_at, r.resolution_note, \
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

/// One request by id, with its subject — what every operator action reads first to decide
/// whether the action is legal for that request's kind and status.
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
        "SELECT r.id, r.kind AS \"kind: RequestKind\", r.status AS \"status: RequestStatus\", \
                r.detail, r.requested_at, r.due_at, r.resolved_at, r.resolution_note, \
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

/// Take ownership of a pending request.
///
/// The `status = 'pending'` predicate is the whole point: two operators opening the queue at
/// the same time cannot both claim the same request, and the loser is told so rather than
/// silently overwriting the winner's claim.
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

/// Resolve a request as completed, rejected, or cancelled.
///
/// Only transitions *out of* an open state, so a resolution cannot be recorded twice and a
/// completed request cannot later be re-decided — an audit trail that can be rewritten is not
/// one. Returns `false` when the request was already resolved.
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

/// Withdraw one's own open request. Scoped to the owner, so the id alone is not authority to
/// cancel someone else's.
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

/// Extend a request's deadline (Art. 12(3) allows two further months for complex requests).
///
/// Refuses to move a deadline *earlier*: the subject has been told a date, and a controller
/// shortening its own window after the fact is not an extension.
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

/// How many open requests there are, and how many of those are past their deadline.
///
/// Surfaced on the console overview: an overdue data-subject request is a compliance breach in
/// progress, so it belongs on the front page rather than behind a tab someone has to remember
/// to open.
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
        // Rectification, restriction and objection are decisions, not data movements: they
        // must offer neither the export nor the erasure action.
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
