//! Fine-grained capabilities, which are this system's only authorization primitive.
//!
//! A principal holds an unordered set of them, with no implication between grants. See
//! [`Permission`] for the invariant that protects.
//!
//! [`Permission::SuperUser`] is the single exception, and deliberately not a role: it is one
//! grant that answers `true` to every check, held by the account the bootstrap migrator creates
//! and by no one else, because the permission editor never offers it.

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

    /// Read scan runs, queue state and task failures.
    #[serde(rename = "scans.read")]
    ScansRead,
    /// Trigger a scan run.
    #[serde(rename = "scans.run")]
    ScansRun,

    /// Read the merge-candidate review queue.
    #[serde(rename = "merge.read")]
    MergeRead,
    /// Merge two series or dismiss a candidate.
    #[serde(rename = "merge.write")]
    MergeWrite,
    /// Read the automatic-merge decision journal: the itemised score, the rule that decided
    /// each pair, the guards that held one back, and what a revert would restore.
    #[serde(rename = "merge.audit")]
    MergeAudit,
    /// Undo an automatic merge, or flag one as wrong. Separate from `merge.write` because it is
    /// the only capability that can *resurrect* a deleted series, and because an operator
    /// trusted to work the review queue is not thereby trusted to reverse the sweep.
    #[serde(rename = "merge.revert")]
    MergeRevert,

    /// Read the catalogue maintenance surface: the operator's series list and the
    /// deployment-wide catalogue totals. Distinct from public browsing, which needs no grant.
    #[serde(rename = "catalogue.read")]
    CatalogueRead,
    /// Delete series from the catalogue — one at a time, in bulk, or the whole thing.
    ///
    /// Deliberately *not* implied by `merge.write`, which also removes series: a merge folds one
    /// row into another and keeps every reader's progress, while this discards both. It is the
    /// only capability in the system that can empty the deployment.
    #[serde(rename = "catalogue.delete")]
    CatalogueDelete,

    /// Read the recommender's tuning registry and its model-health figures.
    #[serde(rename = "recsys.read")]
    RecsysRead,
    /// Change a tuning value, or trigger a model rebuild. Separated from `recsys.read`
    /// because reading the panel is diagnostic and writing to it changes what every reader
    /// on the deployment is shown.
    #[serde(rename = "recsys.write")]
    RecsysWrite,

    /// Read any user's linked accounts, series mappings and matching backlogs.
    #[serde(rename = "sync.admin.read")]
    SyncAdminRead,
    /// Force a pull/push/unlink or repoint a mapping on another user's behalf.
    #[serde(rename = "sync.admin.write")]
    SyncAdminWrite,
    /// Read the automatic-sync decision journal across every user: what each reconciliation
    /// matched, wrote, skipped, and why. Separate from `sync.admin.read` because it discloses
    /// every reader's progress *history* rather than the current state of their links.
    #[serde(rename = "sync.audit")]
    SyncAudit,
    /// Undo a sync decision, or flag one wrong and refuse the match it made.
    #[serde(rename = "sync.revert")]
    SyncRevert,

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

    /// Read the system-wide statistics rollup.
    #[serde(rename = "system.stats")]
    SystemStats,
    /// Read the privileged-action audit trail.
    #[serde(rename = "audit.read")]
    AuditRead,

    /// Read the feature-flag catalogue and its resolved state.
    #[serde(rename = "flags.read")]
    FlagsRead,
    /// Turn a feature on or off for the whole deployment.
    #[serde(rename = "flags.write")]
    FlagsWrite,

    /// Every capability there is, including ones added by a later release.
    ///
    /// The deployment owner, held by the first account the bootstrap migrator creates and by
    /// nobody else. It is not in [`Permission::grantable`], so no catalogue, preset or seed
    /// loop can hand it out, and the permission editor refuses it explicitly — a holder of
    /// `users.permissions` can escalate anyone to everything *enumerable*, but cannot mint an
    /// account that outlives the next capability the codebase gains.
    ///
    /// Declared last on purpose: the variant order is the sort order of a stored grant set, so
    /// appending leaves every existing serialised set byte-identical.
    #[serde(rename = "system.superuser")]
    SuperUser,
}

