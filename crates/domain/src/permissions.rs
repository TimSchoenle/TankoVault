//! Fine-grained permissions — the system's *only* authorization primitive.
//!
//! # Why permissions and not roles
//!
//! This system previously authorized with a three-tier ordered role
//! (`user` < `operator` < `admin`) and a `require(at_least(role))` check. That shape has two
//! defects no amount of care at the call site can fix:
//!
//! 1. **It cannot express least privilege.** Letting someone triage the merge queue meant
//!    handing them provider editing, scan triggering, the audit trail and every user's
//!    linked-account state, because all of it sat behind the same tier. The only way to
//!    narrow a grant was to invent another tier, and tiers do not compose.
//! 2. **The requirement is invisible in the model.** `at_least(Operator)` says how
//!    privileged the caller must be, not *what they are allowed to do*, so nothing in the
//!    type system connects an endpoint to the capability it exercises. Reviewing "who can
//!    delete a provider" meant reading every handler.
//!
//! A [`Permission`] is a single, named capability. A principal holds a set of them, granted
//! individually and stored per user (`user_permissions`); there is no tier, no ordering and
//! no implication between permissions — holding `users.write` does not imply `users.read`,
//! because a grant that silently widens is exactly the bug this replaces. Endpoints that
//! need two capabilities ask for both.
//!
//! # Presets are a UI convenience, not a stored role
//!
//! Granting twenty permissions one at a time is hostile, so [`PermissionPreset`] names a few
//! common bundles ("Operator", "Administrator"). A preset is *expanded at the moment an
//! administrator applies it* and never persisted: nothing in the database or in an
//! authorization decision knows presets exist. That is the distinction from a role — a role
//! is a stored indirection that keeps applying, a preset is a starting point you then edit.
//!
//! # Naming
//!
//! `<surface>.<action>`, lowercase, dot-separated, stable forever: these strings are
//! persisted grants and appear in audit records, so renaming one is a migration.

use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fmt;
use std::str::FromStr;
use utoipa::ToSchema;

/// Everything a principal can be authorized to do.
///
/// The enum is exhaustive by design: a permission that exists only as a string somewhere
/// cannot be listed in the admin UI, cannot be spell-checked by the compiler, and cannot be
/// audited as a known capability. Adding a capability means adding a variant here and
/// nothing else.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, ToSchema,
)]
#[non_exhaustive]
pub enum Permission {
    // --- provider administration ---
    /// Read the provider list, per-provider statistics and health.
    #[serde(rename = "providers.read")]
    ProvidersRead,
    /// Edit an existing provider: name, `base_url` migration, adapter config, politeness.
    #[serde(rename = "providers.write")]
    ProvidersWrite,
    /// Register a brand-new provider.
    #[serde(rename = "providers.create")]
    ProvidersCreate,
    /// Delete a provider and cascade its source links.
    #[serde(rename = "providers.delete")]
    ProvidersDelete,
    /// Enable or disable a provider without deleting it.
    #[serde(rename = "providers.state")]
    ProvidersState,
    /// Run the live adapter dry-run and the challenge re-solve against a real site.
    #[serde(rename = "providers.test")]
    ProvidersTest,

    // --- scanning ---
    /// Read scan runs, queue state and task failures.
    #[serde(rename = "scans.read")]
    ScansRead,
    /// Trigger a scan run.
    #[serde(rename = "scans.run")]
    ScansRun,

    // --- canonicalisation ---
    /// Read the merge-candidate review queue.
    #[serde(rename = "merge.read")]
    MergeRead,
    /// Merge two series or dismiss a candidate.
    #[serde(rename = "merge.write")]
    MergeWrite,

    // --- external sync administration ---
    /// Read any user's linked accounts, series mappings and matching backlogs.
    #[serde(rename = "sync.admin.read")]
    SyncAdminRead,
    /// Force a pull/push/unlink or repoint a mapping on another user's behalf.
    #[serde(rename = "sync.admin.write")]
    SyncAdminWrite,

