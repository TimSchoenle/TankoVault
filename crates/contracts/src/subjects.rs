//! NATS `JetStream` subject and stream naming.
//!
//! Two durable streams: **tasks** (`scan.tasks.<provider_slug>.<scan_mode>`, one queue per
//! provider and scan mode) and **events** (`scan.progress`, `chapter.discovered`, ...),
//! relayed to the notifier, sync service, and (via SSE) the operator console.

use tankovault_domain::ScanMode;

/// Durable stream carrying dispatched scan tasks.
pub const TASKS_STREAM: &str = "TANKOVAULT_TASKS";
/// Durable stream carrying domain events.
pub const EVENTS_STREAM: &str = "TANKOVAULT_EVENTS";

/// Wildcard subject that the tasks stream binds (`scan.tasks.>`).
///
/// `>` rather than `*` because a task subject carries two tokens — provider and scan mode —
/// and because it keeps matching [`legacy_task_subject`], the single-token spelling used
/// before the queue was tiered.
pub const TASKS_SUBJECT_WILDCARD: &str = "scan.tasks.>";
/// Wildcard subject that the events stream binds (`scan.events.>`).
pub const EVENTS_SUBJECT_WILDCARD: &str = "scan.events.>";

/// Subject a scan task is published to: one queue per provider **and scan mode**.
///
/// The mode is part of the subject because it is the priority class: a fast scan surfaces new
/// chapters and costs one task per provider, while a full scan fans out into one task per
/// catalogue entry. Separate queues let a worker take the fast task first instead of finding
/// it behind a six-figure backfill backlog.
#[must_use]
pub fn task_subject(provider_slug: &str, mode: ScanMode) -> String {
    format!("scan.tasks.{provider_slug}.{}", mode.as_str())
}