impl Permission {
    /// Every permission this build defines, in declaration order.
    ///
    /// Includes [`Self::SuperUser`], so parsing and exhaustiveness checks see the whole enum.
    /// Anything that *hands out* a permission wants [`Self::grantable`] instead.
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
            Self::MergeAudit,
            Self::MergeRevert,
            Self::CatalogueRead,
            Self::CatalogueDelete,
            Self::RecsysRead,
            Self::RecsysWrite,
            Self::SyncAdminRead,
            Self::SyncAdminWrite,
            Self::SyncAudit,
            Self::SyncRevert,
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
            Self::SuperUser,
        ]
    }

    /// Every permission an administrator may grant: [`Self::all`] minus [`Self::SuperUser`], in
    /// the same order — what the admin UI lists.
    ///
    /// This exclusion is the enforcement point for "one super user, minted by the migrator". A
    /// catalogue, a preset expansion or a seed loop built from this list cannot grant it, and
    /// none of them has to remember not to.
    #[must_use]
    pub fn grantable() -> Vec<Self> {
        Self::all()
            .iter()
            .copied()
            .filter(|p| !p.is_super_user())
            .collect()
    }

    /// Whether exercising this capability changes durable state, or reaches into another
    /// person's account.
    ///
    /// Consulted by the API's authorization funnel, which demands a fresh second-factor
    /// presentation (a "step-up") before any mutating capability is exercised. Reading the
    /// console is left alone: an administrator who had to re-authenticate to load a dashboard
    /// would keep a standing elevation open all day, which is worse than not prompting at all.
    ///
    /// Exhaustive by construction — a capability added later does not compile until it is
    /// classified here, which is the point. Guessing from the token's `.write` suffix would
    /// leave `scans.run`, `merge.revert` and `users.sessions` silently on the read side.
    ///
    /// `privacy.export` counts as mutating despite reading nothing of this system's own: it
    /// discloses another person's entire record, which is the single highest-consequence thing
    /// an operator can do with one request.
    #[must_use]
    pub const fn is_mutating(self) -> bool {
        match self {
            Self::ProvidersRead
            | Self::ProvidersTest
            | Self::ScansRead
            | Self::MergeRead
            | Self::MergeAudit
            | Self::CatalogueRead
            | Self::RecsysRead
            | Self::SyncAdminRead
            | Self::SyncAudit
            | Self::UsersRead
            | Self::PrivacyRead
            | Self::SystemStats
            | Self::AuditRead
            | Self::FlagsRead => false,
            Self::ProvidersWrite
            | Self::ProvidersCreate
            | Self::ProvidersDelete
            | Self::ProvidersState
            | Self::ScansRun
            | Self::MergeWrite
            | Self::MergeRevert
            | Self::CatalogueDelete
            | Self::RecsysWrite
            | Self::SyncAdminWrite
            | Self::SyncRevert
            | Self::UsersWrite
            | Self::UsersPermissions
            | Self::UsersDelete
            | Self::UsersSessions
            | Self::PrivacyWrite
            | Self::PrivacyExport
            | Self::FlagsWrite
            | Self::SuperUser => true,
        }
    }

    /// Whether this is the grant that answers every check.
    #[must_use]
    pub const fn is_super_user(self) -> bool {
        matches!(self, Self::SuperUser)
    }

    /// The persisted wire token (`<surface>.<action>`), stable forever — renaming one
    /// orphans every stored grant that used the old string.
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
            Self::MergeAudit => "merge.audit",
            Self::MergeRevert => "merge.revert",
            Self::CatalogueRead => "catalogue.read",
            Self::CatalogueDelete => "catalogue.delete",
            Self::RecsysRead => "recsys.read",
            Self::RecsysWrite => "recsys.write",
            Self::SyncAdminRead => "sync.admin.read",
            Self::SyncAdminWrite => "sync.admin.write",
            Self::SyncAudit => "sync.audit",
            Self::SyncRevert => "sync.revert",
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
            Self::SuperUser => "system.superuser",
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
            Self::MergeRead
            | Self::MergeWrite
            | Self::MergeAudit
            | Self::MergeRevert
            | Self::CatalogueRead
            | Self::CatalogueDelete
            | Self::RecsysRead
            | Self::RecsysWrite => PermissionGroup::Catalogue,
            Self::SyncAdminRead | Self::SyncAdminWrite | Self::SyncAudit | Self::SyncRevert => {
                PermissionGroup::Sync
            }
            // `SuperUser` is never rendered — the editor lists `grantable()` — but it is
            // grouped with the other privilege-over-people capabilities so the answer is not a
            // lie if it ever is.
            Self::UsersRead
            | Self::UsersWrite
            | Self::UsersPermissions
            | Self::UsersDelete
            | Self::UsersSessions
            | Self::SuperUser => PermissionGroup::Users,
            Self::PrivacyRead | Self::PrivacyWrite | Self::PrivacyExport => {
                PermissionGroup::Privacy
            }
            Self::SystemStats | Self::AuditRead => PermissionGroup::Observability,
            Self::FlagsRead | Self::FlagsWrite => PermissionGroup::Flags,
        }
    }

    /// Shown next to the toggle in the permission editor. Lives here, not in the frontend,
    /// so it can never describe a capability the backend no longer has.
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
            Self::MergeAudit => "Inspect why the automatic merge merged each pair.",
            Self::MergeRevert => "Undo an automatic merge and flag it as wrong.",
            Self::CatalogueRead => "View the catalogue maintenance list and its totals.",
            Self::CatalogueDelete => {
                "Delete series in bulk and purge the catalogue. Takes readers' watchlist \
                 entries and reading progress for those series with it."
            }
            Self::RecsysRead => "View recommendation tuning and model health.",
            Self::RecsysWrite => "Change recommendation tuning and trigger model rebuilds.",
            Self::SyncAdminRead => "View any user's linked trackers and series mappings.",
            Self::SyncAdminWrite => {
                "Force sync pulls, pushes and unlinks, and repoint series mappings."
            }
            Self::SyncAudit => "Inspect why the automatic sync matched and wrote each value.",
            Self::SyncRevert => "Undo a sync decision and refuse the match it made.",
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
            Self::SuperUser => {
                "Every capability, present and future. Held only by the account the installer \
                 created; cannot be granted here."
            }
        }
    }
}

