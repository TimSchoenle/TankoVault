//! Response bodies for the signed-in user's own surface (`/v1/me/*`).
//!
//! Here for the same reason as [`crate::admin`]: these shapes were repository row structs
//! carrying `ToSchema`, which made a `SELECT` column rename a silent, uncatchable rewrite of
//! the public API. See that module's header for the full reasoning and for why the published
//! `OpenAPI` component names are pinned with `#[schema(as = ...)]`.

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use utoipa::ToSchema;
use uuid::Uuid;

/// Lifetime reading stats for the Home/Profile headline (frontend §9.3 `GET /v1/me/stats`).
/// `chapters_read` is the sum of whole chapters below each series' last-read marker — an
/// honest proxy over stored progress (there is no per-chapter read-event log, so a daily
/// "streak" is intentionally omitted rather than fabricated).
#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
#[schema(as = MeStats)]
pub struct MeStatsView {
    pub tracking: i64,
    pub reading: i64,
    pub completed: i64,
    pub chapters_read: i64,
    pub unread: i64,
}

/// What a data subject is exercising (GDPR Chapter III). Mirrors the `gdpr_request_kind` SQL
/// enum, whose `sqlx`-typed counterpart stays in `tankovault_db` — this crate must not depend
/// on the driver.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[schema(as = RequestKind)]
#[serde(rename_all = "snake_case")]
pub enum PrivacyRequestKind {
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

/// Where a data-subject request is in its lifecycle. Mirrors the `gdpr_request_status` SQL
/// enum; see [`PrivacyRequestKind`] for why the mirror exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[schema(as = RequestStatus)]
#[serde(rename_all = "snake_case")]
pub enum PrivacyRequestStatus {
    Pending,
    InProgress,
    Completed,
    Rejected,
    /// Withdrawn by the subject before it was resolved.
    Cancelled,
}

/// A request as the subject sees it, and the shape the operator queue extends.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[schema(as = RequestRow)]
pub struct PrivacyRequestView {
    pub id: Uuid,
    pub kind: PrivacyRequestKind,
    pub status: PrivacyRequestStatus,
    pub detail: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String)]
    pub requested_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String)]
    pub due_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339::option")]
    #[schema(value_type = Option<String>)]
    pub resolved_at: Option<OffsetDateTime>,
    /// How it was resolved, or — for a rejection — why. Art. 12(4) obliges the controller to
    /// give reasons for a refusal, so a rejected request without this is incomplete.
    pub resolution_note: Option<String>,
}
