//! Pure mapping and reconciliation logic for `AniList` sync (design §15).
//!
//! Kept free of I/O so the tricky parts — status translation and the user-selectable
//! conflict policy — are exhaustively unit-tested. The engine layer wires these to the
//! database and the `AniList` GraphQL client.

use tankovault_domain::{ContentType, WatchStatus};

/// `AniList` `MediaListStatus` enum values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AniListStatus {
    Current,
    Planning,
    Completed,
    Dropped,
    Paused,
    Repeating,
}

impl AniListStatus {
    /// The GraphQL enum token sent in mutations.
    #[must_use]
    pub(crate) fn as_graphql(self) -> &'static str {
        match self {
            Self::Current => "CURRENT",
            Self::Planning => "PLANNING",
            Self::Completed => "COMPLETED",
            Self::Dropped => "DROPPED",
            Self::Paused => "PAUSED",
            Self::Repeating => "REPEATING",
        }
    }

    /// Parse a GraphQL `MediaListStatus` token.
    #[must_use]
    pub(crate) fn parse(token: &str) -> Option<Self> {
        match token {
            "CURRENT" => Some(Self::Current),
            "PLANNING" => Some(Self::Planning),
            "COMPLETED" => Some(Self::Completed),
            "DROPPED" => Some(Self::Dropped),
            "PAUSED" => Some(Self::Paused),
            "REPEATING" => Some(Self::Repeating),
            _ => None,
        }
    }

    /// Map an `AniList` status onto our local [`WatchStatus`]. `REPEATING` (a re-read) maps
    /// to `Reading`, its closest local equivalent.
    #[must_use]
    pub(crate) fn to_watch_status(self) -> WatchStatus {
        match self {
            Self::Current | Self::Repeating => WatchStatus::Reading,
            Self::Planning => WatchStatus::Planned,
            Self::Completed => WatchStatus::Completed,
            Self::Dropped => WatchStatus::Dropped,
            Self::Paused => WatchStatus::Paused,
        }
    }

    /// Map a local [`WatchStatus`] onto the `AniList` status pushed in a mutation.
    #[must_use]
    pub(crate) fn from_watch_status(status: WatchStatus) -> Self {
        match status {
            WatchStatus::Reading => Self::Current,
            WatchStatus::Planned => Self::Planning,
            WatchStatus::Completed => Self::Completed,
            WatchStatus::Dropped => Self::Dropped,
            WatchStatus::Paused => Self::Paused,
        }
    }
}

/// `AniList` `countryOfOrigin` → our [`ContentType`] (`AniList` models manga/manhwa/manhua as
/// one `MANGA` format distinguished only by country).
#[must_use]
pub(crate) fn content_type_from_country(country: Option<&str>) -> ContentType {
    match country {
        Some("JP") => ContentType::Manga,
        Some("KR") => ContentType::Manhwa,
        Some("CN" | "TW" | "HK") => ContentType::Manhua,
        _ => ContentType::Unknown,
    }
}

/// Round a fractional local progress to the whole-chapter count a provider expects.
///
/// Lives here rather than in the provider module because this file owns every other unit
/// conversion across the boundary (ARCH-7), and the rounding rule is not `AniList`-specific:
/// remote trackers count whole chapters, local progress does not.
#[expect(
    clippy::cast_possible_truncation,
    reason = "the target is signed and `max(0.0)` rules out a negative, and Rust's \
              float-to-int cast saturates rather than wrapping, so the value is the \
              rounded progress or `i64::MAX`, never a wrapped one"
)]
#[must_use]
pub(crate) fn progress_to_int(progress: f64) -> i64 {
    progress.max(0.0).round() as i64
}

/// The user-selectable reconciliation policy when a series exists on both sides (§15).
///
/// Re-exported rather than defined here. This was a private enum plus a bare `String` on the
/// wire, so the vocabulary lived in three unconnected places — this service, the `OpenAPI` prose
/// and a closed enumeration the frontend maintained by hand — and both parsers had a
/// `_ => NewestWins` arm, which turned a misspelled token into a silent policy change rather
/// than an error. It is now declared once in [`tankovault_contracts::sync::ConflictPolicy`],
/// which is what the schema publishes and the generated client carries.
pub(crate) use tankovault_contracts::sync::ConflictPolicy;