    // --- user administration ---
    /// Read the user directory and individual user detail.
    #[serde(rename = "users.read")]
    UsersRead,
    /// Edit a user's identity, and suspend or reinstate an account.
    #[serde(rename = "users.write")]
    UsersWrite,
    /// Grant and revoke another user's permissions. The meta-capability: a holder can
    /// escalate anyone (including themselves) to anything, so it is the one grant to treat
    /// as equivalent to full control.
    #[serde(rename = "users.permissions")]
    UsersPermissions,
    /// Erase another user's account and all their data.
    #[serde(rename = "users.delete")]
    UsersDelete,
    /// Revoke another user's active sessions.
    #[serde(rename = "users.sessions")]
    UsersSessions,

    // --- privacy / data protection ---
    /// Read the data-subject request queue.
    #[serde(rename = "privacy.read")]
    PrivacyRead,
    /// Progress, complete or reject a data-subject request, and run an erasure on its behalf.
    #[serde(rename = "privacy.write")]
    PrivacyWrite,
    /// Export another person's personal data in order to fulfil an access request. Separated
    /// from `privacy.write` because it is the one action that *discloses* the whole record
    /// rather than administering the queue.
    #[serde(rename = "privacy.export")]
    PrivacyExport,

    // --- observability ---
    /// Read the system-wide statistics rollup.
    #[serde(rename = "system.stats")]
    SystemStats,
    /// Read the privileged-action audit trail.
    #[serde(rename = "audit.read")]
    AuditRead,

    // --- feature flags ---
    /// Read the feature-flag catalogue and its resolved state.
    #[serde(rename = "flags.read")]
    FlagsRead,
    /// Turn a feature on or off for the whole deployment.
    #[serde(rename = "flags.write")]
    FlagsWrite,
}

