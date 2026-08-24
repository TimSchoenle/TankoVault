//! Serde message and event payloads exchanged over NATS.
//!
//! All payloads are `serde_json`-encoded. They are versioned implicitly by their
//! shape; add fields with `#[serde(default)]` to preserve backward compatibility.

use serde::{Deserialize, Serialize};
use tankovault_domain::{
    ChapterAccess, ProviderId, ProviderState, RunState, ScanMode, ScanRunId, ScanTaskId, SeriesId,
    SeriesSourceId, UserId,
};
use time::OffsetDateTime;
use uuid::Uuid;

/// The kind of scan task, matching `scan_tasks.kind` and adapter entry points.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskKind {
    /// One page of the provider catalogue (full scan fan-out).
    CatalogPage,
    /// One series: fetch metadata + chapters and upsert.
    Series,
    /// The provider "latest updates" feed (fast scan).
    LatestFeed,
}

impl TaskKind {
    /// Stable lowercase name, matching the `scan_tasks.kind` column and used as a metric
    /// label — so a rename here is a schema and a dashboard change, not just a rename.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::CatalogPage => "catalog_page",
            Self::Series => "series",
            Self::LatestFeed => "latest_feed",
        }
    }

    /// The kind a `scan_tasks.kind` value names, or `None` if it names none of them.
    ///
    /// The inverse of [`as_str`](Self::as_str), for the one direction the column has to be read
    /// back in: rebuilding a task's message from its row.
    #[must_use]
    pub fn from_token(token: &str) -> Option<Self> {
        [Self::CatalogPage, Self::Series, Self::LatestFeed]
            .into_iter()
            .find(|kind| kind.as_str() == token)
    }
}

/// A dispatched unit of work, published to `scan.tasks.<provider_slug>` and mirrored
/// in the `scan_tasks` table.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScanTaskMessage {
    /// The `scan_tasks` row this message mirrors, and what a worker claims.
    pub task_id: ScanTaskId,
    /// The run that dispatched it.
    pub run_id: ScanRunId,
    /// The provider to crawl.
    pub provider_id: ProviderId,
    /// Its slug, which is also the last token of the subject this was published on.
    pub provider_slug: String,
    /// The run's cadence, which decides which adapter entry point the worker calls.
    pub mode: ScanMode,
    /// Which entry point, and therefore how `target` is read.
    pub kind: TaskKind,
    /// Task-specific target, e.g. `{"path":"/manga/x","page":3}`.
    pub target: serde_json::Value,
    /// Optional distributed-trace context carried across the broker hop.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub traceparent: Option<String>,
}

/// A compact progress update, published on [`crate::subjects::PROGRESS_SUBJECT`] and
/// relayed to the console over SSE.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProgressEvent {
    /// The run this update is about.
    pub run_id: ScanRunId,
    /// `None` for a run covering every enabled provider.
    pub provider_id: Option<ProviderId>,
    /// The run's cadence.
    pub mode: ScanMode,
    /// Where the run is in its lifecycle at `at`.
    pub state: RunState,
    /// Tasks the run dispatched.
    pub total_tasks: i32,
    /// Tasks settled successfully so far.
    pub done_tasks: i32,
    /// Tasks that gave up so far.
    pub failed_tasks: i32,
    /// When the counters were read. Events can arrive out of order, so this is the tiebreak.
    #[serde(with = "time::serde::rfc3339")]
    pub at: OffsetDateTime,
}

/// Emitted when a worker upserts a genuinely new chapter. Consumed by the notifier
/// (fan-out to watchers) and available to the sync service.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChapterDiscovered {
    /// The canonical series the chapter belongs to.
    pub series_id: SeriesId,
    /// The provider page it was found on.
    pub series_source_id: SeriesSourceId,
    /// The provider hosting that page.
    pub provider_id: ProviderId,
    /// Its slug, carried so a consumer need not join to render the event.
    pub provider_slug: String,
    /// Chapter number, fractional for a part release.
    pub chapter_number: f64,
    /// Title as the provider gives it, `None` when it publishes only a number.
    pub chapter_title: Option<String>,
    /// RELATIVE path — resolve against the provider `base_url` at read time.
    pub chapter_path: String,
    /// When the provider says it went up, `None` when it publishes no date.
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub published_at: Option<OffsetDateTime>,
    /// What the provider said this chapter costs, as the ingest stored it.
    ///
    /// Carried on the event rather than looked up by the consumer: the notifier decides whether
    /// to announce, and a paywalled chapter announced to a reader who has not paid sends them to
    /// a page that answers with a paywall. Defaulted, so a message published by an older worker
    /// still deserializes — as `free`, which is what every message before this field meant.
    #[serde(default)]
    pub access: ChapterAccess,
    /// When the provider said the paywall lifts, where it stated one. Always `None` on a free
    /// chapter, and `None` on a locked one means no date was announced — never "already open".
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub unlocks_at: Option<OffsetDateTime>,
    /// When the ingest wrote the row, which is what makes the chapter new.
    #[serde(with = "time::serde::rfc3339")]
    pub discovered_at: OffsetDateTime,
}

