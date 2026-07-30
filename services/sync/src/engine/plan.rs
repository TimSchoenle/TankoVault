//! The pure half of one series' reconciliation (design v2 §B.3).
//!
//! [`plan_series`] and [`plan_merge`] decide *what* must happen — which side is written, whether
//! a conflict is left pending, what the refreshed common ancestor becomes — from values alone:
//! no pool, no provider, no I/O. [`super::reconcile::Reconciler`] owns the half that performs
//! it.
//!
//! Before ARCH-6 both halves were one 216-line method, so the merge rules could only be
//! exercised with a live pool and a provider behind them. Splitting them is what makes the
//! rules directly testable, which is the point — the tests at the bottom of this file pin the
//! two invariants that are easiest to break by accident and hardest to see in an integration
//! run: a conflict must *not* advance the snapshot, and one remote write must cover both fields.

use time::OffsetDateTime;

use tankovault_domain::WatchStatus;

use crate::mapping::{ConflictPolicy, MergeAction, Side, three_way};
use crate::provider::RemoteEntry;

/// One series' local sync state, as the merge sees it.
#[derive(Debug, Clone, Copy)]
pub(crate) struct LocalSide {
    /// Whole-chapter read frontier; `0.0` when the series has no progress row.
    pub(crate) progress: f64,
    /// When that frontier last changed; the epoch when there is no progress row.
    pub(crate) updated_at: OffsetDateTime,
    /// Watchlist status, `None` when the series is not on the watchlist at all.
    pub(crate) status: Option<WatchStatus>,
    /// Excluded from syncing with this provider (design v2 §A.5).
    pub(crate) excluded: bool,
}

/// The value each side held at the last successful reconciliation — the common ancestor the
/// three-way merge compares against. A `None` field means "no snapshot yet".
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct Ancestor {
    pub(crate) local_progress: Option<f64>,
    pub(crate) remote_progress: Option<f64>,
    pub(crate) local_status: Option<WatchStatus>,
    pub(crate) remote_status: Option<WatchStatus>,
}

/// What one field's merge decided, carrying both sides' values so the caller can write the
/// history or conflict row without re-deriving them.
#[derive(Debug, Clone, Copy)]
pub(crate) struct FieldPlan<T> {
    pub(crate) action: MergeAction,
    pub(crate) local: T,
    pub(crate) remote: T,
}

/// The part of a series' plan that can be decided without reading the common ancestor.
///
/// Deliberately separate from [`MergePlan`]: the snapshot read is the one query the merge needs
/// and neither other outcome does, so folding both steps into a single function would have cost
/// a round trip per excluded and per first-push series (PERF-13 removed exactly such reads).
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum SeriesPlan {
    /// Excluded from sync (§A.5): touch neither side.
    Skip,
    /// Not present on the remote yet: create it there. Local is authoritative for a first push.
    CreateRemote { status: WatchStatus, progress: f64 },
    /// Both sides exist — load the ancestor and call [`plan_merge`].
    Merge,
}

/// Everything the three-way merge decided for one series present on both sides.
#[derive(Debug)]
pub(crate) struct MergePlan {
    /// Set when the series was not on the local watchlist: import the remote status first, so
    /// the status merge below compares something meaningful.
    pub(crate) import_status: Option<WatchStatus>,
    pub(crate) progress: FieldPlan<f64>,
    pub(crate) status: FieldPlan<WatchStatus>,
    /// The single remote write covering both fields, set when either field wants to push local.
    pub(crate) remote_write: Option<(WatchStatus, f64)>,
    /// The refreshed common ancestor, `None` when a conflict was left pending — so the conflict
    /// is re-detected on the next run instead of silently becoming unresolvable.
    pub(crate) snapshot: Option<(f64, WatchStatus)>,
}