impl Permission {
    /// Every permission, in declaration order — the order the admin UI lists them in.
    #[must_use]
    pub const fn all() -> &'static [Self] {
        &[
            Self::ProvidersRead,
            Self::ProvidersWrite,
            Self::ProvidersCreate,
            Self::ProvidersDelete,
            Self::ProvidersState,
            Self::ProvidersTest,
            Self::ScansRead,
            Self::ScansRun,
            Self::MergeRead,
            Self::MergeWrite,
            Self::SyncAdminRead,
            Self::SyncAdminWrite,
            Self::UsersRead,
            Self::UsersWrite,
            Self::UsersPermissions,
            Self::UsersDelete,
            Self::UsersSessions,
            Self::PrivacyRead,
            Self::PrivacyWrite,
            Self::PrivacyExport,
            Self::SystemStats,
            Self::AuditRead,
            Self::FlagsRead,
            Self::FlagsWrite,
        ]
    }

    /// The persisted, wire-level token. Stable forever; see the module docs.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ProvidersRead => "providers.read",
            Self::ProvidersWrite => "providers.write",
            Self::ProvidersCreate => "providers.create",
            Self::ProvidersDelete => "providers.delete",
            Self::ProvidersState => "providers.state",
            Self::ProvidersTest => "providers.test",
            Self::ScansRead => "scans.read",
            Self::ScansRun => "scans.run",
            Self::MergeRead => "merge.read",
            Self::MergeWrite => "merge.write",
            Self::SyncAdminRead => "sync.admin.read",
            Self::SyncAdminWrite => "sync.admin.write",
            Self::UsersRead => "users.read",
            Self::UsersWrite => "users.write",
            Self::UsersPermissions => "users.permissions",
            Self::UsersDelete => "users.delete",
            Self::UsersSessions => "users.sessions",
            Self::PrivacyRead => "privacy.read",
            Self::PrivacyWrite => "privacy.write",
            Self::PrivacyExport => "privacy.export",
            Self::SystemStats => "system.stats",
            Self::AuditRead => "audit.read",
            Self::FlagsRead => "flags.read",
            Self::FlagsWrite => "flags.write",
        }
    }

    /// The admin UI's grouping heading for this permission.
    #[must_use]
    pub const fn group(self) -> PermissionGroup {
        match self {
            Self::ProvidersRead
            | Self::ProvidersWrite
            | Self::ProvidersCreate
            | Self::ProvidersDelete
            | Self::ProvidersState
            | Self::ProvidersTest => PermissionGroup::Providers,
            Self::ScansRead | Self::ScansRun => PermissionGroup::Scanning,
            Self::MergeRead | Self::MergeWrite => PermissionGroup::Catalogue,
            Self::SyncAdminRead | Self::SyncAdminWrite => PermissionGroup::Sync,
            Self::UsersRead
            | Self::UsersWrite
            | Self::UsersPermissions
            | Self::UsersDelete
            | Self::UsersSessions => PermissionGroup::Users,
            Self::PrivacyRead | Self::PrivacyWrite | Self::PrivacyExport => {
                PermissionGroup::Privacy
            }
            Self::SystemStats | Self::AuditRead => PermissionGroup::Observability,
            Self::FlagsRead | Self::FlagsWrite => PermissionGroup::Flags,
        }
    }

    /// One-line description of what the capability allows, shown next to the toggle in the
    /// permission editor. Lives here rather than in the frontend so the authoritative
    /// wording sits with the definition and cannot describe a capability the backend no
    /// longer has.
    #[must_use]
    pub const fn description(self) -> &'static str {
        match self {
            Self::ProvidersRead => "View providers, their configuration and health.",
            Self::ProvidersWrite => {
                "Edit a provider's name, base URL, adapter config and crawl politeness."
            }
            Self::ProvidersCreate => "Register a new provider.",
            Self::ProvidersDelete => "Delete a provider and unlink its sources.",
            Self::ProvidersState => "Enable or disable a provider.",
            Self::ProvidersTest => "Run live adapter dry-runs and challenge re-solves.",
            Self::ScansRead => "View scan runs, queue progress and task failures.",
            Self::ScansRun => "Trigger scan runs.",
            Self::MergeRead => "View the series merge-candidate queue.",
            Self::MergeWrite => "Merge series and dismiss merge candidates.",
            Self::SyncAdminRead => "View any user's linked trackers and series mappings.",
            Self::SyncAdminWrite => {
                "Force sync pulls, pushes and unlinks, and repoint series mappings."
            }
            Self::UsersRead => "View the user directory and individual accounts.",
            Self::UsersWrite => "Edit user identities, suspend and reinstate accounts.",
            Self::UsersPermissions => {
                "Grant and revoke permissions. Equivalent to full control of the deployment."
            }
            Self::UsersDelete => "Erase a user account and all of its data.",
            Self::UsersSessions => "Sign another user out of every device.",
            Self::PrivacyRead => "View the data-subject request queue.",
            Self::PrivacyWrite => "Progress, complete or reject data-subject requests.",
            Self::PrivacyExport => "Export another person's personal data to fulfil a request.",
            Self::SystemStats => "View system-wide statistics.",
            Self::AuditRead => "Read the privileged-action audit trail.",
            Self::FlagsRead => "View which features are enabled.",
            Self::FlagsWrite => "Turn features on and off for the whole deployment.",
        }
    }
}

impl fmt::Display for Permission {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Error raised when a stored or submitted permission token is not a known capability.
///
/// Reaching this means a grant row names a permission this build does not have — after a
/// rollback that removed a capability, most likely. Callers treat it as "not held" rather
/// than failing the request, so an unknown grant can never *widen* access.
#[derive(Debug, Clone, thiserror::Error)]
#[error("unknown permission: {token:?}")]
pub struct ParsePermissionError {
    pub token: String,
}

impl FromStr for Permission {
    type Err = ParsePermissionError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::all()
            .iter()
            .copied()
            .find(|p| p.as_str() == s)
            .ok_or_else(|| ParsePermissionError {
                token: s.to_owned(),
            })
    }
}

/// The admin UI's grouping of permissions into sections.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, ToSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum PermissionGroup {
    Providers,
    Scanning,
    Catalogue,
    Sync,
    Users,
    Privacy,
    Observability,
    Flags,
}

