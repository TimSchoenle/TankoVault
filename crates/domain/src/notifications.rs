//! The notification kind registry and the per-reader preference document.
//!
//! Neither type belongs in [`crate::enums`], whose contract is a 1:1 mirror of a `PostgreSQL`
//! enum: `notifications.kind` is plain `text` and `users.notification_prefs` is `jsonb`.

use crate::enums::{ParseEnumError, WatchStatus};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;
use time::{OffsetDateTime, UtcOffset};
use utoipa::ToSchema;

/// Minutes in a day; the exclusive ceiling on a [`QuietHours`] boundary.
const MINUTES_PER_DAY: u16 = 24 * 60;

/// Widest real UTC offset, in minutes (±18:00, the range `time::UtcOffset` accepts).
const MAX_OFFSET_MINUTES: i16 = 18 * 60;

/// What a notification is about.
///
/// Deliberately *not* a `PostgreSQL` enum. The read path has to keep rendering rows written by a
/// newer notifier than itself, so an unrecognised token has to survive the round trip as text
/// rather than fail a cast on the way out of the database.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    PartialOrd,
    Ord,
    Serialize,
    Deserialize,
    ToSchema,
    Default,
)]
#[serde(rename_all = "snake_case")]
pub enum NotificationKind {
    /// One or more new chapters of a watched series.
    #[default]
    NewChapter,
    /// A watched series reached `completed`.
    SeriesCompleted,
    /// A watched series gained another provider to read it on.
    SourceAdded,
    /// External sync needs a decision the engine will not take unattended.
    SyncConflict,
    /// An operator message to the whole instance.
    Announcement,
}

impl NotificationKind {
    /// The wire token, byte-identical to the `notifications.kind` value.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NewChapter => "new_chapter",
            Self::SeriesCompleted => "series_completed",
            Self::SourceAdded => "source_added",
            Self::SyncConflict => "sync_conflict",
            Self::Announcement => "announcement",
        }
    }

    /// Every variant, in declaration order.
    #[must_use]
    pub fn all() -> &'static [Self] {
        &[
            Self::NewChapter,
            Self::SeriesCompleted,
            Self::SourceAdded,
            Self::SyncConflict,
            Self::Announcement,
        ]
    }
}

impl fmt::Display for NotificationKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for NotificationKind {
    type Err = ParseEnumError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "new_chapter" => Ok(Self::NewChapter),
            "series_completed" => Ok(Self::SeriesCompleted),
            "source_added" => Ok(Self::SourceAdded),
            "sync_conflict" => Ok(Self::SyncConflict),
            "announcement" => Ok(Self::Announcement),
            other => Err(ParseEnumError {
                kind: "NotificationKind",
                value: other.to_owned(),
            }),
        }
    }
}

/// The preference-document version this build writes and understands.
///
/// Bumped when an existing field changes *meaning*; adding a field that defaults does not need it.
pub const PREFS_VERSION: u8 = 1;

/// Why a submitted preference document was refused.
#[derive(Debug, Clone, thiserror::Error)]
pub enum PrefsError {
    /// Written by a newer build. Storing it would let this one silently reinterpret the fields.
    #[error(
        "preference document version {found} is newer than this server understands ({PREFS_VERSION})"
    )]
    UnknownVersion { found: u8 },
    /// A quiet-hours boundary outside `0..1440`.
    #[error("minute-of-day {found} is out of range (0..{MINUTES_PER_DAY})")]
    MinuteOutOfRange { found: u16 },
    /// A UTC offset outside ±18:00.
    #[error("UTC offset {found} minutes is out of range (±{MAX_OFFSET_MINUTES})")]
    OffsetOutOfRange { found: i16 },
}

