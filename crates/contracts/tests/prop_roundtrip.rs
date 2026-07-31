//! Serde round-trip properties for the shared wire contracts.
//!
//! # Why this suite exists
//!
//! `crates/contracts` is the one place where a type is *both* the producer's return value and
//! the consumer's parse target. Everything in it therefore has exactly one job: survive the
//! trip. The project has already paid for that not being checked — the sync DTOs were
//! hand-mirrored on the frontend and silently dropped the connected display name, the
//! last-sync time and every persisted auto-sync setting before they were hoisted here.
//!
//! # Value equality, not `PartialEq`
//!
//! These types deliberately do not derive `PartialEq` (several carry `serde_json::Value`), so
//! the round trip is asserted on the serialized form: `to_value(x) == to_value(from_value(…))`.
//! That is not a weaker check — it is the *right* check, because it is the serialized form
//! that crosses the wire. It catches a field that serializes but does not deserialize, a
//! `rename` applied on only one side, and a `skip_serializing_if` whose default does not
//! survive a decode.
//!
//! A new contract type belongs in this file. If it is not here, nothing proves it round-trips.

use proptest::prelude::*;
use serde::Serialize;
use serde::de::DeserializeOwned;
use tankovault_contracts::messages::{
    ChapterDiscovered, ProgressEvent, ProviderStateChanged, ScanTaskMessage, TaskKind,
    UserNotification,
};
use tankovault_contracts::subjects::{
    is_valid_provider_slug, worker_consumer, worker_consumer_lane,
};
use tankovault_contracts::sync;
use tankovault_domain::{
    ProviderId, ProviderState, RunState, ScanMode, ScanRunId, ScanTaskId, SeriesId, SeriesSourceId,
    UserId,
};
use time::OffsetDateTime;
use uuid::Uuid;

/// Serialize, decode, re-serialize, and require the two documents to be identical.
fn survives_the_wire<T: Serialize + DeserializeOwned>(value: &T) -> Result<(), TestCaseError> {
    let sent = serde_json::to_value(value).expect("a contract type must serialize");
    let received: T = serde_json::from_value(sent.clone())
        .map_err(|e| TestCaseError::fail(format!("failed to decode {sent}: {e}")))?;
    let resent = serde_json::to_value(&received).expect("a contract type must serialize");
    prop_assert_eq!(sent, resent);
    Ok(())
}

/// Timestamps inside the range RFC 3339 can represent, at nanosecond precision — the precision
/// the `time::serde::rfc3339` codec actually writes, so a truncating round trip would show up.
fn timestamp() -> impl Strategy<Value = OffsetDateTime> {
    (0i64..4_000_000_000i64, 0u32..1_000_000_000u32).prop_map(|(secs, nanos)| {
        OffsetDateTime::from_unix_timestamp_nanos(
            i128::from(secs) * 1_000_000_000 + i128::from(nanos),
        )
        .expect("the generated range is representable")
    })
}

/// A handful of payload shapes covering the cases that break naive codecs: empty, nested,
/// a bare scalar, a null, and a number that is not an integer.
fn payload() -> impl Strategy<Value = serde_json::Value> {
    prop::sample::select(vec![
        serde_json::json!({}),
        serde_json::json!({ "page": 3 }),
        serde_json::json!({ "path": "/manga/x", "nested": { "a": [1, 2, 3] } }),
        serde_json::json!(null),
        serde_json::json!("scalar"),
        serde_json::json!(10.5),
        serde_json::json!([]),
    ])
}

fn scan_mode() -> impl Strategy<Value = ScanMode> {
    prop::sample::select(ScanMode::all().to_vec())
}

fn task_kind() -> impl Strategy<Value = TaskKind> {
    prop::sample::select(vec![
        TaskKind::CatalogPage,
        TaskKind::Series,
        TaskKind::LatestFeed,
    ])
}

fn run_state() -> impl Strategy<Value = RunState> {
    prop::sample::select(RunState::all().to_vec())
}

fn provider_state() -> impl Strategy<Value = ProviderState> {
    prop::sample::select(ProviderState::all().to_vec())
}

/// Slugs of the shape the bus actually accepts, including the `-` that makes the consumer
/// name ambiguous to a naive parser.
fn provider_slug() -> impl Strategy<Value = String> {
    "[a-z0-9][a-z0-9_-]{0,20}"
}

