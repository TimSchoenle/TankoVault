//! The *linked group*: every local series mapped to one external id at one provider.
//!
//! `sync_mappings` is keyed on `(series_id, provider)`, so catalogue duplicates legitimately
//! share an external id while the remote keeps a single entry for all of them. Any value settled
//! against that entry — a targeted push after a chapter is marked read, or a reconciliation's
//! merge — is therefore settled for every member, and this module is what makes that true of the
//! local rows as well.
//!
//! Split the same way the rest of the engine is: [`plan_group`] and [`plan_mirror`] decide from
//! values alone, [`LinkedSeries`] performs.

use time::OffsetDateTime;

use tankovault_db::PgPool;
use tankovault_db::repo::{sync, tracking};
use tankovault_domain::{SeriesId, UserId, WatchStatus};

use super::plan::LocalSide;

/// One member of a linked group, with the local state one user holds for it.
#[derive(Debug, Clone, Copy)]
pub(crate) struct LinkedMember {
    pub(crate) series_id: SeriesId,
    /// Whole-chapter frontier and when it last changed; `None` when there is no progress row.
    pub(crate) progress: Option<(f64, OffsetDateTime)>,
    /// Watchlist status; `None` when the series is not on the user's watchlist.
    pub(crate) status: Option<WatchStatus>,
    /// Excluded from syncing with this provider.
    pub(crate) excluded: bool,
}

impl LinkedMember {
    /// Whether the user holds anything at all for this series. A duplicate they never added is
    /// not out of step with the remote — it is simply not theirs — so the mirror leaves it
    /// alone rather than inventing a shelf entry for it.
    const fn tracked(&self) -> bool {
        self.progress.is_some() || self.status.is_some()
    }
}

/// The group folded into one local side, plus the member that stands for it.
#[derive(Debug, Clone, Copy)]
pub(crate) struct GroupSide {
    /// The member the merge's local writes and journal entries are recorded against.
    pub(crate) primary: SeriesId,
    pub(crate) side: LocalSide,
}

/// Fold a linked group into the single local side the merge compares against the remote.
///
/// Progress is the **highest** frontier any included member holds, not `preferred`'s. The remote
/// keeps one number for the whole group, so mirroring a lower member's value back over a higher
/// one would un-read chapters the reader had marked; taking the maximum is what makes the
/// write-back safe. The timestamp travels with that value so `NewestWins` still compares the
/// side it actually chose.
///
/// A member excluded from syncing contributes nothing and is never written; a group whose
/// members are all excluded is skipped whole, which is the same answer the single-series path
/// gave before groups existed.
#[must_use]
pub(crate) fn plan_group(members: &[LinkedMember], preferred: SeriesId) -> GroupSide {
    let included: Vec<&LinkedMember> = members.iter().filter(|m| !m.excluded).collect();
    let Some(first) = included.first() else {
        return GroupSide {
            primary: preferred,
            side: LocalSide {
                progress: 0.0,
                updated_at: OffsetDateTime::UNIX_EPOCH,
                status: None,
                excluded: true,
            },
        };
    };
    let primary = if included.iter().any(|m| m.series_id == preferred) {
        preferred
    } else {
        first.series_id
    };
    let frontier = included
        .iter()
        .filter_map(|m| m.progress)
        .max_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
    // Statuses are unordered, so there is no equivalent of the maximum: the member the caller
    // arrived at speaks for the group, and any other tracked member only stands in for it.
    let status = included
        .iter()
        .find(|m| m.series_id == primary)
        .and_then(|m| m.status)
        .or_else(|| included.iter().find_map(|m| m.status));
    GroupSide {
        primary,
        side: LocalSide {
            progress: frontier.map_or(0.0, |(p, _)| p),
            updated_at: frontier.map_or(OffsetDateTime::UNIX_EPOCH, |(_, u)| u),
            status,
            excluded: false,
        },
    }
}

/// One linked member's write-back. A member already holding the settled values still yields a
/// `MirrorWrite` with both fields `None`, because its common-ancestor snapshot is refreshed
/// either way — that snapshot is what stops the next run re-deciding the member from an
/// ancestor the group has moved past.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct MirrorWrite {
    pub(crate) series_id: SeriesId,
    /// Set when the member's frontier differs from the settled value.
    pub(crate) progress: Option<f64>,
    /// Set when the member is on the watchlist at a different status. Never set for a member
    /// that is not on the watchlist: a mirror keeps entries in step, it does not create them.
    pub(crate) status: Option<WatchStatus>,
}