/// Decide the part of the plan that needs no common-ancestor snapshot.
#[must_use]
pub(crate) fn plan_series(local: &LocalSide, remote: Option<&RemoteEntry>) -> SeriesPlan {
    if local.excluded {
        return SeriesPlan::Skip;
    }
    match remote {
        Some(_) => SeriesPlan::Merge,
        None => SeriesPlan::CreateRemote {
            status: local.status.unwrap_or(WatchStatus::Reading),
            progress: local.progress,
        },
    }
}

/// Apply the three-way merge to both fields of a series present on both sides.
#[must_use]
pub(crate) fn plan_merge(
    local: &LocalSide,
    remote: &RemoteEntry,
    ancestor: &Ancestor,
    policy: ConflictPolicy,
) -> MergePlan {
    // The side whose own last-modified time is later, consulted only by `NewestWins`.
    let newer = if local.updated_at >= remote.updated_at {
        Side::Local
    } else {
        Side::Remote
    };

    // An absent local watchlist entry is imported from the remote first, so its status agrees
    // with the remote by construction and the status merge below is a no-op rather than a
    // spurious conflict.
    let import_status = if local.status.is_none() {
        Some(remote.status)
    } else {
        None
    };
    let local_status = local.status.unwrap_or(remote.status);

    let progress = three_way(
        local.progress,
        remote.progress,
        ancestor.local_progress,
        ancestor.remote_progress,
        policy,
        newer,
    )
    .action;
    let status = three_way(
        local_status,
        remote.status,
        ancestor.local_status,
        ancestor.remote_status,
        policy,
        newer,
    )
    .action;

    // One remote write covers both fields when either of them wants to push local; the other
    // field contributes whichever value is going to survive.
    let remote_write = (progress == MergeAction::PushLocal || status == MergeAction::PushLocal)
        .then(|| {
            let status_for_remote = match status {
                MergeAction::PushLocal | MergeAction::Noop => local_status,
                _ => remote.status,
            };
            let progress_for_remote = match progress {
                MergeAction::PushLocal | MergeAction::Noop => local.progress,
                _ => remote.progress,
            };
            (status_for_remote, progress_for_remote)
        });

    // Refresh the common ancestor only when nothing was left in conflict.
    let conflict = progress == MergeAction::Conflict || status == MergeAction::Conflict;
    let snapshot = (!conflict).then(|| {
        let agreed_progress = if progress == MergeAction::PullRemote {
            remote.progress
        } else {
            local.progress
        };
        let agreed_status = if status == MergeAction::PullRemote {
            remote.status
        } else {
            local_status
        };
        (agreed_progress, agreed_status)
    });

    MergePlan {
        import_status,
        progress: FieldPlan {
            action: progress,
            local: local.progress,
            remote: remote.progress,
        },
        status: FieldPlan {
            action: status,
            local: local_status,
            remote: remote.status,
        },
        remote_write,
        snapshot,
    }
}

#[cfg(test)]
mod tests {
    use super::{Ancestor, LocalSide, MergePlan, SeriesPlan, plan_merge, plan_series};
    use crate::mapping::{ConflictPolicy, MergeAction};
    use crate::provider::RemoteEntry;
    use tankovault_domain::{ContentType, WatchStatus};
    use time::OffsetDateTime;

    fn local(progress: f64, status: Option<WatchStatus>, updated_unix: i64) -> LocalSide {
        LocalSide {
            progress,
            updated_at: OffsetDateTime::from_unix_timestamp(updated_unix).unwrap(),
            status,
            excluded: false,
        }
    }

    fn remote(progress: f64, status: WatchStatus, updated_unix: i64) -> RemoteEntry {
        RemoteEntry {
            external_id: "1".to_owned(),
            titles: vec!["t".to_owned()],
            status,
            progress,
            updated_at: OffsetDateTime::from_unix_timestamp(updated_unix).unwrap(),
            start_year: None,
            content_type: ContentType::Unknown,
            tags: Vec::new(),
            authors: Vec::new(),
        }
    }

