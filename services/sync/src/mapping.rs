//! Pure mapping and reconciliation logic for `AniList` sync (design §15).
//!
//! Kept free of I/O so the tricky parts — status translation and the user-selectable
//! conflict policy — are exhaustively unit-tested. The engine layer wires these to the
//! database and the `AniList` GraphQL client.

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
/// Country is the primary signal: `AniList` models manga, manhwa and manhua as one `MANGA`
/// format distinguished only by where the work was published.
///
/// `format` is the fallback for the countries this catalogue does not model — an OEL comic
/// published in the US, a French *manfra*. Those used to land on `Unknown` even though `AniList`
/// had said plainly that the work is manga-format, which is one of the ways a series that
/// `AniList` knew perfectly well still displayed as "Unknown" locally. `NOVEL` is deliberately
/// absent from the fallback: a light novel is not a content type this catalogue models, and
/// calling it `Manga` would be worse than admitting we do not know.
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
/// This is the *publication* status of the work, not the reader's own `MediaListStatus` — two
/// different `AniList` enums, and only this one belongs on a catalogue row. [`AniListStatus`]
/// above covers the other.
///
/// `NOT_YET_RELEASED` has no local counterpart and maps to `Unknown`: the catalogue models four
/// publication states, and inventing a fifth to carry one upstream token would ripple through
/// the Postgres enum, the API contract and every locale file for a state no source adapter can
/// produce.
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

    /// **Half** a snapshot is not a common ancestor.
    ///
    /// The four snapshot fields are independent `Option`s, so a row can carry the local value
    /// and not the remote one — a partially written snapshot, or one from before a field was
    /// added. Treating that as an ancestor makes the missing side look *changed*, so the
    /// absent half wins: with `last_remote` empty and both sides sitting on the same value,
    /// this returns `Noop`, while reading `have_ancestor` as "either side has one" turns it
    /// into a `PullRemote` that overwrites the user's progress with the value it already had —
    /// invisible when the values agree, and a silent rollback the moment they do not.
    ///
    /// `cargo mutants` found this: flipping the `&&` in `have_ancestor` to `||` survived the
    /// whole suite, because every earlier test supplied either both halves or neither.
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

    /// Local progress is fractional (a part release is `152.5`); every remote tracker counts
    /// whole chapters. The rounding rule is the whole of this function and nothing asserted on
    /// it — `cargo mutants` replaced the body with `0`, `1` and `-1` in turn and the suite
    /// stayed green, which for a *push* means writing the wrong chapter count to somebody's
    /// public list.
    ///
    /// Three properties, each with a way to get it wrong: it **rounds** rather than truncating,
    /// so a reader on `152.5` is pushed as having finished 153 rather than 152; it clamps at
    /// zero, because a negative chapter count is a value no tracker accepts; and the cast
    /// saturates rather than wrapping, so the `f64::INFINITY` that `parse_number` used to be
    /// able to produce (F-01b) becomes `i64::MAX` instead of a negative count.
    #[test]
    fn progress_rounds_to_whole_chapters_and_never_goes_negative() {
        assert_eq!(progress_to_int(0.0), 0);
        assert_eq!(progress_to_int(152.0), 152);
        assert_eq!(progress_to_int(152.4), 152);
        assert_eq!(progress_to_int(152.5), 153);
        assert_eq!(progress_to_int(-3.0), 0);
        assert_eq!(progress_to_int(f64::INFINITY), i64::MAX);
    }

    // The policy token round trip used to be pinned here, against this module's own copy of
    // the enum. Both moved: the vocabulary is `tankovault_contracts::sync::ConflictPolicy` and
    // its round trip is pinned there, while the tolerance for an unreadable *persisted* token
    // — the part that was this service's own judgement, not the wire's — is pinned by
    // `engine::accounts::tests`.
}