/// Which side a reconciliation deemed authoritative.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Side {
    Local,
    Remote,
}

/// What the three-way merge decided should happen to one field of one mapped series.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MergeAction {
    /// Neither side changed since the last agreement; nothing to write.
    Noop,
    /// Push the local value to the remote.
    PushLocal,
    /// Pull the remote value into the local store.
    PullRemote,
    /// A genuine conflict under `AskMe`: touch neither side, queue for the user.
    Conflict,
}

/// The outcome of a single field's three-way merge.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct MergeDecision {
    pub(crate) action: MergeAction,
    /// The side deemed authoritative for a real conflict / directional write.
    pub(crate) winner: Side,
}

/// Three-way merge for one field of one series (design v2 §B.3).
///
/// `last_*` is the value that side held at the last successful reconciliation (the common
/// ancestor); `None` means "no snapshot yet" (first reconciliation), in which case there is no
/// memory of what changed, so equal values converge silently and unequal values are treated as
/// a real conflict resolved by `policy`. `newer` names the side whose *own* last-modified time
/// is later, consulted only by `NewestWins`.
#[must_use]
pub(crate) fn three_way<T: PartialEq + Copy>(
    current_local: T,
    current_remote: T,
    last_local: Option<T>,
    last_remote: Option<T>,
    policy: ConflictPolicy,
    newer: Side,
) -> MergeDecision {
    let local_changed = last_local != Some(current_local);
    let remote_changed = last_remote != Some(current_remote);
    let have_ancestor = last_local.is_some() && last_remote.is_some();

    // With no common ancestor we cannot tell what changed: agreement is a no-op, disagreement
    // is a real conflict resolved by policy.
    if !have_ancestor {
        if current_local == current_remote {
            return MergeDecision {
                action: MergeAction::Noop,
                winner: Side::Local,
            };
        }
        return resolve_conflict(policy, newer);
    }

    match (local_changed, remote_changed) {
        (false, false) => MergeDecision {
            action: MergeAction::Noop,
            winner: Side::Local,
        },
        (true, false) => MergeDecision {
            action: MergeAction::PushLocal,
            winner: Side::Local,
        },
        (false, true) => MergeDecision {
            action: MergeAction::PullRemote,
            winner: Side::Remote,
        },
        (true, true) => {
            if current_local == current_remote {
                // Both moved to the same value: converged, just refresh the snapshot.
                MergeDecision {
                    action: MergeAction::Noop,
                    winner: Side::Local,
                }
            } else {
                resolve_conflict(policy, newer)
            }
        }
    }
}