impl fmt::Display for Permission {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A stored or submitted token names no known capability — most likely a grant surviving a
/// rollback. Treated as "not held" rather than a failure, so it can only narrow access.
#[derive(Debug, Clone, thiserror::Error)]
#[error("unknown permission: {token:?}")]
pub struct ParsePermissionError {
    /// The token that matched no permission in this build.
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
    /// Registering providers and editing their adapter configuration.
    Providers,
    /// Dispatching, cancelling and inspecting scan runs.
    Scanning,
    /// Editing series, merging duplicates and purging catalogue rows.
    Catalogue,
    /// `AniList` account links and the conflicts they raise.
    Sync,
    /// Accounts, their grants and their sessions.
    Users,
    /// The export and erasure queue.
    Privacy,
    /// Metrics, traces and the health surfaces.
    Observability,
    /// Feature switches and the recommender tunables.
    Flags,
}

impl PermissionGroup {
    /// The token this group serializes to, which is also its section key in the console.
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
    /// Every preset, weakest first.
    #[must_use]
    pub const fn all() -> &'static [Self] {
        &[Self::Reader, Self::Operator, Self::Administrator]
    }

    /// The token this preset is named by in the console and in audit records.
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
                // Reading the merge journal is diagnostic and belongs with working the queue;
                // reversing the sweep does not, and is left to an administrator.
                Permission::MergeAudit,
                // Reading the maintenance list is diagnostic; emptying the deployment from it
                // is not, so `catalogue.delete` stays with the administrator.
                Permission::CatalogueRead,
                Permission::RecsysRead,
                Permission::SyncAdminRead,
                Permission::SystemStats,
                Permission::AuditRead,
                Permission::FlagsRead,
            ],
            // "Everything grantable", not an enumerated list — a hand-maintained one would
            // silently exclude a new capability from the administrator preset. Not `all()`:
            // the preset must never expand to the super user grant.
            Self::Administrator => Permission::grantable(),
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
/// A `BTreeSet`, not a `Vec`: membership is the only question ever asked of it, and the
/// ordering makes the serialised form (audit records, capabilities endpoint) deterministic.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PermissionSet(BTreeSet<Permission>);

impl PermissionSet {
    /// An empty set, which is what an account with no grants holds.
    #[must_use]
    pub fn new() -> Self {
        Self(BTreeSet::new())
    }

    /// Build from stored tokens, silently dropping any this build does not recognise — a
    /// stale row can only narrow access, never grant something unknown. Each drop is
    /// reported via `on_unknown` for logging.
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
    ///
    /// A super user holds everything, including capabilities added after their grant was
    /// written — the one implication in the model, and the reason
    /// [`Permission::SuperUser`] cannot be handed out through the API.
    #[must_use]
    pub fn has(&self, permission: Permission) -> bool {
        self.is_super_user() || self.0.contains(&permission)
    }