/// What every member of the group must be written to, given the state it and the remote have
/// just settled on.
///
/// The series the merge drove is included, not exempt: the settled progress can come from a
/// *different* member (see [`plan_group`]), in which case the driver's own row is the one left
/// behind. Callers hold the pre-run values, so re-writing a row the merge already wrote is
/// idempotent — what must not be repeated is the journal entry, which the merge owns.
#[expect(
    clippy::float_cmp,
    reason = "both frontiers are whole-chapter numbers read back from the same column, so the \
              question really is whether the member already holds this exact value"
)]
#[must_use]
pub(crate) fn plan_mirror(
    members: &[LinkedMember],
    progress: f64,
    status: WatchStatus,
) -> Vec<MirrorWrite> {
    members
        .iter()
        .filter(|m| !m.excluded && m.tracked())
        .map(|m| MirrorWrite {
            series_id: m.series_id,
            progress: match m.progress {
                Some((held, _)) => (held != progress).then_some(progress),
                // No row already means "nothing read", which is all a settled zero would
                // record; writing it would manufacture a row that says the same thing.
                None => (progress > 0.0).then_some(progress),
            },
            status: m.status.filter(|s| *s != status).map(|_| status),
        })
        .collect()
}

/// Reads and writes the linked group around one external id.
pub(crate) struct LinkedSeries {
    pool: PgPool,
}

impl LinkedSeries {
    pub(crate) const fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// The group mapped to `external_id`, with `user_id`'s local state for each member.
    ///
    /// Queries per member rather than in one pass, because the callers that need it handle a
    /// single series and a group is one row wide in all but the duplicate case this exists for.
    /// The whole-account reconciliation builds its members from the state it already loaded.
    ///
    /// # Errors
    /// Database failures.
    pub(crate) async fn members(
        &self,
        user_id: UserId,
        provider: &str,
        external_id: &str,
    ) -> anyhow::Result<Vec<LinkedMember>> {
        let ids = sync::mapping_linked_series(&self.pool, provider, external_id).await?;
        let mut members = Vec::with_capacity(ids.len());
        for series_id in ids {
            members.push(LinkedMember {
                series_id,
                progress: tracking::progress_state(&self.pool, user_id, series_id).await?,
                status: tracking::watchlist_status_get(&self.pool, user_id, series_id).await?,
                excluded: tracking::is_sync_excluded(&self.pool, user_id, series_id, provider)
                    .await?,
            });
        }
        Ok(members)
    }

    /// Apply the planned write-backs and refresh each written member's common-ancestor snapshot
    /// to the state the group settled on.
    ///
    /// # Errors
    /// Database failures.
    pub(crate) async fn apply(
        &self,
        user_id: UserId,
        provider: &str,
        writes: &[MirrorWrite],
        agreed: (f64, WatchStatus),
    ) -> anyhow::Result<()> {
        let (progress, status) = agreed;
        for write in writes {
            if let Some(progress) = write.progress {
                tracking::progress_set(&self.pool, user_id, write.series_id, progress).await?;
            }
            if let Some(status) = write.status {
                tracking::watchlist_set_status(&self.pool, user_id, write.series_id, status)
                    .await?;
            }
            sync::record_snapshot(
                &self.pool,
                &sync::AgreedSnapshot {
                    series_id: write.series_id,
                    provider,
                    local_progress: progress,
                    remote_progress: progress,
                    local_status: status.as_str(),
                    remote_status: status.as_str(),
                },
            )
            .await?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    // Progress values under test are small, exactly-representable integers.
    #![expect(
        clippy::float_cmp,
        reason = "the group folds by exact equality of whole-chapter frontiers"
    )]

    use super::{LinkedMember, MirrorWrite, plan_group, plan_mirror};
    use tankovault_domain::{SeriesId, WatchStatus};
    use time::OffsetDateTime;
    use uuid::Uuid;

    fn id(n: u8) -> SeriesId {
        SeriesId::from_uuid(Uuid::from_bytes([n; 16]))
    }

    fn member(
        series_id: SeriesId,
        progress: Option<(f64, i64)>,
        status: Option<WatchStatus>,
    ) -> LinkedMember {
        LinkedMember {
            series_id,
            progress: progress.map(|(p, at)| (p, OffsetDateTime::from_unix_timestamp(at).unwrap())),
            status,
            excluded: false,
        }
    }

    /// Pins the write-back that made mirroring safe to add at all: the group's local side is the
    /// highest frontier any member holds, so settling on it can never un-read chapters recorded
    /// against a member the remote entry did not resolve to.
    #[test]
    fn the_group_speaks_with_its_highest_frontier() {
        let members = [
            member(id(1), Some((100.0, 900)), Some(WatchStatus::Reading)),
            member(id(2), Some((120.0, 100)), Some(WatchStatus::Reading)),
        ];

        let group = plan_group(&members, id(1));

        assert_eq!(group.primary, id(1), "the caller's series still drives");
        assert_eq!(group.side.progress, 120.0);
        assert_eq!(
            group.side.updated_at.unix_timestamp(),
            100,
            "the timestamp must travel with the value NewestWins would act on"
        );
    }