proptest! {
    #[test]
    fn a_scan_task_message_survives_the_wire(
        provider_slug in provider_slug(),
        mode in scan_mode(),
        kind in task_kind(),
        target in payload(),
        traceparent in prop::option::of("[0-9a-f-]{1,64}"),
    ) {
        survives_the_wire(&ScanTaskMessage {
            task_id: ScanTaskId::new(),
            run_id: ScanRunId::new(),
            provider_id: ProviderId::new(),
            provider_slug,
            mode,
            kind,
            target,
            traceparent,
        })?;
    }

    #[test]
    fn a_progress_event_survives_the_wire(
        has_provider in any::<bool>(),
        mode in scan_mode(),
        state in run_state(),
        total_tasks in any::<i32>(),
        done_tasks in any::<i32>(),
        failed_tasks in any::<i32>(),
        at in timestamp(),
    ) {
        survives_the_wire(&ProgressEvent {
            run_id: ScanRunId::new(),
            provider_id: has_provider.then(ProviderId::new),
            mode,
            state,
            total_tasks,
            done_tasks,
            failed_tasks,
            at,
        })?;
    }

    /// `published_at` is the interesting field: it uses `rfc3339::option`, so a `None` must
    /// survive as `None` rather than becoming `null`-then-decode-error.
    #[test]
    fn a_chapter_discovered_event_survives_the_wire(
        provider_slug in provider_slug(),
        chapter_number in prop::num::f64::NORMAL | prop::num::f64::ZERO,
        chapter_title in prop::option::of(".{0,40}"),
        chapter_path in "/[a-z0-9/_-]{0,40}",
        published_at in prop::option::of(timestamp()),
        discovered_at in timestamp(),
    ) {
        survives_the_wire(&ChapterDiscovered {
            series_id: SeriesId::new(),
            series_source_id: SeriesSourceId::new(),
            provider_id: ProviderId::new(),
            provider_slug,
            chapter_number,
            chapter_title,
            chapter_path,
            published_at,
            discovered_at,
        })?;
    }

    #[test]
    fn a_user_notification_survives_the_wire(
        kind in "[a-z_]{1,24}",
        payload in payload(),
        created_at in timestamp(),
        unread_count in any::<i64>(),
    ) {
        survives_the_wire(&UserNotification {
            user_id: UserId::new(),
            notification_id: Uuid::now_v7(),
            kind,
            payload,
            created_at,
            unread_count,
        })?;
    }

    #[test]
    fn a_provider_state_change_survives_the_wire(
        provider_slug in provider_slug(),
        previous in provider_state(),
        current in provider_state(),
        reason in prop::option::of(".{0,60}"),
        at in timestamp(),
    ) {
        survives_the_wire(&ProviderStateChanged {
            provider_id: ProviderId::new(),
            provider_slug,
            previous,
            current,
            reason,
            at,
        })?;
    }

    /// The sync link status. `username` and `last_synced_at` are the two fields the
    /// hand-mirrored frontend copy dropped; a round trip is what would have caught it.
    #[test]
    fn a_sync_account_status_survives_the_wire(
        linked in any::<bool>(),
        username in prop::option::of(".{0,40}"),
        last_synced_at in prop::option::of("[0-9TZ:.-]{0,30}"),
    ) {
        survives_the_wire(&sync::AccountStatus { linked, username, last_synced_at })?;
    }

    #[test]
    fn sync_account_settings_survive_the_wire(
        linked in any::<bool>(),
        auto_sync_enabled in any::<bool>(),
        // Drawn from `ConflictPolicy::ALL` rather than from a list of token literals. The
        // literals were this test's own third copy of the vocabulary — the very drift the
        // typed policy exists to remove — and a policy added to the enum would not have been
        // exercised here at all.
        conflict_policy in prop::sample::select(sync::ConflictPolicy::ALL.to_vec()),
        pending_conflicts in any::<i64>(),
    ) {
        survives_the_wire(&sync::AccountSettings {
            linked,
            auto_sync_enabled,
            conflict_policy,
            pending_conflicts,
        })?;
    }

    #[test]
    fn a_sync_provider_listing_survives_the_wire(slug in provider_slug(), name in ".{0,40}") {
        survives_the_wire(&sync::ProviderInfo { slug, name })?;
    }

    #[test]
    fn a_sync_authorize_url_survives_the_wire(url in ".{0,120}") {
        survives_the_wire(&sync::AuthorizeUrl { url })?;
    }

    /// The durable consumer name is built from `(mode, slug)` and taken apart again to recover
    /// lanes for providers that have since been renamed. The existing test does this for one
    /// hand-picked slug; the general contract is that *every* valid slug survives — including
    /// one that itself begins with a mode token, which is precisely where a naive split fails.
    #[test]
    fn a_worker_consumer_name_round_trips_through_its_lane(
        slug in provider_slug(),
        mode in scan_mode(),
    ) {
        prop_assume!(is_valid_provider_slug(&slug));
        let name = worker_consumer(&slug, mode);
        prop_assert_eq!(
            worker_consumer_lane(&name),
            Some((mode, slug.as_str())),
            "consumer name {:?} did not decompose back to its lane", name
        );
    }

    /// A slug that deliberately collides with the mode token. `worker_consumer` documents that
    /// "the mode sits in a fixed position ahead of the slug so the name can be taken apart
    /// again even though a slug may itself contain `-`" — this is that claim, executed.
    #[test]
    fn a_slug_that_starts_with_a_mode_token_still_decomposes(
        tail in "[a-z0-9][a-z0-9_-]{0,12}",
        prefix_mode in scan_mode(),
        mode in scan_mode(),
    ) {
        let slug = format!("{}-{tail}", prefix_mode.as_str());
        let name = worker_consumer(&slug, mode);
        prop_assert_eq!(worker_consumer_lane(&name), Some((mode, slug.as_str())));
    }
}
