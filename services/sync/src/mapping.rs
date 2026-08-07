//! Pure mapping and reconciliation logic for `AniList` sync, kept free of I/O so status
//! translation and the conflict policy are exhaustively unit-tested.

use tankovault_domain::{ContentType, SeriesStatus, WatchStatus};

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

/// `AniList` `countryOfOrigin`/`format` → our [`ContentType`].
///
/// Country is the primary signal, since `AniList` models manga/manhwa/manhua as one `MANGA`
/// format distinguished only by origin. `format` is the fallback for countries this catalogue
/// doesn't model (a US-published OEL comic, a French *manfra*). `NOVEL` is deliberately excluded
/// from the fallback: a light novel isn't a content type this catalogue models, and calling it
/// `Manga` would be worse than `Unknown`.
#[must_use]
pub(crate) fn content_type_from_origin(country: Option<&str>, format: Option<&str>) -> ContentType {
    match country {
        Some("JP") => ContentType::Manga,
        Some("KR") => ContentType::Manhwa,
        Some("CN" | "TW" | "HK") => ContentType::Manhua,
        _ => match format {
            Some("MANGA" | "ONE_SHOT") => ContentType::Manga,
            _ => ContentType::Unknown,
        },
    }
}

/// `AniList` `MediaStatus` → our [`SeriesStatus`].
///
/// This is the work's *publication* status, not the reader's `MediaListStatus` ([`AniListStatus`]
/// covers that). `NOT_YET_RELEASED` has no local counterpart and maps to `Unknown` — inventing a
/// fifth publication state for it would ripple through the Postgres enum, the API contract and
/// every locale file.
#[must_use]
pub(crate) fn series_status_from_media(status: Option<&str>) -> SeriesStatus {
    match status {
        Some("RELEASING") => SeriesStatus::Ongoing,
        Some("FINISHED") => SeriesStatus::Completed,
        Some("HIATUS") => SeriesStatus::Hiatus,
        Some("CANCELLED") => SeriesStatus::Cancelled,
        _ => SeriesStatus::Unknown,
    }
}

/// Round a fractional local progress to the whole-chapter count a provider expects. Lives here,
/// not the provider module, since remote trackers all count whole chapters, not just `AniList`.
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

/// The user-selectable reconciliation policy when a series exists on both sides.
///
/// Re-exported, not defined here: it used to be a private enum plus a bare wire `String` with a
/// `_ => NewestWins` fallback parser, turning a misspelled token into a silent policy change.
/// Now declared once in [`tankovault_contracts::sync::ConflictPolicy`].
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
    /// Stable slug naming the situation that produced the action, journalled on the decision.
    ///
    /// The action alone is not an explanation: a `PullRemote` because only the remote moved and a
    /// `PullRemote` because both moved and the policy said so are the same write and completely
    /// different events — the second is a value the reader lost, and it is the one they will ask
    /// about. Stable because it is persisted and rendered.
    pub(crate) reason: &'static str,
}

/// Three-way merge for one field of one series.
///
/// `last_*` is the value each side held at the last successful reconciliation; `None` means no
/// snapshot yet, in which case equal values converge silently and unequal values are a real
/// conflict resolved by `policy`. `newer` names the side more recently touched, consulted only
/// by `NewestWins`.
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
                reason: "no_ancestor_agreement",
            };
        }
        return resolve_conflict(policy, newer, true);
    }

    match (local_changed, remote_changed) {
        (false, false) => MergeDecision {
            action: MergeAction::Noop,
            winner: Side::Local,
            reason: "neither_side_changed",
        },
        (true, false) => MergeDecision {
            action: MergeAction::PushLocal,
            winner: Side::Local,
            reason: "only_local_changed",
        },
        (false, true) => MergeDecision {
            action: MergeAction::PullRemote,
            winner: Side::Remote,
            reason: "only_remote_changed",
        },
        (true, true) => {
            if current_local == current_remote {
                // Both moved to the same value: converged, just refresh the snapshot.
                MergeDecision {
                    action: MergeAction::Noop,
                    winner: Side::Local,
                    reason: "both_sides_converged",
                }
            } else {
                resolve_conflict(policy, newer, false)
            }
        }
    }
}

