//! NATS `JetStream` subject and stream naming.
//!
//! Two durable streams:
//! - **tasks** (`scan.tasks.<provider_slug>`): per-provider work streams so worker
//!   consumer groups can be balanced per provider (design §2, §12).
//! - **events** (`scan.progress`, `chapter.discovered`, ...): domain events relayed to
//!   the notifier, sync service, and (via SSE) the operator console.

/// Durable stream carrying dispatched scan tasks.
pub const TASKS_STREAM: &str = "TANKOVAULT_TASKS";
/// Durable stream carrying domain events.
pub const EVENTS_STREAM: &str = "TANKOVAULT_EVENTS";

/// Wildcard subject that the tasks stream binds (`scan.tasks.*`).
pub const TASKS_SUBJECT_WILDCARD: &str = "scan.tasks.*";
/// Wildcard subject that the events stream binds (`scan.events.>`).
pub const EVENTS_SUBJECT_WILDCARD: &str = "scan.events.>";

/// Subject a scan task for a specific provider is published to.
#[must_use]
pub fn task_subject(provider_slug: &str) -> String {
    format!("scan.tasks.{provider_slug}")
}

/// Progress event subject (run/task counters for the console).
pub const PROGRESS_SUBJECT: &str = "scan.events.progress";
/// New-chapter domain event subject, consumed by the notifier and sync service.
pub const CHAPTER_DISCOVERED_SUBJECT: &str = "scan.events.chapter_discovered";
/// Provider health-change event subject.
pub const PROVIDER_STATE_SUBJECT: &str = "scan.events.provider_state";

/// Core-NATS (non-durable) subject prefix for live per-user notification pushes.
///
/// Live pushes are deliberately best-effort and **not** carried on the durable events
/// stream: the durable record is the `notifications` row, so a client that is offline
/// simply misses the push and catches up via its unread count on reconnect. This keeps
/// `JetStream` free of one-subject-per-user fan-out and avoids retained backlog for users
/// who are never connected.
pub const USER_NOTIFY_SUBJECT_PREFIX: &str = "notify.user";

/// The core-NATS subject a single user's live notifications are published to. The API's
/// `/v1/me/stream` SSE endpoint subscribes to exactly this subject for the connected user.
#[must_use]
pub fn user_notify_subject(user_id: uuid::Uuid) -> String {
    format!("{USER_NOTIFY_SUBJECT_PREFIX}.{user_id}")
}

/// Durable consumer name used by the worker pool on the tasks stream.
pub const WORKER_CONSUMER: &str = "tankovault-workers";
/// Durable consumer name used by the notifier on the events stream.
pub const NOTIFIER_CONSUMER: &str = "tankovault-notifier";
/// Durable consumer name used by the control-plane progress aggregator.
pub const PROGRESS_CONSUMER: &str = "tankovault-progress";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_subject_is_namespaced_by_provider() {
        assert_eq!(task_subject("kunmanga"), "scan.tasks.kunmanga");
    }

    #[test]
    fn user_notify_subject_is_namespaced_by_user() {
        let id = uuid::Uuid::nil();
        assert_eq!(
            user_notify_subject(id),
            "notify.user.00000000-0000-0000-0000-000000000000"
        );
    }
}
