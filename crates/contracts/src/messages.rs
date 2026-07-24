//! Serde message and event payloads exchanged over NATS.
//!
//! All payloads are `serde_json`-encoded. They are versioned implicitly by their
//! shape; add fields with `#[serde(default)]` to preserve backward compatibility.

use serde::{Deserialize, Serialize};
use tankovault_domain::{
    ProviderId, ProviderState, RunState, ScanMode, ScanRunId, ScanTaskId, SeriesId, SeriesSourceId,
    UserId,
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

/// A dispatched unit of work, published to `scan.tasks.<provider_slug>` and mirrored
/// in the `scan_tasks` table.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScanTaskMessage {
    pub task_id: ScanTaskId,
    pub run_id: ScanRunId,
    pub provider_id: ProviderId,
    pub provider_slug: String,
    pub mode: ScanMode,
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
    pub run_id: ScanRunId,
    pub provider_id: Option<ProviderId>,
    pub mode: ScanMode,
    pub state: RunState,
    pub total_tasks: i32,
    pub done_tasks: i32,
    pub failed_tasks: i32,
    #[serde(with = "time::serde::rfc3339")]
    pub at: OffsetDateTime,
}

/// Emitted when a worker upserts a genuinely new chapter. Consumed by the notifier
/// (fan-out to watchers) and available to the sync service.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChapterDiscovered {
    pub series_id: SeriesId,
    pub series_source_id: SeriesSourceId,
    pub provider_id: ProviderId,
    pub provider_slug: String,
    pub chapter_number: f64,
    pub chapter_title: Option<String>,
    /// RELATIVE path — resolve against the provider `base_url` at read time.
    pub chapter_path: String,
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub published_at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339")]
    pub discovered_at: OffsetDateTime,
}

/// A live per-user notification push, relayed over **core NATS** (best-effort, non-durable)
/// to the API's `/v1/me/stream` SSE endpoint so a connected client updates its unread badge
/// and feed in real time (design §14, §17.4). The durable copy is the `notifications` row;
/// an offline client misses the push and reconciles via [`Self::unread_count`] on reconnect.
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
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    /// The recipient's unread count *including* this notification, so the client can set its
    /// badge directly without a round-trip to `/v1/me/notifications`.
    pub unread_count: i64,
}

/// Emitted when a provider changes health state (drives console tiles + alerts).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderStateChanged {
    pub provider_id: ProviderId,
    pub provider_slug: String,
    pub previous: ProviderState,
    pub current: ProviderState,
    pub reason: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    pub at: OffsetDateTime,
}

#[cfg(test)]
mod tests {
    use super::*;

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