/// A reader's notification preferences, stored as `users.notification_prefs`.
///
/// Every field defaults, so an absent, partial or legacy document decodes to the product
/// defaults rather than an error: a preference read must never fail the request it decorates.
/// The pre-v1 document (`{"new_chapters": …, "email": …, "digest": …}`) therefore decodes to
/// full defaults — those keys were never read by anything, so nothing observable is lost.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct NotificationPrefs {
    /// Schema version of this document; see [`PREFS_VERSION`].
    pub version: u8,
    /// Which kinds of event are worth telling this reader about.
    pub kinds: KindPrefs,
    /// Which watchlist statuses are worth telling this reader about.
    pub watch_status: StatusPrefs,
    /// Where the notification goes once it has passed the two filters above.
    pub channels: ChannelPrefs,
    /// A nightly window in which the live push stays silent.
    pub quiet_hours: QuietHours,
    /// Collapse a series' chapters into one row while it stays unread, instead of one row each.
    pub group_unread: bool,
}

impl Default for NotificationPrefs {
    fn default() -> Self {
        Self {
            version: PREFS_VERSION,
            kinds: KindPrefs::default(),
            watch_status: StatusPrefs::default(),
            channels: ChannelPrefs::default(),
            quiet_hours: QuietHours::default(),
            group_unread: true,
        }
    }
}

impl NotificationPrefs {
    /// Whether `kind`, about a series the reader has in `status`, earns a durable notification.
    #[must_use]
    pub fn allows(&self, kind: NotificationKind, status: WatchStatus) -> bool {
        self.channels.in_app && self.kinds.allows(kind) && self.watch_status.allows(status)
    }

    /// Whether the best-effort live push should fire at `at`.
    ///
    /// Separate from [`Self::allows`] on purpose: quiet hours silence the *nudge*, never the
    /// durable row — waking up to an empty inbox because the notifier ran at 03:00 would be a
    /// data-loss bug wearing a preference's clothes.
    #[must_use]
    pub fn allows_live(&self, at: OffsetDateTime) -> bool {
        self.channels.live && !self.quiet_hours.covers(at)
    }

    /// # Errors
    /// [`PrefsError`] naming the first field that is out of range or from a future schema.
    pub fn validate(&self) -> Result<(), PrefsError> {
        if self.version > PREFS_VERSION {
            return Err(PrefsError::UnknownVersion {
                found: self.version,
            });
        }
        self.quiet_hours.validate()
    }
}

/// One switch per [`NotificationKind`].
#[expect(
    clippy::struct_excessive_bools,
    reason = "one named field per variant is the point: a map or a bitfield would publish an \
              untyped key set to the client and lose the per-switch default"
)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct KindPrefs {
    pub new_chapter: bool,
    pub series_completed: bool,
    pub source_added: bool,
    pub sync_conflict: bool,
    pub announcement: bool,
}

impl Default for KindPrefs {
    fn default() -> Self {
        Self {
            new_chapter: true,
            series_completed: true,
            source_added: true,
            sync_conflict: true,
            announcement: true,
        }
    }
}

impl KindPrefs {
    #[must_use]
    pub fn allows(&self, kind: NotificationKind) -> bool {
        match kind {
            NotificationKind::NewChapter => self.new_chapter,
            NotificationKind::SeriesCompleted => self.series_completed,
            NotificationKind::SourceAdded => self.source_added,
            NotificationKind::SyncConflict => self.sync_conflict,
            NotificationKind::Announcement => self.announcement,
        }
    }
}

/// One switch per [`WatchStatus`].
///
/// `dropped` and `completed` default **off**: a reader who put a series down has said so, and
/// the watchlist's own `notify` flag is a per-series override, not a way to express that.
#[expect(
    clippy::struct_excessive_bools,
    reason = "one named field per `WatchStatus`, for the same reason as `KindPrefs`"
)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct StatusPrefs {
    pub reading: bool,
    pub planned: bool,
    pub paused: bool,
    pub dropped: bool,
    pub completed: bool,
}

impl Default for StatusPrefs {
    fn default() -> Self {
        Self {
            reading: true,
            planned: true,
            paused: true,
            dropped: false,
            completed: false,
        }
    }
}