    fn merge(l: &LocalSide, r: &RemoteEntry, a: &Ancestor, policy: ConflictPolicy) -> MergePlan {
        plan_merge(l, r, a, policy)
    }

    #[test]
    fn an_excluded_series_is_skipped_whether_or_not_the_remote_has_it() {
        let mut l = local(5.0, Some(WatchStatus::Reading), 100);
        l.excluded = true;
        let r = remote(9.0, WatchStatus::Reading, 200);
        assert_eq!(plan_series(&l, Some(&r)), SeriesPlan::Skip);
        assert_eq!(plan_series(&l, None), SeriesPlan::Skip);
    }

    #[test]
    fn a_series_the_remote_does_not_have_is_created_from_local_state() {
        let l = local(12.0, Some(WatchStatus::Completed), 100);
        assert_eq!(
            plan_series(&l, None),
            SeriesPlan::CreateRemote {
                status: WatchStatus::Completed,
                progress: 12.0,
            }
        );
    }

    #[test]
    fn a_series_not_on_the_watchlist_defaults_to_reading_when_created_remotely() {
        let l = local(3.0, None, 100);
        assert_eq!(
            plan_series(&l, None),
            SeriesPlan::CreateRemote {
                status: WatchStatus::Reading,
                progress: 3.0,
            }
        );
    }

    /// The invariant that matters most: an unresolved conflict must leave the common ancestor
    /// where it was. Advancing it would make the two sides look converged on the next run, so
    /// the queued conflict could never be re-detected — it would become unresolvable while
    /// still sitting in the user's queue.
    #[test]
    fn a_conflict_does_not_advance_the_snapshot() {
        // Both sides moved away from a shared ancestor, to different values.
        let l = local(20.0, Some(WatchStatus::Reading), 300);
        let r = remote(30.0, WatchStatus::Reading, 400);
        let ancestor = Ancestor {
            local_progress: Some(10.0),
            remote_progress: Some(10.0),
            local_status: Some(WatchStatus::Reading),
            remote_status: Some(WatchStatus::Reading),
        };

        let plan = merge(&l, &r, &ancestor, ConflictPolicy::AskMe);

        assert_eq!(plan.progress.action, MergeAction::Conflict);
        assert!(plan.snapshot.is_none(), "conflicts must not converge");
        assert!(
            plan.remote_write.is_none(),
            "a conflict writes nothing to the remote"
        );
    }

    /// Progress and status both wanting to push must produce **one** remote write, not two —
    /// a provider charges rate-limit budget per call, and two writes race each other.
    #[test]
    fn one_remote_write_covers_both_fields() {
        let l = local(20.0, Some(WatchStatus::Completed), 300);
        let r = remote(10.0, WatchStatus::Reading, 200);
        let ancestor = Ancestor {
            local_progress: Some(10.0),
            remote_progress: Some(10.0),
            local_status: Some(WatchStatus::Reading),
            remote_status: Some(WatchStatus::Reading),
        };

        let plan = merge(&l, &r, &ancestor, ConflictPolicy::AskMe);

        assert_eq!(plan.progress.action, MergeAction::PushLocal);
        assert_eq!(plan.status.action, MergeAction::PushLocal);
        assert_eq!(
            plan.remote_write,
            Some((WatchStatus::Completed, 20.0)),
            "both local values go up in a single call"
        );
        assert_eq!(plan.snapshot, Some((20.0, WatchStatus::Completed)));
    }

    /// When only one field moved, the untouched field still has to travel in the write — the
    /// provider's `save_entry` sets both, so sending a stale value would clobber the remote.
    #[test]
    fn a_one_sided_push_carries_the_other_field_unchanged() {
        let l = local(20.0, Some(WatchStatus::Reading), 300);
        let r = remote(10.0, WatchStatus::Reading, 200);
        let ancestor = Ancestor {
            local_progress: Some(10.0),
            remote_progress: Some(10.0),
            local_status: Some(WatchStatus::Reading),
            remote_status: Some(WatchStatus::Reading),
        };

        let plan = merge(&l, &r, &ancestor, ConflictPolicy::AskMe);

        assert_eq!(plan.status.action, MergeAction::Noop);
        assert_eq!(plan.remote_write, Some((WatchStatus::Reading, 20.0)));
    }