    /// Whether this principal *is* the super user, as opposed to holding a permission through
    /// one. Every site that writes or displays grants needs this rather than [`Self::has`],
    /// which cannot tell the two apart.
    #[must_use]
    pub fn is_super_user(&self) -> bool {
        self.0.contains(&Permission::SuperUser)
    }

    /// Whether this principal holds *every* listed permission.
    #[must_use]
    pub fn has_all(&self, permissions: &[Permission]) -> bool {
        permissions.iter().all(|p| self.has(*p))
    }

    /// Whether this principal holds *any* listed permission — a union check, not a specific right.
    #[must_use]
    pub fn has_any(&self, permissions: &[Permission]) -> bool {
        permissions.iter().any(|p| self.has(*p))
    }

    /// Whether this principal holds nothing at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// How many distinct permissions are held.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Grants `permission`, answering whether it was not already held.
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

    /// The exact read-only set, spelled out so a capability cannot join it by accident.
    ///
    /// The bug this pins is a quiet one: classify a mutating capability as read-only and the
    /// step-up prompt in front of it simply stops appearing. Nothing fails, no test goes red,
    /// and the protection is gone from that one action until somebody notices the console no
    /// longer asks. `.write`-suffix guessing produces exactly that outcome for `scans.run`,
    /// `merge.revert`, `users.sessions` and `privacy.export`, which is why the classification is
    /// an exhaustive match and this list is its mirror.
    #[test]
    fn only_the_declared_capabilities_are_read_only() {
        let read_only: BTreeSet<Permission> = [
            Permission::ProvidersRead,
            Permission::ProvidersTest,
            Permission::ScansRead,
            Permission::MergeRead,
            Permission::MergeAudit,
            Permission::CatalogueRead,
            Permission::RecsysRead,
            Permission::SyncAdminRead,
            Permission::SyncAudit,
            Permission::UsersRead,
            Permission::PrivacyRead,
            Permission::SystemStats,
            Permission::AuditRead,
            Permission::FlagsRead,
        ]
        .into_iter()
        .collect();

        for &p in Permission::all() {
            assert_eq!(
                !p.is_mutating(),
                read_only.contains(&p),
                "{} is classified {}, which this list disagrees with",
                p.as_str(),
                if p.is_mutating() {
                    "mutating"
                } else {
                    "read-only"
                }
            );
        }
    }

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
        // `all()` is hand-written and can drift from the enum; bump this count when adding
        // a variant, or a forgotten one slips through unnoticed.
        assert_eq!(Permission::all().len(), 33);
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
        assert!(set.has_all(&Permission::grantable()));
    }

    /// The super user grant is what makes "everything" mean everything, so the two ways of
    /// asking must not diverge: `has` says yes to a capability the set does not contain, while
    /// `is_super_user` still distinguishes the holder from a fully-granted administrator.
    #[test]
    fn the_super_user_holds_every_capability() {
        let set: PermissionSet = [Permission::SuperUser].into_iter().collect();
        assert!(set.has_all(Permission::all()));
        assert!(set.is_super_user());
        assert_eq!(set.len(), 1, "one stored grant, not an expansion");

        let admin: PermissionSet = PermissionPreset::Administrator
            .permissions()
            .into_iter()
            .collect();
        assert!(!admin.is_super_user());
    }

    /// Nothing that hands out permissions may offer the super user grant: the editor's
    /// catalogue, the presets and the bootstrap seed loop are all built from `grantable()`.
    #[test]
    fn the_super_user_is_not_grantable() {
        assert!(!Permission::grantable().contains(&Permission::SuperUser));
        assert_eq!(Permission::grantable().len(), Permission::all().len() - 1);
        for preset in PermissionPreset::all() {
            assert!(
                !preset.permissions().contains(&Permission::SuperUser),
                "preset {preset} expands to the super user grant"
            );
        }
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
            Permission::RecsysWrite,
            Permission::ProvidersDelete,
            Permission::ProvidersCreate,
            Permission::SyncAdminWrite,
            // Undoing the sweep resurrects a deleted series, and the sync journal discloses
            // every reader's progress history. Neither belongs to day-to-day operation.
            Permission::MergeRevert,
            Permission::SyncAudit,
            Permission::SyncRevert,
            // The one capability that can empty the deployment. Day-to-day operation of the
            // catalogue does not include discarding it.
            Permission::CatalogueDelete,
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