impl StatusPrefs {
    #[must_use]
    pub fn allows(&self, status: WatchStatus) -> bool {
        match status {
            WatchStatus::Reading => self.reading,
            WatchStatus::Planned => self.planned,
            WatchStatus::Paused => self.paused,
            WatchStatus::Dropped => self.dropped,
            WatchStatus::Completed => self.completed,
        }
    }
}

/// Where a notification is allowed to go.
///
/// No `email` switch, deliberately. The notifier's email channel addresses *operator* recipients
/// configured on the deployment, not the reader, so a per-reader email toggle would have nothing
/// to switch — which is exactly what the three preferences this document replaced were.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct ChannelPrefs {
    /// The durable inbox row and the unread badge.
    pub in_app: bool,
    /// The live SSE push. Off still writes the row; only the real-time nudge stops.
    pub live: bool,
}

impl Default for ChannelPrefs {
    fn default() -> Self {
        Self {
            in_app: true,
            live: true,
        }
    }
}

/// A nightly window, expressed in minutes from local midnight.
///
/// Minutes plus a fixed UTC offset rather than an IANA zone: the alternative is a timezone
/// database in a crate that deliberately has no I/O, to move a quiet window by an hour twice a
/// year. The client sends the offset its browser reports.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct QuietHours {
    pub enabled: bool,
    /// Minutes from local midnight at which the window opens (inclusive).
    pub start_minute: u16,
    /// Minutes from local midnight at which it closes (exclusive). May be less than
    /// `start_minute`, which means the window wraps past midnight.
    pub end_minute: u16,
    /// Minutes east of UTC that the two boundaries above are measured in.
    pub utc_offset_minutes: i16,
}

impl Default for QuietHours {
    fn default() -> Self {
        Self {
            enabled: false,
            start_minute: 23 * 60,
            end_minute: 7 * 60,
            utc_offset_minutes: 0,
        }
    }
}

impl QuietHours {
    /// Whether `at` falls inside the window.
    ///
    /// An unrepresentable offset and `start == end` both yield `false` — a malformed or empty
    /// window must fail open, since failing closed silences a reader indefinitely.
    #[must_use]
    pub fn covers(&self, at: OffsetDateTime) -> bool {
        if !self.enabled || self.start_minute == self.end_minute {
            return false;
        }
        let Ok(offset) = UtcOffset::from_whole_seconds(i32::from(self.utc_offset_minutes) * 60)
        else {
            return false;
        };
        let local = at.to_offset(offset).time();
        let minute = u16::from(local.hour()) * 60 + u16::from(local.minute());
        if self.start_minute < self.end_minute {
            (self.start_minute..self.end_minute).contains(&minute)
        } else {
            // Wrapping past midnight is two ranges, not one: 23:00–07:00 is `>= 23:00 || < 07:00`.
            minute >= self.start_minute || minute < self.end_minute
        }
    }

