//! Privacy requests as the console reviews them.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

/// A queue entry as the operator sees it: the subject's identity (while they still exist) and
/// who is handling it.
///
/// The subject-facing fields are a nested `request` rather than `#[serde(flatten)]`. Flattening
/// reads better on the wire, but `utoipa` cannot describe it: a flattened field contributes no
/// properties to the generated schema, so the typed client ended up with a struct missing every
/// field the queue actually renders. A nested object is honest about its shape and survives
/// code generation.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[schema(as = AdminRequestRow)]
pub struct AdminPrivacyRequestView {
    /// The subject-facing record, exactly as the subject sees it on their own page.
    pub request: crate::me::PrivacyRequestView,
    /// The subject's id, or `None` once they have been erased.
    #[schema(value_type = Option<String>)]
    pub user_id: Option<Uuid>,
    /// The subject's username. `None` means the account is gone — for a completed erasure
    /// that is the expected end state, not missing data.
    pub username: Option<String>,
    /// The subject's address, `null` once they have been erased.
    pub email: Option<String>,
    /// Operator who claimed it, if any.
    pub claimed_by: Option<String>,
    /// Operator who resolved it, if resolved.
    pub resolved_by: Option<String>,
    /// Whether the Art. 12(3) deadline has passed with the request still open. Computed in
    /// SQL against `now()` so the queue cannot disagree with itself about what is late
    /// depending on when a client's clock says it rendered.
    pub overdue: bool,
    /// Whether fulfilling this request means disclosing the subject's data export, i.e.
    /// whether the queue should offer the "release export" action on this row.
    ///
    /// Derived server-side from `RequestKind::needs_export` for the same reason `overdue` is
    /// computed in SQL: the console used to re-derive it from `kind` in its own `match`
    /// (FRONTEND F10), so the set of kinds that disclose an export lived in two places with
    /// nothing connecting them — and the one the operator sees is the one that decides which
    /// button appears. The server refuses the call either way, so the divergence would have
    /// shown as a button that does nothing rather than as a disclosure; still a bug, and one
    /// no test could reach.
    pub needs_export: bool,
}