    /// A series absent from the local watchlist is imported rather than treated as a status
    /// disagreement — otherwise every first sync would queue a status conflict per series.
    #[test]
    fn a_series_missing_locally_is_imported_instead_of_conflicting() {
        // Progress agrees, so only the status half is under test here.
        let l = local(7.0, None, 300);
        let r = remote(7.0, WatchStatus::Completed, 200);

        let plan = merge(&l, &r, &Ancestor::default(), ConflictPolicy::AskMe);

        assert_eq!(plan.import_status, Some(WatchStatus::Completed));
        assert_eq!(
            plan.status.action,
            MergeAction::Noop,
            "the imported status agrees with the remote by construction"
        );
        assert!(plan.remote_write.is_none());
        assert_eq!(plan.snapshot, Some((7.0, WatchStatus::Completed)));
    }

    /// With no common ancestor there is no way to tell which side moved, so unequal values are
    /// a genuine disagreement and the policy — not the merge — decides. `AskMe` queues it;
    /// every other policy picks a side. This is what a first sync against an already-populated
    /// remote library does, so it is worth stating rather than discovering.
    #[test]
    fn a_first_sync_disagreement_is_decided_by_policy_alone() {
        let l = local(0.0, None, 0);
        let r = remote(7.0, WatchStatus::Reading, 200);

        let asked = merge(&l, &r, &Ancestor::default(), ConflictPolicy::AskMe);
        assert_eq!(asked.progress.action, MergeAction::Conflict);
        assert!(asked.snapshot.is_none());

        let pulled = merge(&l, &r, &Ancestor::default(), ConflictPolicy::RemoteWins);
        assert_eq!(pulled.progress.action, MergeAction::PullRemote);
        assert_eq!(pulled.snapshot, Some((7.0, WatchStatus::Reading)));
    }

    /// No ancestor and equal values is convergence, not a conflict.
    #[test]
    fn agreement_without_an_ancestor_is_a_noop_that_records_the_snapshot() {
        let l = local(7.0, Some(WatchStatus::Reading), 300);
        let r = remote(7.0, WatchStatus::Reading, 200);

        let plan = merge(&l, &r, &Ancestor::default(), ConflictPolicy::AskMe);

        assert_eq!(plan.progress.action, MergeAction::Noop);
        assert_eq!(plan.status.action, MergeAction::Noop);
        assert!(plan.remote_write.is_none());
        assert_eq!(plan.snapshot, Some((7.0, WatchStatus::Reading)));
    }

    /// `NewestWins` breaks a tie by each side's own last-modified time, so the same inputs
    /// resolve in opposite directions depending only on the clock.
    #[test]
    fn newest_wins_follows_the_more_recently_touched_side() {
        let ancestor = Ancestor {
            local_progress: Some(10.0),
            remote_progress: Some(10.0),
            local_status: Some(WatchStatus::Reading),
            remote_status: Some(WatchStatus::Reading),
        };

        let local_newer = merge(
            &local(20.0, Some(WatchStatus::Reading), 900),
            &remote(30.0, WatchStatus::Reading, 100),
            &ancestor,
            ConflictPolicy::NewestWins,
        );
        assert_eq!(local_newer.progress.action, MergeAction::PushLocal);

        let remote_newer = merge(
            &local(20.0, Some(WatchStatus::Reading), 100),
            &remote(30.0, WatchStatus::Reading, 900),
            &ancestor,
            ConflictPolicy::NewestWins,
        );
        assert_eq!(remote_newer.progress.action, MergeAction::PullRemote);
        assert_eq!(remote_newer.snapshot, Some((30.0, WatchStatus::Reading)));
    }
}
