//! # tankovault-contracts
//!
//! The wire contract shared between services: task dispatch messages, progress and domain
//! events, subject, stream and consumer names, and the HTTP response bodies the services
//! publish.
//!
//! [`admin`], [`me`] and [`catalogue`] hold the HTTP view types `services/api` returns. They
//! live here, outside `tankovault-db`, so a repository row cannot become the public schema by
//! accident.

pub mod admin;
pub mod catalogue;
pub mod me;
pub mod messages;
pub mod subjects;
pub mod sync;
pub mod watchlist_transfer;

/// Re-exported: a task's subject and worker consumer are both named after the scan mode.
pub use tankovault_domain::ScanMode;

pub use messages::{
    ChapterDiscovered, ProgressEvent, ProviderStateChanged, ScanTaskMessage, TaskKind,
    UserNotification,
};
pub use subjects::{
    CHAPTER_DISCOVERED_SUBJECT, EVENTS_STREAM, EVENTS_SUBJECT_WILDCARD,
    LEGACY_WILDCARD_WORKER_CONSUMER, NOTIFIER_CONSUMER, PROGRESS_CONSUMER, PROGRESS_SUBJECT,
    PROVIDER_STATE_SUBJECT, TASKS_STREAM, TASKS_SUBJECT_WILDCARD, USER_NOTIFY_SUBJECT_PREFIX,
    WORKER_CONSUMER_PREFIX, is_valid_provider_slug, legacy_task_subject, task_subject,
    user_notify_subject, worker_consumer, worker_consumer_lane,
};

#[cfg(test)]
mod tests {
    /// The persistence crate does not depend on `utoipa`.
    ///
    /// A row struct that derives `ToSchema` and is returned verbatim lets a renamed `SELECT`
    /// column rewrite the public HTTP schema and the generated client with no compile error.
    /// Without the dependency that derive cannot be written. This is a test, not a CI step, so
    /// it runs on every `cargo test --workspace`.
    ///
    /// Only direct dependencies are checked. `tankovault-domain` depends on `utoipa`
    /// legitimately, since the wire schema names its entities and enums, and `tankovault-db`
    /// depends on it; only a direct edge can put a `ToSchema` derive on a row struct.
    #[test]
    fn db_crate_does_not_depend_on_utoipa() {
        let output = std::process::Command::new(env!("CARGO"))
            .args([
                "tree",
                "--package",
                "tankovault-db",
                // Build-time and dev-only edges are irrelevant: neither can put a `ToSchema`
                // derive on a row struct. Only the normal dependency graph can.
                "--edges",
                "normal",
                "--depth",
                "1",
                "--prefix",
                "none",
                "--quiet",
            ])
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .output()
            .expect("`cargo tree` must be runnable to check the dependency graph");

        assert!(
            output.status.success(),
            "cargo tree failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );

        let tree = String::from_utf8_lossy(&output.stdout);
        let offenders: Vec<&str> = tree
            .lines()
            .map(str::trim)
            .filter(|line| line.starts_with("utoipa"))
            .collect();

        assert!(
            offenders.is_empty(),
            "tankovault-db must not depend on utoipa — the persistence layer is not the wire \
             schema. Put the published shape in tankovault-contracts and convert in \
             services/api::views instead. Offending edges: {offenders:?}"
        );
    }
}
