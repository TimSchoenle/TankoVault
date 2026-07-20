//! Pure mapping and reconciliation logic for `AniList` sync (design §15).
//!
//! Kept free of I/O so the tricky parts — status translation and the user-selectable
//! conflict policy — are exhaustively unit-tested. The engine layer wires these to the
//! database and the `AniList` GraphQL client.

use tankovault_domain::{ContentType, WatchStatus};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

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

/// The user-selectable reconciliation policy when a series exists on both sides (§15).
/// The shared `Wins` suffix is intentional, user-facing vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
#[allow(clippy::enum_variant_names)]
pub(crate) enum ConflictPolicy {
    /// Local progress/status is authoritative.
    LocalWins,
    /// The remote (`AniList`) value is authoritative.
    RemoteWins,
    /// Whichever side was updated most recently wins.
    #[default]
    NewestWins,
}

/// One side's progress plus when it last changed, used for `NewestWins`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct ProgressState {
    pub(crate) progress: f64,
    pub(crate) updated_at: OffsetDateTime,
}

/// Which side a reconciliation deemed authoritative.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Side {
    Local,
    Remote,
}

/// The result of reconciling a single series across the two sides. The engine applies
/// `update_local` on a pull and `update_remote` on a push.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Reconciliation {
    /// The progress value both sides should converge to.
    pub(crate) agreed_progress: f64,
    /// The authoritative side (drives which side's *status* is adopted on pull).
    pub(crate) winner: Side,
    /// The local store should be written to `agreed_progress`.
    pub(crate) update_local: bool,
    /// The remote (`AniList`) entry should be written to `agreed_progress`.
    pub(crate) update_remote: bool,
}

/// Reconcile a series' progress across local and remote states under `policy`.
///
/// At least one side must be present (the engine only reconciles series that exist
/// somewhere). When only one side is present, the other is brought into agreement with it.
#[must_use]
pub(crate) fn reconcile_progress(
    local: Option<ProgressState>,
    remote: Option<ProgressState>,
    policy: ConflictPolicy,
) -> Reconciliation {
    match (local, remote) {
        (Some(l), None) => Reconciliation {
            agreed_progress: l.progress,
            winner: Side::Local,
            update_local: false,
            update_remote: true,
        },
        (None, Some(r)) => Reconciliation {
            agreed_progress: r.progress,
            winner: Side::Remote,
            update_local: true,
            update_remote: false,
        },
        (None, None) => Reconciliation {
            agreed_progress: 0.0,
            winner: Side::Local,
            update_local: false,
            update_remote: false,
        },
        (Some(l), Some(r)) => {
            let winner = match policy {
                ConflictPolicy::LocalWins => Side::Local,
                ConflictPolicy::RemoteWins => Side::Remote,
                ConflictPolicy::NewestWins => {
                    if r.updated_at > l.updated_at {
                        Side::Remote
                    } else if l.updated_at > r.updated_at {
                        Side::Local
                    } else if r.progress > l.progress {
                        // Same timestamp: prefer the further-along side (reading is monotonic).
                        Side::Remote
                    } else {
                        Side::Local
                    }
                }
            };
            let agreed = match winner {
                Side::Local => l.progress,
                Side::Remote => r.progress,
            };
            Reconciliation {
                agreed_progress: agreed,
                winner,
                // Only write a side that actually differs from the agreed value.
                update_local: (l.progress - agreed).abs() > f64::EPSILON,
                update_remote: (r.progress - agreed).abs() > f64::EPSILON,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    // Reconciliation returns exactly one side's stored progress unchanged, so exact
    // float comparison is correct here.
    #![allow(clippy::float_cmp)]

    use super::*;

    fn ts(secs: i64) -> OffsetDateTime {
        OffsetDateTime::from_unix_timestamp(secs).unwrap()
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
    fn only_local_pushes_to_remote() {
        let r = reconcile_progress(
            Some(ProgressState {
                progress: 12.0,
                updated_at: ts(100),
            }),
            None,
            ConflictPolicy::NewestWins,
        );
        assert_eq!(r.agreed_progress, 12.0);
        assert!(r.update_remote && !r.update_local);
        assert_eq!(r.winner, Side::Local);
    }

    #[test]
    fn only_remote_pulls_to_local() {
        let r = reconcile_progress(
            None,
            Some(ProgressState {
                progress: 5.0,
                updated_at: ts(100),
            }),
            ConflictPolicy::LocalWins,
        );
        assert_eq!(r.agreed_progress, 5.0);
        assert!(r.update_local && !r.update_remote);
        assert_eq!(r.winner, Side::Remote);
    }

    #[test]
    fn local_wins_overrides_newer_remote() {
        let r = reconcile_progress(
            Some(ProgressState {
                progress: 3.0,
                updated_at: ts(100),
            }),
            Some(ProgressState {
                progress: 9.0,
                updated_at: ts(200),
            }),
            ConflictPolicy::LocalWins,
        );
        assert_eq!(r.winner, Side::Local);
        assert_eq!(r.agreed_progress, 3.0);
        assert!(r.update_remote); // remote differs, must be corrected down
        assert!(!r.update_local);
    }

    #[test]
    fn remote_wins_overrides_newer_local() {
        let r = reconcile_progress(
            Some(ProgressState {
                progress: 9.0,
                updated_at: ts(200),
            }),
            Some(ProgressState {
                progress: 3.0,
                updated_at: ts(100),
            }),
            ConflictPolicy::RemoteWins,
        );
        assert_eq!(r.winner, Side::Remote);
        assert_eq!(r.agreed_progress, 3.0);
        assert!(r.update_local);
        assert!(!r.update_remote);
    }

    #[test]
    fn newest_wins_picks_the_later_timestamp() {
        let r = reconcile_progress(
            Some(ProgressState {
                progress: 3.0,
                updated_at: ts(100),
            }),
            Some(ProgressState {
                progress: 9.0,
                updated_at: ts(200),
            }),
            ConflictPolicy::NewestWins,
        );
        assert_eq!(r.winner, Side::Remote);
        assert_eq!(r.agreed_progress, 9.0);
        assert!(r.update_local && !r.update_remote);
    }

    #[test]
    fn equal_progress_needs_no_writes() {
        let r = reconcile_progress(
            Some(ProgressState {
                progress: 7.0,
                updated_at: ts(100),
            }),
            Some(ProgressState {
                progress: 7.0,
                updated_at: ts(300),
            }),
            ConflictPolicy::NewestWins,
        );
        assert!(!r.update_local && !r.update_remote);
    }

    #[test]
    fn newest_wins_tie_prefers_further_along() {
        let r = reconcile_progress(
            Some(ProgressState {
                progress: 4.0,
                updated_at: ts(100),
            }),
            Some(ProgressState {
                progress: 10.0,
                updated_at: ts(100),
            }),
            ConflictPolicy::NewestWins,
        );
        assert_eq!(r.winner, Side::Remote);
        assert_eq!(r.agreed_progress, 10.0);
    }
}