/// Resolve a genuine conflict (both sides changed to different values, or first-sync
/// disagreement) under `policy`.
#[must_use]
fn resolve_conflict(policy: ConflictPolicy, newer: Side) -> MergeDecision {
    match policy {
        ConflictPolicy::LocalWins => MergeDecision {
            action: MergeAction::PushLocal,
            winner: Side::Local,
        },
        ConflictPolicy::RemoteWins => MergeDecision {
            action: MergeAction::PullRemote,
            winner: Side::Remote,
        },
        ConflictPolicy::NewestWins => match newer {
            Side::Local => MergeDecision {
                action: MergeAction::PushLocal,
                winner: Side::Local,
            },
            Side::Remote => MergeDecision {
                action: MergeAction::PullRemote,
                winner: Side::Remote,
            },
        },
        ConflictPolicy::AskMe => MergeDecision {
            action: MergeAction::Conflict,
            winner: Side::Local,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn newer_local() -> Side {
        Side::Local
    }
    fn newer_remote() -> Side {
        Side::Remote
    }

    #[test]
    fn status_round_trips_through_local() {
        for status in WatchStatus::all() {
            let there_and_back = AniListStatus::from_watch_status(*status).to_watch_status();
            assert_eq!(there_and_back, *status);
        }
    }

    #[test]
    fn repeating_maps_to_reading() {
        assert_eq!(
            AniListStatus::Repeating.to_watch_status(),
            WatchStatus::Reading
        );
    }

    #[test]
    fn graphql_tokens_parse_back() {
        for s in [
            AniListStatus::Current,
            AniListStatus::Planning,
            AniListStatus::Completed,
            AniListStatus::Dropped,
            AniListStatus::Paused,
            AniListStatus::Repeating,
        ] {
            assert_eq!(AniListStatus::parse(s.as_graphql()), Some(s));
        }
        assert_eq!(AniListStatus::parse("NONSENSE"), None);
    }

    #[test]
    fn country_maps_to_content_type() {
        assert_eq!(content_type_from_country(Some("JP")), ContentType::Manga);
        assert_eq!(content_type_from_country(Some("KR")), ContentType::Manhwa);
        assert_eq!(content_type_from_country(Some("CN")), ContentType::Manhua);
        assert_eq!(content_type_from_country(None), ContentType::Unknown);
        assert_eq!(content_type_from_country(Some("US")), ContentType::Unknown);
    }

    #[test]
    fn no_change_since_ancestor_is_noop() {
        let d = three_way(
            5.0,
            5.0,
            Some(5.0),
            Some(5.0),
            ConflictPolicy::NewestWins,
            newer_local(),
        );
        assert_eq!(d.action, MergeAction::Noop);
    }

    #[test]
    fn only_local_changed_pushes() {
        let d = three_way(
            7.0,
            5.0,
            Some(5.0),
            Some(5.0),
            ConflictPolicy::NewestWins,
            newer_local(),
        );
        assert_eq!(d.action, MergeAction::PushLocal);
        assert_eq!(d.winner, Side::Local);
    }

    #[test]
    fn only_remote_changed_pulls() {
        // Only remote changed: not a conflict, so policy is irrelevant — pull it.
        let d = three_way(
            5.0,
            9.0,
            Some(5.0),
            Some(5.0),
            ConflictPolicy::LocalWins,
            newer_local(),
        );
        assert_eq!(d.action, MergeAction::PullRemote);
        assert_eq!(d.winner, Side::Remote);
    }

    #[test]
    fn both_changed_to_same_value_converges() {
        let d = three_way(
            8.0,
            8.0,
            Some(5.0),
            Some(6.0),
            ConflictPolicy::AskMe,
            newer_local(),
        );
        assert_eq!(d.action, MergeAction::Noop);
    }

    #[test]
    fn real_conflict_local_wins() {
        let d = three_way(
            7.0,
            9.0,
            Some(5.0),
            Some(5.0),
            ConflictPolicy::LocalWins,
            newer_remote(),
        );
        assert_eq!(d.action, MergeAction::PushLocal);
    }

    #[test]
    fn real_conflict_remote_wins() {
        let d = three_way(
            7.0,
            9.0,
            Some(5.0),
            Some(5.0),
            ConflictPolicy::RemoteWins,
            newer_local(),
        );
        assert_eq!(d.action, MergeAction::PullRemote);
    }

    #[test]
    fn real_conflict_newest_wins_follows_newer_side() {
        let d = three_way(
            7.0,
            9.0,
            Some(5.0),
            Some(5.0),
            ConflictPolicy::NewestWins,
            newer_remote(),
        );
        assert_eq!(d.action, MergeAction::PullRemote);
        let d = three_way(
            7.0,
            9.0,
            Some(5.0),
            Some(5.0),
            ConflictPolicy::NewestWins,
            newer_local(),
        );
        assert_eq!(d.action, MergeAction::PushLocal);
    }

    #[test]
    fn real_conflict_ask_me_queues() {
        let d = three_way(
            7.0,
            9.0,
            Some(5.0),
            Some(5.0),
            ConflictPolicy::AskMe,
            newer_local(),
        );
        assert_eq!(d.action, MergeAction::Conflict);
    }

    #[test]
    fn no_ancestor_agreement_is_noop() {
        let d = three_way(4.0, 4.0, None, None, ConflictPolicy::AskMe, newer_local());
        assert_eq!(d.action, MergeAction::Noop);
    }

    #[test]
    fn no_ancestor_disagreement_is_conflict_under_ask_me() {
        let d = three_way(4.0, 8.0, None, None, ConflictPolicy::AskMe, newer_local());
        assert_eq!(d.action, MergeAction::Conflict);
    }

    // The policy token round trip used to be pinned here, against this module's own copy of
    // the enum. Both moved: the vocabulary is `tankovault_contracts::sync::ConflictPolicy` and
    // its round trip is pinned there, while the tolerance for an unreadable *persisted* token
    // — the part that was this service's own judgement, not the wire's — is pinned by
    // `engine::accounts::tests`.
}