    #[test]
    fn an_excluded_member_contributes_nothing_and_a_wholly_excluded_group_is_skipped() {
        let mut excluded = member(id(2), Some((120.0, 100)), Some(WatchStatus::Reading));
        excluded.excluded = true;
        let included = member(id(1), Some((100.0, 900)), Some(WatchStatus::Reading));

        let group = plan_group(&[included, excluded], id(1));
        assert!(!group.side.excluded);
        assert_eq!(group.side.progress, 100.0, "the excluded 120 is not read");
        assert_eq!(
            plan_mirror(&[included, excluded], 100.0, WatchStatus::Reading)
                .iter()
                .map(|w| w.series_id)
                .collect::<Vec<_>>(),
            vec![id(1)],
            "and it is not written either"
        );

        let mut all_excluded = included;
        all_excluded.excluded = true;
        let group = plan_group(&[all_excluded, excluded], id(1));
        assert!(group.side.excluded);
        assert_eq!(group.primary, id(1));
    }

    /// The member the resolver arrived at may itself be excluded while a sibling is not. Driving
    /// the group from the sibling is the only reading that honours both flags.
    #[test]
    fn an_excluded_preferred_member_hands_the_group_to_an_included_one() {
        let mut preferred = member(id(1), Some((100.0, 900)), Some(WatchStatus::Paused));
        preferred.excluded = true;
        let sibling = member(id(2), Some((40.0, 100)), Some(WatchStatus::Reading));

        let group = plan_group(&[preferred, sibling], id(1));

        assert_eq!(group.primary, id(2));
        assert_eq!(group.side.progress, 40.0);
        assert_eq!(group.side.status, Some(WatchStatus::Reading));
    }

    #[test]
    fn a_member_the_reader_never_added_is_left_alone() {
        let untracked = member(id(2), None, None);
        let driving = member(id(1), Some((100.0, 900)), Some(WatchStatus::Reading));

        let writes = plan_mirror(&[driving, untracked], 100.0, WatchStatus::Reading);

        assert!(
            writes.iter().all(|w| w.series_id != id(2)),
            "mirroring keeps entries in step; it does not create them"
        );
    }

    /// The settled progress can come from a member the merge did not drive, which leaves the
    /// driver's own row behind — the group would be pushed to 120 while the series the push was
    /// recorded against still read 100.
    #[test]
    fn the_driving_member_is_written_too_when_a_sibling_supplied_the_value() {
        let members = [
            member(id(1), Some((100.0, 900)), Some(WatchStatus::Reading)),
            member(id(2), Some((120.0, 100)), Some(WatchStatus::Reading)),
        ];

        let writes = plan_mirror(&members, 120.0, WatchStatus::Reading);

        assert_eq!(
            writes,
            vec![
                MirrorWrite {
                    series_id: id(1),
                    progress: Some(120.0),
                    status: None,
                },
                MirrorWrite {
                    series_id: id(2),
                    progress: None,
                    status: None,
                },
            ]
        );
    }

    /// The bug this whole module exists for: a second local series mapped to the same remote
    /// entry kept its stale frontier forever, because the remote-driven pass reconciled one
    /// member and the local-driven pass skipped the rest as already handled.
    #[test]
    fn every_other_tracked_member_adopts_the_settled_state() {
        let members = [
            member(id(1), Some((100.0, 900)), Some(WatchStatus::Reading)),
            member(id(2), Some((40.0, 100)), Some(WatchStatus::Paused)),
            // On the watchlist but never read: gains a progress row at the settled value.
            member(id(3), None, Some(WatchStatus::Reading)),
            // Read but not on the watchlist: progress only, no entry conjured for it.
            member(id(4), Some((100.0, 100)), None),
        ];

        let writes = plan_mirror(&members, 100.0, WatchStatus::Reading);

        assert_eq!(
            writes,
            vec![
                MirrorWrite {
                    series_id: id(1),
                    progress: None,
                    status: None,
                },
                MirrorWrite {
                    series_id: id(2),
                    progress: Some(100.0),
                    status: Some(WatchStatus::Reading),
                },
                MirrorWrite {
                    series_id: id(3),
                    progress: Some(100.0),
                    status: None,
                },
                MirrorWrite {
                    series_id: id(4),
                    progress: None,
                    status: None,
                },
            ]
        );
    }
}
