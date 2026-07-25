//! # tankovault-contracts
//!
//! The wire contract shared between services: task dispatch messages, progress and domain
//! events, the subject/stream/consumer naming every service agrees on, and the HTTP response
//! bodies one service publishes on another's behalf.
//! Keeping these in one crate means a schema change is a single, reviewable diff that
//! the producer and all consumers compile against.

pub mod messages;
pub mod subjects;
pub mod sync;

pub use messages::{
    ChapterDiscovered, ProgressEvent, ProviderStateChanged, ScanTaskMessage, TaskKind,
    UserNotification,
};
pub use subjects::{
    CHAPTER_DISCOVERED_SUBJECT, EVENTS_STREAM, EVENTS_SUBJECT_WILDCARD, NOTIFIER_CONSUMER,
    PROGRESS_CONSUMER, PROGRESS_SUBJECT, PROVIDER_STATE_SUBJECT, TASKS_STREAM,
    TASKS_SUBJECT_WILDCARD, USER_NOTIFY_SUBJECT_PREFIX, WORKER_CONSUMER, task_subject,
    user_notify_subject,
};