/// A live per-user notification push, relayed to the API's `/v1/me/stream` SSE endpoint.
///
/// Carried over core NATS, so delivery is best-effort and nothing is replayed. The durable copy
/// is the `notifications` row: a client that was offline misses the push and reconciles from
/// [`Self::unread_count`] when it reconnects.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserNotification {
    /// Recipient; also determines the [`crate::subjects::user_notify_subject`] routing.
    pub user_id: UserId,
    /// The persisted `notifications.id` this push mirrors (lets the client dedup/mark-read).
    pub notification_id: Uuid,
    /// Notification kind, e.g. `new_chapter` (mirrors `notifications.kind`).
    pub kind: String,
    /// Opaque per-kind payload (mirrors `notifications.payload`).
    pub payload: serde_json::Value,
    /// When the persisted row was written.
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    /// The recipient's unread count *including* this notification, so the client can set its
    /// badge directly without a round-trip to `/v1/me/notifications`.
    pub unread_count: i64,
}

/// Emitted when a provider changes health state (drives console tiles + alerts).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderStateChanged {
    /// The provider whose health moved.
    pub provider_id: ProviderId,
    /// Its slug, carried so a console tile needs no join.
    pub provider_slug: String,
    /// The state it left.
    pub previous: ProviderState,
    /// The state it entered.
    pub current: ProviderState,
    /// What moved it, `None` for a transition an operator made by hand.
    pub reason: Option<String>,
    /// When the transition was recorded.
    #[serde(with = "time::serde::rfc3339")]
    pub at: OffsetDateTime,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every kind's column token must read back as the same kind.
    ///
    /// The pair is what lets a task be rebuilt from its row after its message was lost. A kind
    /// missing from `from_token` is not a compile error — it is a task the reconciler quietly
    /// refuses to republish, so its run never finishes.
    #[test]
    fn every_task_kind_round_trips_through_its_column_token() {
        for kind in [
            TaskKind::CatalogPage,
            TaskKind::Series,
            TaskKind::LatestFeed,
        ] {
            assert_eq!(TaskKind::from_token(kind.as_str()), Some(kind));
        }
        assert_eq!(TaskKind::from_token("catalogue_page"), None);
        assert_eq!(TaskKind::from_token(""), None);
    }

    #[test]
    fn task_message_round_trips() {
        let msg = ScanTaskMessage {
            task_id: ScanTaskId::new(),
            run_id: ScanRunId::new(),
            provider_id: ProviderId::new(),
            provider_slug: "kunmanga".into(),
            mode: ScanMode::Full,
            kind: TaskKind::CatalogPage,
            target: serde_json::json!({ "page": 3 }),
            traceparent: None,
        };
        let bytes = serde_json::to_vec(&msg).unwrap();
        let back: ScanTaskMessage = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(back.provider_slug, "kunmanga");
        assert_eq!(back.kind, TaskKind::CatalogPage);
    }

    #[test]
    fn task_kind_serializes_snake_case() {
        assert_eq!(
            serde_json::to_string(&TaskKind::LatestFeed).unwrap(),
            "\"latest_feed\""
        );
    }

    #[test]
    fn user_notification_round_trips() {
        let event = UserNotification {
            user_id: tankovault_domain::UserId::new(),
            notification_id: Uuid::now_v7(),
            kind: "new_chapter".into(),
            payload: serde_json::json!({ "chapter_number": 42.5 }),
            created_at: OffsetDateTime::UNIX_EPOCH,
            unread_count: 3,
        };
        let bytes = serde_json::to_vec(&event).unwrap();
        let back: UserNotification = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(back.kind, "new_chapter");
        assert_eq!(back.unread_count, 3);
        assert_eq!(back.notification_id, event.notification_id);
    }
}