impl PermissionGroup {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Providers => "providers",
            Self::Scanning => "scanning",
            Self::Catalogue => "catalogue",
            Self::Sync => "sync",
            Self::Users => "users",
            Self::Privacy => "privacy",
            Self::Observability => "observability",
            Self::Flags => "flags",
        }
    }
}

impl fmt::Display for PermissionGroup {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A named bundle offered in the permission editor as a starting point.
///
/// Not persisted and never consulted during an authorization decision — see the module docs
/// on why this is not a role by another name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum PermissionPreset {
    /// No permissions at all: an ordinary reader, which is what every new account is.
    Reader,
    /// Day-to-day operation of the catalogue: read everything operational, run scans, work
    /// the merge queue. Deliberately excludes anything that touches *people* — no user
    /// administration, no privacy queue, no cross-user sync writes.
    Operator,
    /// Everything an operator can do, plus provider lifecycle, user administration, the
    /// privacy queue and feature flags. Full control of the deployment.
    Administrator,
}

impl PermissionPreset {
    #[must_use]
    pub const fn all() -> &'static [Self] {
        &[Self::Reader, Self::Operator, Self::Administrator]
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Reader => "reader",
            Self::Operator => "operator",
            Self::Administrator => "administrator",
        }
    }

    /// The permissions this preset expands to.
    #[must_use]
    pub fn permissions(self) -> Vec<Permission> {
        match self {
            Self::Reader => Vec::new(),
            Self::Operator => vec![
                Permission::ProvidersRead,
                Permission::ProvidersWrite,
                Permission::ProvidersState,
                Permission::ProvidersTest,
                Permission::ScansRead,
                Permission::ScansRun,
                Permission::MergeRead,
                Permission::MergeWrite,
                Permission::SyncAdminRead,
                Permission::SystemStats,
                Permission::AuditRead,
                Permission::FlagsRead,
            ],
            // Spelled as "everything" rather than an enumerated list: a new capability must
            // be reachable by an administrator the moment it exists, and a hand-maintained
            // list would silently omit it.
            Self::Administrator => Permission::all().to_vec(),
        }
    }
}

impl fmt::Display for PermissionPreset {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for PermissionPreset {
    type Err = ParsePermissionError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::all()
            .iter()
            .copied()
            .find(|p| p.as_str() == s)
            .ok_or_else(|| ParsePermissionError {
                token: s.to_owned(),
            })
    }
}

/// The set of permissions a principal holds.
///
/// A `BTreeSet` rather than a `Vec`: membership is the only question ever asked of it, and
/// the ordering makes the serialised form (audit records, the capabilities endpoint)
/// deterministic, so two equal grant sets compare equal as JSON too.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PermissionSet(BTreeSet<Permission>);

impl PermissionSet {
    #[must_use]
    pub fn new() -> Self {
        Self(BTreeSet::new())
    }

    /// Build from stored tokens, **silently dropping** any token this build does not know.
    ///
    /// Dropping rather than failing is the safe direction: an unrecognised grant becomes no
    /// grant, so a stale row can only ever narrow access. Each drop is reported through
    /// `on_unknown` so the caller can log it instead of it vanishing.
    pub fn from_tokens<I, S>(tokens: I, mut on_unknown: impl FnMut(&str)) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut set = BTreeSet::new();
        for token in tokens {
            match Permission::from_str(token.as_ref()) {
                Ok(p) => {
                    set.insert(p);
                }
                Err(_) => on_unknown(token.as_ref()),
            }
        }
        Self(set)
    }

    /// Whether this principal holds `permission`.
    #[must_use]
    pub fn has(&self, permission: Permission) -> bool {
        self.0.contains(&permission)
    }

    /// Whether this principal holds *every* listed permission.
    #[must_use]
    pub fn has_all(&self, permissions: &[Permission]) -> bool {
        permissions.iter().all(|p| self.has(*p))
    }

    /// Whether this principal holds *any* listed permission. Used for the "may this account
    /// see the operator console at all?" question, which is a union, not a specific right.
    #[must_use]
    pub fn has_any(&self, permissions: &[Permission]) -> bool {
        permissions.iter().any(|p| self.has(*p))
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn insert(&mut self, permission: Permission) -> bool {
        self.0.insert(permission)
    }

    /// The held permissions in canonical order.
    pub fn iter(&self) -> impl Iterator<Item = Permission> + '_ {
        self.0.iter().copied()
    }

    /// The held permissions as their persisted tokens, for storage and audit detail.
    #[must_use]
    pub fn tokens(&self) -> Vec<&'static str> {
        self.0.iter().map(|p| p.as_str()).collect()
    }
}