    /// # Errors
    /// [`PrefsError`] when a boundary is not a minute of the day, or the offset is not a real one.
    pub fn validate(&self) -> Result<(), PrefsError> {
        for minute in [self.start_minute, self.end_minute] {
            if minute >= MINUTES_PER_DAY {
                return Err(PrefsError::MinuteOutOfRange { found: minute });
            }
        }
        if self.utc_offset_minutes.abs() > MAX_OFFSET_MINUTES {
            return Err(PrefsError::OffsetOutOfRange {
                found: self.utc_offset_minutes,
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::datetime;

    #[test]
    fn defaults_mute_dropped_and_completed_only() {
        let prefs = NotificationPrefs::default();
        assert!(prefs.allows(NotificationKind::NewChapter, WatchStatus::Reading));
        assert!(prefs.allows(NotificationKind::NewChapter, WatchStatus::Planned));
        assert!(prefs.allows(NotificationKind::NewChapter, WatchStatus::Paused));
        assert!(!prefs.allows(NotificationKind::NewChapter, WatchStatus::Dropped));
        assert!(!prefs.allows(NotificationKind::NewChapter, WatchStatus::Completed));
    }

    /// The pre-v1 document was three keys nothing ever read. Decoding it must land on the
    /// defaults rather than on `false` for every unmentioned switch, which is what a
    /// field-level `#[serde(default)]` on `bool` would have produced.
    #[test]
    fn a_legacy_document_decodes_to_the_defaults() {
        let legacy = r#"{"new_chapters": false, "email": true, "digest": false}"#;
        let prefs: NotificationPrefs = serde_json::from_str(legacy).expect("legacy prefs decode");
        assert_eq!(prefs, NotificationPrefs::default());
    }

    #[test]
    fn an_empty_document_is_the_defaults() {
        let prefs: NotificationPrefs = serde_json::from_str("{}").expect("empty prefs decode");
        assert_eq!(prefs, NotificationPrefs::default());
    }

    #[test]
    fn a_partial_document_keeps_the_other_defaults() {
        let prefs: NotificationPrefs =
            serde_json::from_str(r#"{"watch_status": {"dropped": true}}"#).expect("decode");
        assert!(prefs.watch_status.dropped);
        assert!(prefs.watch_status.reading);
        assert!(!prefs.watch_status.completed);
        assert!(prefs.channels.in_app);
    }

    #[test]
    fn a_future_version_is_refused() {
        let prefs = NotificationPrefs {
            version: PREFS_VERSION + 1,
            ..NotificationPrefs::default()
        };
        assert!(matches!(
            prefs.validate(),
            Err(PrefsError::UnknownVersion { .. })
        ));
    }

    #[test]
    fn an_out_of_range_boundary_is_refused() {
        let prefs = NotificationPrefs {
            quiet_hours: QuietHours {
                start_minute: MINUTES_PER_DAY,
                ..QuietHours::default()
            },
            ..NotificationPrefs::default()
        };
        assert!(matches!(
            prefs.validate(),
            Err(PrefsError::MinuteOutOfRange { .. })
        ));
    }

    #[test]
    fn a_wrapping_window_covers_both_sides_of_midnight() {
        let quiet = QuietHours {
            enabled: true,
            start_minute: 23 * 60,
            end_minute: 7 * 60,
            utc_offset_minutes: 0,
        };
        assert!(quiet.covers(datetime!(2026-08-07 23:30 UTC)));
        assert!(quiet.covers(datetime!(2026-08-07 03:00 UTC)));
        assert!(!quiet.covers(datetime!(2026-08-07 07:00 UTC)));
        assert!(!quiet.covers(datetime!(2026-08-07 12:00 UTC)));
    }

    #[test]
    fn the_offset_shifts_the_window() {
        let quiet = QuietHours {
            enabled: true,
            start_minute: 23 * 60,
            end_minute: 7 * 60,
            utc_offset_minutes: 120,
        };
        // 21:30 UTC is 23:30 in the reader's own clock.
        assert!(quiet.covers(datetime!(2026-08-07 21:30 UTC)));
        assert!(!quiet.covers(datetime!(2026-08-07 05:30 UTC)));
    }

    #[test]
    fn a_disabled_or_empty_window_covers_nothing() {
        assert!(!QuietHours::default().covers(datetime!(2026-08-07 23:30 UTC)));
        let empty = QuietHours {
            enabled: true,
            start_minute: 60,
            end_minute: 60,
            utc_offset_minutes: 0,
        };
        assert!(!empty.covers(datetime!(2026-08-07 01:00 UTC)));
    }

    #[test]
    fn every_kind_round_trips_through_its_token() {
        for &kind in NotificationKind::all() {
            assert_eq!(kind.as_str().parse::<NotificationKind>().ok(), Some(kind));
        }
        assert!("not_a_kind".parse::<NotificationKind>().is_err());
    }
}