/// Resolve a genuine conflict (both sides changed to different values, or first-sync
/// disagreement) under `policy`.
///
/// `first_sync` distinguishes the two situations that reach here, and travels into the reason
/// slug: the same policy applied to a first sync and to a genuine divergence are different
/// events, and only the second means a value the reader had was overwritten.
///
/// The slugs are spelled out rather than composed, because they are persisted: a `format!` here
/// would either allocate per field per series or leak, and the set is small enough to read.
#[must_use]
fn resolve_conflict(policy: ConflictPolicy, newer: Side, first_sync: bool) -> MergeDecision {
    let (action, winner, reason) = match (policy, newer, first_sync) {
        (ConflictPolicy::LocalWins, _, true) => (
            MergeAction::PushLocal,
            Side::Local,
            "no_ancestor_policy_local_wins",
        ),
        (ConflictPolicy::LocalWins, _, false) => (
            MergeAction::PushLocal,
            Side::Local,
            "both_sides_changed_policy_local_wins",
        ),
        (ConflictPolicy::RemoteWins, _, true) => (
            MergeAction::PullRemote,
            Side::Remote,
            "no_ancestor_policy_remote_wins",
        ),
        (ConflictPolicy::RemoteWins, _, false) => (
            MergeAction::PullRemote,
            Side::Remote,
            "both_sides_changed_policy_remote_wins",
        ),
        (ConflictPolicy::NewestWins, Side::Local, true) => (
            MergeAction::PushLocal,
            Side::Local,
            "no_ancestor_newest_is_local",
        ),
        (ConflictPolicy::NewestWins, Side::Local, false) => (
            MergeAction::PushLocal,
            Side::Local,
            "both_sides_changed_newest_is_local",
        ),
        (ConflictPolicy::NewestWins, Side::Remote, true) => (
            MergeAction::PullRemote,
            Side::Remote,
            "no_ancestor_newest_is_remote",
        ),
        (ConflictPolicy::NewestWins, Side::Remote, false) => (
            MergeAction::PullRemote,
            Side::Remote,
            "both_sides_changed_newest_is_remote",
        ),
        (ConflictPolicy::AskMe, _, true) => (
            MergeAction::Conflict,
            Side::Local,
            "no_ancestor_queued_for_the_reader",
        ),
        (ConflictPolicy::AskMe, _, false) => (
            MergeAction::Conflict,
            Side::Local,
            "both_sides_changed_queued_for_the_reader",
        ),
    };
    MergeDecision {
        action,
        winner,
        reason,
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
        let ct = |c| content_type_from_origin(c, None);
        assert_eq!(ct(Some("JP")), ContentType::Manga);
        assert_eq!(ct(Some("KR")), ContentType::Manhwa);
        assert_eq!(ct(Some("CN")), ContentType::Manhua);
        assert_eq!(ct(None), ContentType::Unknown);
        assert_eq!(ct(Some("US")), ContentType::Unknown);
    }

    /// A country we do not model used to erase a content type `AniList` had stated: an OEL
    /// comic is `countryOfOrigin: "US"`, `format: "MANGA"`, and landed on `Unknown`.
    #[test]
    fn format_fills_in_a_country_we_do_not_model() {
        assert_eq!(
            content_type_from_origin(Some("US"), Some("MANGA")),
            ContentType::Manga
        );
        assert_eq!(
            content_type_from_origin(None, Some("ONE_SHOT")),
            ContentType::Manga
        );
        // Country still wins where we have one: a Korean work is manhwa, not manga, however
        // AniList spells its format.
        assert_eq!(
            content_type_from_origin(Some("KR"), Some("MANGA")),
            ContentType::Manhwa
        );
        // A light novel is not a content type this catalogue models — better `Unknown` than
        // filed as manga.
        assert_eq!(
            content_type_from_origin(Some("US"), Some("NOVEL")),
            ContentType::Unknown
        );
    }

    #[test]
    fn media_status_maps_to_publication_status() {
        assert_eq!(
            series_status_from_media(Some("RELEASING")),
            SeriesStatus::Ongoing
        );
        assert_eq!(
            series_status_from_media(Some("FINISHED")),
            SeriesStatus::Completed
        );
        assert_eq!(
            series_status_from_media(Some("HIATUS")),
            SeriesStatus::Hiatus
        );
        assert_eq!(
            series_status_from_media(Some("CANCELLED")),
            SeriesStatus::Cancelled
        );
        // Both the unmodelled upstream state and an absent field are `Unknown`, so neither
        // can be written over a status a source adapter already determined.
        assert_eq!(
            series_status_from_media(Some("NOT_YET_RELEASED")),
            SeriesStatus::Unknown
        );
        assert_eq!(series_status_from_media(None), SeriesStatus::Unknown);
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

    /// Pins a mutation `cargo mutants` found alive: flipping `have_ancestor`'s `&&` to `||`
    /// survived the suite because every earlier test supplied both halves or neither. With only
    /// `last_local` or only `last_remote` set, `||` would read it as a full ancestor and turn an
    /// agreed value into a silent `PullRemote` rollback.
    #[test]
    fn one_half_of_a_snapshot_is_not_an_ancestor() {
        for (last_local, last_remote) in [(Some(5.0), None), (None, Some(5.0))] {
            let agreed = three_way(
                5.0,
                5.0,
                last_local,
                last_remote,
                ConflictPolicy::LocalWins,
                newer_local(),
            );
            assert_eq!(
                agreed.action,
                MergeAction::Noop,
                "{last_local:?}/{last_remote:?}"
            );

            // …and a disagreement is a real conflict decided by policy, not by which half of
            // the snapshot happens to be present.
            let disagreed = three_way(
                5.0,
                9.0,
                last_local,
                last_remote,
                ConflictPolicy::AskMe,
                newer_local(),
            );
            assert_eq!(
                disagreed.action,
                MergeAction::Conflict,
                "{last_local:?}/{last_remote:?}"
            );
        }
    }

    /// Pins three properties `cargo mutants` found untested (replacing the body with `0`, `1`,
    /// `-1` stayed green): rounds rather than truncates, clamps negative to zero, and the cast
    /// saturates so `f64::INFINITY` becomes `i64::MAX` rather than a negative count.
    #[test]
    fn progress_rounds_to_whole_chapters_and_never_goes_negative() {
        assert_eq!(progress_to_int(0.0), 0);
        assert_eq!(progress_to_int(152.0), 152);
        assert_eq!(progress_to_int(152.4), 152);
        assert_eq!(progress_to_int(152.5), 153);
        assert_eq!(progress_to_int(-3.0), 0);
        assert_eq!(progress_to_int(f64::INFINITY), i64::MAX);
    }

    // The policy round trip moved to `tankovault_contracts::sync::ConflictPolicy`'s own tests;
    // the persisted-token fallback is pinned by `engine::accounts::tests`.
}