impl FromIterator<Permission> for PermissionSet {
    fn from_iter<I: IntoIterator<Item = Permission>>(iter: I) -> Self {
        Self(iter.into_iter().collect())
    }
}

impl Serialize for PermissionSet {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.0.iter().collect::<Vec<_>>().serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for PermissionSet {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self(BTreeSet::deserialize(deserializer)?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_token_round_trips_and_is_unique() {
        let mut seen = BTreeSet::new();
        for &p in Permission::all() {
            assert_eq!(Permission::from_str(p.as_str()).unwrap(), p);
            assert!(seen.insert(p.as_str()), "duplicate token {}", p.as_str());
        }
        assert_eq!(seen.len(), Permission::all().len());
    }

    #[test]
    fn all_lists_every_variant() {
        // `all()` is hand-written, so it can fall behind the enum. Serialising each entry
        // proves the token table is complete for everything `all()` knows about; this
        // assertion pins the count so adding a variant without listing it fails here.
        assert_eq!(Permission::all().len(), 24);
    }

    #[test]
    fn serde_uses_the_persisted_token() {
        let json = serde_json::to_string(&Permission::UsersPermissions).unwrap();
        assert_eq!(json, "\"users.permissions\"");
    }

    #[test]
    fn unknown_tokens_are_dropped_not_granted() {
        let mut unknown = Vec::new();
        let set = PermissionSet::from_tokens(["users.read", "providers.teleport"], |t| {
            unknown.push(t.to_owned());
        });
        assert!(set.has(Permission::UsersRead));
        assert_eq!(set.len(), 1);
        assert_eq!(unknown, vec!["providers.teleport".to_owned()]);
    }

    #[test]
    fn permissions_do_not_imply_one_another() {
        // The whole point of the model: a write grant is not a read grant.
        let set: PermissionSet = [Permission::UsersWrite].into_iter().collect();
        assert!(set.has(Permission::UsersWrite));
        assert!(!set.has(Permission::UsersRead));
    }

    #[test]
    fn administrator_preset_covers_every_capability() {
        let set: PermissionSet = PermissionPreset::Administrator
            .permissions()
            .into_iter()
            .collect();
        assert!(set.has_all(Permission::all()));
    }

    #[test]
    fn operator_preset_cannot_touch_people() {
        let set: PermissionSet = PermissionPreset::Operator
            .permissions()
            .into_iter()
            .collect();
        assert!(set.has(Permission::ScansRun));
        for forbidden in [
            Permission::UsersRead,
            Permission::UsersWrite,
            Permission::UsersPermissions,
            Permission::UsersDelete,
            Permission::PrivacyRead,
            Permission::PrivacyExport,
            Permission::FlagsWrite,
            Permission::ProvidersDelete,
            Permission::ProvidersCreate,
            Permission::SyncAdminWrite,
        ] {
            assert!(!set.has(forbidden), "operator must not hold {forbidden}");
        }
    }

    #[test]
    fn reader_preset_is_empty() {
        assert!(PermissionPreset::Reader.permissions().is_empty());
    }

    #[test]
    fn set_serialises_as_a_sorted_token_array() {
        let set: PermissionSet = [Permission::ScansRun, Permission::ProvidersRead]
            .into_iter()
            .collect();
        // Declaration order, not insertion order.
        assert_eq!(
            serde_json::to_string(&set).unwrap(),
            r#"["providers.read","scans.run"]"#
        );
    }
}
