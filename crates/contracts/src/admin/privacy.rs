//! Privacy requests as the console reviews them.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

/// A queue entry as the operator sees it: the subject's identity (while they still exist) and
/// who is handling it.
///
/// The subject-facing fields are a nested `request`, not `#[serde(flatten)]`: `utoipa` gives a
/// flattened field no properties in the generated schema, so the typed client would lack every
/// field the queue renders.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[schema(as = AdminRequestRow)]
pub struct AdminPrivacyRequestView {
    /// The subject-facing record, exactly as the subject sees it on their own page.
    pub request: crate::me::PrivacyRequestView,
    /// The subject's id, or `None` once they have been erased.
    #[schema(value_type = Option<String>)]
    pub user_id: Option<Uuid>,
    /// The subject's username. `null` means the account is gone, which for a completed erasure
    /// is the expected end state, not missing data.
    pub username: Option<String>,
    /// The subject's address, `null` once they have been erased.
    pub email: Option<String>,
    /// Operator who claimed it, if any.
    pub claimed_by: Option<String>,
    /// Operator who resolved it, if resolved.
    pub resolved_by: Option<String>,
    /// Whether the Art. 12(3) deadline has passed with the request still open. Computed in SQL
    /// against `now()`, so the queue agrees with itself about what is late whatever a client's
    /// clock says.
    pub overdue: bool,
    /// Whether fulfilling this request means disclosing the subject's data export, which is
    /// whether the queue offers the "release export" action on this row.
    ///
    /// Derived on the server from `RequestKind::needs_export`, so the set of kinds that
    /// disclose an export lives in one place and the button the operator sees matches what the
    /// server accepts.
    pub needs_export: bool,
}
