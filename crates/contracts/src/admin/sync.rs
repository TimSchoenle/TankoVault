//! External list sync: linked accounts, series mappings, remote entries and sync decisions.

use serde::{Deserialize, Serialize};
use serde_json::Value as Json;
use tankovault_domain::SeriesId;
use time::OffsetDateTime;
use utoipa::ToSchema;
use uuid::Uuid;

/// One row of the admin Sync console's "Linked accounts" table.
///
/// The automatic-sync policy columns and the pending-conflict count (design v2 §B.7) are
/// visibility only. They are the user's settings, and no operator route writes them.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[schema(as = AdminAccountRow)]
pub struct SyncAccountView {
    /// The local account the link belongs to.
    pub user_id: Uuid,
    /// Its username, joined in so the table renders from one fetch.
    pub username: String,
    /// Which external tracker, as a slug.
    pub provider: String,
    /// The handle on that tracker, `null` until a sync has read it.
    pub external_username: Option<String>,
    /// When a sync last completed for this link, `null` if none has.
    #[serde(with = "time::serde::rfc3339::option")]
    #[schema(value_type = Option<String>)]
    pub last_synced_at: Option<OffsetDateTime>,
    /// Why the most recent sync failed, `null` when the last one succeeded.
    pub last_error: Option<String>,
    /// The user's own setting for scheduled syncing. Read-only to an operator.
    pub auto_sync_enabled: bool,
    /// The user's own answer to a two-sided change: which side wins.
    pub conflict_policy: String,
    /// Conflicts on this link nobody has resolved.
    pub pending_conflicts: i64,
    /// When the link was made.
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String)]
    pub created_at: OffsetDateTime,
}

/// One row of the admin Sync console's "Series mappings" table.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[schema(as = AdminMappingRow)]
pub struct SyncMappingView {
    /// The local series.
    pub series_id: Uuid,
    /// Its canonical title.
    pub series_title: String,
    /// Which external tracker, as a slug.
    pub provider: String,
    /// The tracker's own id for the same work.
    pub external_id: String,
    /// When the mapping was last written.
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String)]
    pub updated_at: OffsetDateTime,
}

/// One row of the admin Sync console's "Assign queue" — a canonical series that has **no**
/// external mapping for the given provider yet, so an operator can review and assign one.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[schema(as = UnmappedSeriesRow)]
pub struct UnmappedSeriesView {
    /// The unmapped series.
    pub series_id: Uuid,
    /// Its canonical title.
    pub series_title: String,
    /// How many local sources back this series (a proxy for how confident a match is worth).
    pub source_count: i64,
}

/// One row of the admin console's "Unmatched remote entries" queue: a fetched provider entry
/// the auto-matcher could not confidently link to a local series.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[schema(as = RemoteEntryRow)]
pub struct RemoteEntryView {
    /// Whose list the entry came off.
    pub user_id: Uuid,
    /// That user's username.
    pub username: String,
    /// Which external tracker, as a slug.
    pub provider: String,
    /// The tracker's own id for the entry.
    pub external_id: String,
    /// Title as the tracker spells it, which is what the operator matches on.
    pub title: String,
    /// Tracking status as the tracker spells it.
    pub status: String,
    /// Chapters read, on the tracker's scale.
    pub progress: f64,
    /// Medium as the tracker spells it.
    pub content_type: String,
    /// Year the tracker gives, `null` when it gives none.
    pub start_year: Option<i32>,
}

/// One row of the automatic-sync decision journal.
///
/// Covers what `sync_history` never did: the entries that matched no local series, the series
/// skipped as excluded, the fields both sides already agreed on, and the scored title match
/// behind every mapping.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[schema(as = SyncDecision)]
pub struct SyncDecisionView {
    /// The journal row, which is what a revert or a flag names.
    pub id: Uuid,
    /// Groups one account reconciliation.
    pub run_id: Uuid,
    /// When the decision was taken.
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String)]
    pub decided_at: OffsetDateTime,
    /// Whose account was being reconciled.
    pub user_id: Uuid,
    /// That user's username, `null` once the account is erased.
    pub username: Option<String>,
    /// The local series, `null` for an entry that matched none.
    pub series_id: Option<SeriesId>,
    /// Its canonical title, `null` for the same reason.
    pub series_title: Option<String>,
    /// Which external tracker, as a slug.
    pub provider: String,
    /// The tracker's own id, `null` for a decision about a local series only.
    pub external_id: Option<String>,
    /// `match` | `progress` | `status` | `series` | `metadata`.
    pub scope: String,
    /// `matched` | `unmatched` | `pull` | `push` | `create_remote` | `conflict` | `noop`
    /// | `skipped` | `import_status` | `enriched` | `unmapped`.
    pub action: String,
    /// The stable slug for why: `only_remote_changed`, `both_sides_changed_policy_remote_wins`,
    /// `title_match_above_threshold`, `blocked_by_operator`, …
    pub reason: String,
    /// The conflict policy in force, `null` where the decision needed none.
    pub policy: Option<String>,
    /// Whether anything was actually written. A run is mostly considerations.
    pub applied: bool,
    /// The local value before the decision.
    pub local_before: Option<String>,
    /// The local value after it, equal to `local_before` when nothing was written.
    pub local_after: Option<String>,
    /// The remote value before the decision.
    pub remote_before: Option<String>,
    /// The remote value after it.
    pub remote_after: Option<String>,
    /// The three-way merge's common ancestor. Without it a pull cannot be told from a clobber.
    pub ancestor_local: Option<String>,
    /// The remote side of that ancestor.
    pub ancestor_remote: Option<String>,
    /// Title-match similarity in `[0,1]`, `null` outside a match decision.
    pub match_score: Option<f32>,
    /// Stable slugs for the matching rules that fired. Empty outside a match decision.
    pub match_signals: Vec<String>,
    /// Which titles matched, the scored terms, the runner-up, and the provider's own metadata.
    pub evidence: Json,
    /// When the decision was undone, `null` while it stands.
    #[serde(with = "time::serde::rfc3339::option")]
    #[schema(value_type = Option<String>)]
    pub reverted_at: Option<OffsetDateTime>,
    /// Who undid it.
    pub reverted_by: Option<Uuid>,
    /// What they gave as the reason.
    pub revert_reason: Option<String>,
    /// When an operator marked the decision wrong, `null` if nobody has.
    #[serde(with = "time::serde::rfc3339::option")]
    #[schema(value_type = Option<String>)]
    pub flagged_at: Option<OffsetDateTime>,
    /// Who marked it.
    pub flagged_by: Option<Uuid>,
    /// What they gave as the reason.
    pub flag_reason: Option<String>,
}

/// What undoing a sync decision put back.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[schema(as = SyncReverted)]
pub struct SyncRevertedView {
    /// The journal row that was undone.
    pub decision_id: Uuid,
    /// `local_progress` | `local_status` | `watchlist_entry` | `remote_entry` | `match`.
    pub restored: String,
    /// The value the restored side now holds.
    pub value: Option<String>,
    /// Whether the revert also refused the title match permanently.
    pub blocked_match: bool,
}
