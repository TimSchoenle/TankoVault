//! The user directory, one user's detail, and their permission grants.

use serde::{Deserialize, Serialize};
use tankovault_domain::AccountStatus;
use time::OffsetDateTime;
use utoipa::ToSchema;
use uuid::Uuid;

/// One row of the operator user directory.
///
/// Carries a grant count, not the grants: the detail view shows a user's capabilities, and the
/// count answers the question the list is scanned for, which accounts are privileged at all.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[schema(as = DirectoryRow)]
pub struct UserDirectoryRow {
    /// The account.
    #[schema(value_type = String)]
    pub id: Uuid,
    /// Its sign-in address.
    pub email: String,
    /// Its display handle.
    pub username: String,
    /// Whether it may authenticate at all.
    pub status: AccountStatus,
    /// Whether the address has been confirmed. An unverified account that has existed for
    /// months is usually an abandoned registration.
    pub email_verified: bool,
    /// How many permissions this account holds. `0` is an ordinary reader.
    ///
    /// It does not measure how much an account may do: the super user holds one grant and can
    /// do everything, which `is_super_user` reports.
    pub permission_count: i64,
    /// Whether this account holds the super-user grant.
    ///
    /// Published separately because that grant is absent from the permission catalogue. A
    /// client reconciling grants against the catalogue would otherwise render the deployment
    /// owner as an account holding nothing.
    pub is_super_user: bool,
    /// How many series the user tracks, the cheapest signal of an account in use.
    pub tracked_count: i64,
    /// Its most recent sign-in, `null` for an account that has never signed in.
    #[serde(with = "time::serde::rfc3339::option")]
    #[schema(value_type = Option<String>)]
    pub last_login_at: Option<OffsetDateTime>,
    /// When it was registered.
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String)]
    pub created_at: OffsetDateTime,
}

/// A page of the directory with the total across pages, so the UI can render "showing 1 to 25 of
/// 312" without a second request.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[schema(as = DirectoryPage)]
pub struct UserDirectoryPage {
    /// The page, in the directory's own order.
    pub users: Vec<UserDirectoryRow>,
    /// Total matching the current search, ignoring `limit` and `offset`.
    pub total: i64,
}

/// Everything the user-detail panel shows except the grant list, which is fetched separately so
/// the panel can refresh just that part after an edit.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[schema(as = UserDetail)]
pub struct UserDetailView {
    /// The account.
    #[schema(value_type = String)]
    pub id: Uuid,
    /// Its sign-in address.
    pub email: String,
    /// Its display handle.
    pub username: String,
    /// Whether it may authenticate at all.
    pub status: AccountStatus,
    /// Whether the address has been confirmed.
    pub email_verified: bool,
    /// What an operator gave when suspending, `null` while the account is active.
    pub suspension_reason: Option<String>,
    /// When it was suspended, `null` while it is active.
    #[serde(with = "time::serde::rfc3339::option")]
    #[schema(value_type = Option<String>)]
    pub suspended_at: Option<OffsetDateTime>,
    /// Its most recent sign-in, `null` for an account that has never signed in.
    #[serde(with = "time::serde::rfc3339::option")]
    #[schema(value_type = Option<String>)]
    pub last_login_at: Option<OffsetDateTime>,
    /// When it was registered.
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String)]
    pub created_at: OffsetDateTime,
    /// Live login sessions, which tell an operator whether a suspension takes effect without
    /// also revoking them.
    pub active_sessions: i64,
    /// Series the user tracks.
    pub tracked_count: i64,
    /// Linked external trackers, so an operator can see the account has third-party
    /// credentials at rest before deciding to erase it.
    pub linked_accounts: i64,
    /// Unresolved data-subject requests filed by this user. An account with an open erasure
    /// request must not be quietly edited.
    pub open_privacy_requests: i64,
}

/// A single grant, with its provenance, for the user-detail view.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[schema(as = GrantRow)]
pub struct GrantView {
    /// The permission token. A string, not the enum, because a grant for a capability this
    /// build does not know must still be visible to an administrator, who then needs to remove
    /// it.
    pub permission: String,
    /// Whether this build recognises the token. `false` means the grant is inert.
    pub known: bool,
    /// When the grant was made.
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String)]
    pub granted_at: OffsetDateTime,
    /// Who granted it, `null` for a grant made by a migration or by an administrator since
    /// erased.
    pub granted_by: Option<String>,
}
