//! # tankovault-contracts
//!
//! The wire contract shared between services: task dispatch messages, progress and domain
//! events, the subject/stream/consumer naming every service agrees on, and the HTTP response
//! bodies one service publishes on another's behalf — or on its own.
//! Keeping these in one crate means a schema change is a single, reviewable diff that
//! the producer and all consumers compile against.
//!
//! [`admin`], [`me`] and [`catalogue`] hold the HTTP view types `services/api` returns. They
//! were previously repository row structs in `tankovault-db` carrying `ToSchema`, which meant
//! the persistence layer *was* the public schema: renaming a column in a `SELECT` rewrote the
//! published API and the generated client with no compile error at the API boundary, because
//! no handler ever named a field. The row → view conversions now live in `services/api`, the
//! one crate permitted to know both layers, so that change is a compile error in exactly one
//! place. `tankovault-db` no longer depends on `utoipa` at all, which
//! `tests/db_has_no_wire_schema.rs` enforces.

pub mod admin;
pub mod catalogue;
pub mod me;
pub mod messages;
pub mod subjects;
pub mod sync;

/// Re-exported because it is part of the contract's own surface: a task's subject and its
/// worker consumer are both named after the scan mode, so a service that speaks to the bus
/// needs the type without reaching past this crate.
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
    /// The persistence crate must not depend on `utoipa`.
    ///
    /// This is the guard for the bug this crate's `admin`/`me`/`catalogue` modules exist to
    /// fix: `tankovault-db` derived `ToSchema` on 23 repository row structs and 11 handlers
    /// returned those rows verbatim, so renaming a column in a `SELECT` silently rewrote the
    /// public HTTP schema and the generated client with no compile error anywhere. Deleting
    /// the dependency is what makes that impossible to write again — re-adding it here is the
    /// only way back, so this is where the check belongs.
    ///
    /// A test rather than a CI step so it runs on every `cargo test --workspace`, including
    /// locally, and so its reasoning travels with the code it protects.
    ///
    /// The check is on `tankovault-db`'s **direct** dependencies (`--depth 1`), not the whole
    /// tree. The audit proposed `cargo tree -p tankovault-db | grep utoipa`, which cannot
    /// work: `tankovault-domain` depends on `utoipa` legitimately — its entities and enums
    /// *are* shared domain vocabulary that the wire schema names — and `tankovault-db`
    /// depends on `tankovault-domain`. Only a direct edge from the persistence crate can
    /// reintroduce a `ToSchema` derive on a row struct, and only that is checked here.
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