/// The pre-tiering subject, which carried both scan modes for a provider.
///
/// Still consumed: the full-scan lane binds it alongside its own subject, so a task
/// published by a replica that has not yet been upgraded is executed rather than left in
/// the stream with no consumer that can reach it.
#[must_use]
pub fn legacy_task_subject(provider_slug: &str) -> String {
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

/// Name prefix for the worker pool's durable consumers on the tasks stream.
///
/// One consumer per provider and scan mode, not a single consumer on the `scan.tasks.*`
/// wildcard: a wildcard consumer serves the stream in publish order, so a full catalogue scan
/// would sit at the head of the queue with everything else waiting behind it. It is also a
/// hard requirement — `JetStream` rejects two consumers whose filter subjects overlap, so
/// narrow per-lane filters are the only way to have more than one consumer at all.
pub const WORKER_CONSUMER_PREFIX: &str = "tankovault-workers";

/// The pre-fairness durable consumer that bound the whole `scan.tasks.*` wildcard.
///
/// Frozen as its own constant rather than derived from [`WORKER_CONSUMER_PREFIX`]: it names
/// a consumer that must be *deleted* on upgrade (its filter overlaps every per-provider
/// filter, which a work-queue stream refuses), and that removal has to keep working even if
/// the prefix is ever renamed.
pub const LEGACY_WILDCARD_WORKER_CONSUMER: &str = "tankovault-workers";

/// The durable consumer the worker pool uses for one provider's tasks in one scan mode.
///
/// The mode sits in a fixed position ahead of the slug so the name can be taken apart again
/// even though a slug may itself contain `-`.
#[must_use]
pub fn worker_consumer(provider_slug: &str, mode: ScanMode) -> String {
    format!("{WORKER_CONSUMER_PREFIX}-{}-{provider_slug}", mode.as_str())
}

/// The lane a worker consumer name was built for, or `None` if `name` is not one.
///
/// Lets a worker recover the lanes that already exist on the stream, so tasks belonging to a
/// provider that has since been renamed or deleted still get consumed. The mode sits ahead of
/// the slug so the name can be taken apart from the left even when the slug itself contains `-`.
///
/// ```
/// use tankovault_contracts::ScanMode;
/// use tankovault_contracts::subjects::{worker_consumer, worker_consumer_lane};
///
/// // Round-trips for every mode, which is the property the recovery path relies on.
/// for mode in ScanMode::all() {
///     let name = worker_consumer("mangadex", *mode);
///     assert_eq!(worker_consumer_lane(&name), Some((*mode, "mangadex")));
/// }
///
/// // A slug containing the separator still comes back whole.
/// let name = worker_consumer("kun-manga", ScanMode::Fast);
/// assert_eq!(worker_consumer_lane(&name), Some((ScanMode::Fast, "kun-manga")));
///
/// // Anything that is not one of our consumer names is refused rather than half-parsed —
/// // including the legacy wildcard consumer, which carries no lane and must be *deleted*
/// // on upgrade rather than adopted.
/// assert_eq!(worker_consumer_lane("some-other-consumer"), None);
/// assert_eq!(
///     worker_consumer_lane(tankovault_contracts::LEGACY_WILDCARD_WORKER_CONSUMER),
///     None,
/// );
/// ```
#[must_use]
pub fn worker_consumer_lane(name: &str) -> Option<(ScanMode, &str)> {
    let rest = name
        .strip_prefix(WORKER_CONSUMER_PREFIX)
        .and_then(|rest| rest.strip_prefix('-'))?;
    for mode in ScanMode::all() {
        let slug = rest
            .strip_prefix(mode.as_str())
            .and_then(|rest| rest.strip_prefix('-'))
            .filter(|slug| !slug.is_empty());
        if let Some(slug) = slug {
            return Some((*mode, slug));
        }
    }
    None
}

/// Whether `slug` can address a provider on the bus.
///
/// A slug becomes both the final token of [`task_subject`] and part of a durable consumer
/// name, and NATS accepts neither `.`, `*`, `>` nor whitespace in either position. Checking
/// it turns "this provider's tasks silently never dispatch" into a visible rejection.
#[must_use]
pub fn is_valid_provider_slug(slug: &str) -> bool {
    !slug.is_empty()
        && slug
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}
/// Durable consumer name used by the notifier on the events stream.
pub const NOTIFIER_CONSUMER: &str = "tankovault-notifier";
/// Durable consumer name used by the control-plane progress aggregator.
pub const PROGRESS_CONSUMER: &str = "tankovault-progress";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_subject_is_namespaced_by_provider_and_mode() {
        assert_eq!(
            task_subject("kunmanga", ScanMode::Fast),
            "scan.tasks.kunmanga.fast"
        );
        assert_eq!(
            task_subject("kunmanga", ScanMode::Full),
            "scan.tasks.kunmanga.full"
        );
        // The two modes must be genuinely different queues, or prioritising one over the
        // other is impossible.
        assert_ne!(
            task_subject("kunmanga", ScanMode::Fast),
            task_subject("kunmanga", ScanMode::Full)
        );
    }

    #[test]
    fn the_legacy_subject_is_not_reused_by_either_mode() {
        // The full-scan lane binds this alongside its own subject to drain tasks published
        // before the split; if a mode subject collided with it, one lane would swallow the
        // other's work.
        let legacy = legacy_task_subject("kunmanga");
        assert_eq!(legacy, "scan.tasks.kunmanga");
        for mode in ScanMode::all() {
            assert_ne!(task_subject("kunmanga", *mode), legacy);
        }
    }

    #[test]
    fn a_worker_consumer_name_round_trips_to_its_lane() {
        for mode in ScanMode::all() {
            // A slug containing '-' is the case that makes the mode's fixed position matter.
            let name = worker_consumer("kun-manga", *mode);
            assert_eq!(worker_consumer_lane(&name), Some((*mode, "kun-manga")));
        }
        assert_eq!(
            worker_consumer("kunmanga", ScanMode::Fast),
            "tankovault-workers-fast-kunmanga"
        );
    }

    #[test]
    fn only_per_lane_consumer_names_yield_a_lane() {
        // The wildcard consumer this replaced shares the prefix exactly; reading a lane out
        // of it would have the worker filter on a provider called "".
        assert_eq!(worker_consumer_lane(LEGACY_WILDCARD_WORKER_CONSUMER), None);
        assert_eq!(worker_consumer_lane("tankovault-workers-fast-"), None);
        // Pre-tiering per-provider names carry no mode and are not resumable as a lane.
        assert_eq!(worker_consumer_lane("tankovault-workers-kunmanga"), None);
        assert_eq!(worker_consumer_lane(NOTIFIER_CONSUMER), None);
    }

    #[test]
    fn a_slug_that_would_break_its_subject_is_rejected() {
        assert!(is_valid_provider_slug("kunmanga"));
        assert!(is_valid_provider_slug("manga-dex_2"));
        // Each of these either splits the subject into extra tokens or is refused outright
        // as part of a durable consumer name.
        for bad in ["", "man.ga", "*", ">", "kun manga"] {
            assert!(!is_valid_provider_slug(bad), "{bad:?} should be rejected");
        }
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
