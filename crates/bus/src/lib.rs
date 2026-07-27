//! # tankovault-bus
//!
//! Thin `JetStream` helpers shared by the control-plane (producer), workers (task
//! consumer), and notifier (event consumer): connect, provision the durable streams,
//! publish tasks/events, and open a durable pull consumer. Subject/stream naming comes
//! from [`tankovault_contracts::subjects`] so producers and consumers cannot drift.

use async_nats::jetstream::{self, AckKind, consumer::PullConsumer, consumer::pull, stream};
use serde::Serialize;
use std::time::Duration;
use tankovault_contracts::{
    ChapterDiscovered, ProgressEvent, ProviderStateChanged, ScanTaskMessage, UserNotification,
    subjects,
};
use thiserror::Error;
use uuid::Uuid;

/// Redelivery deadline for a claimed scan task.
///
/// This bounds how long a worker may go **silent**, not how long a task may take: a task
/// that outlives it calls [`with_ack_heartbeat`] to keep extending the deadline. Keeping it
/// modest is deliberate — a worker that crashes mid-task is redelivered within this window
/// rather than after an hour of dead air.
pub const TASK_ACK_WAIT: Duration = Duration::from_secs(300);

/// How often an in-flight task reports progress, as a fraction of [`TASK_ACK_WAIT`].
///
/// Derived rather than written out so the two cannot drift apart: raising the deadline
/// without widening the heartbeat would be harmless, but tightening it without tightening
/// the heartbeat would silently reintroduce mid-task redelivery.
pub const TASK_ACK_HEARTBEAT: Duration = Duration::from_secs(TASK_ACK_WAIT.as_secs() / 5);

/// Run `work` while holding off redelivery of `msg`.
///
/// `JetStream` redelivers any message not acked within the consumer's `ack_wait`. For work
/// that legitimately runs longer than that — a catalogue page that registers twenty thousand
/// series, a series with thousands of chapters — the fix is not a larger deadline (which
/// also delays recovery from a genuine crash) but to say "still working": an
/// [`AckKind::Progress`] resets the timer without settling the message.
///
/// This is the reason a slow task no longer means a *duplicated* task. Idempotent writes
/// remain the correctness backstop; this keeps the duplication from happening at all.
///
/// A failed heartbeat is logged, not fatal: the work continues, and the worst case is the
/// redelivery this exists to avoid — which the idempotency layer already tolerates.
pub async fn with_ack_heartbeat<F>(msg: &jetstream::Message, every: Duration, work: F) -> F::Output
where
    F: std::future::Future,
{
    tokio::pin!(work);
    let mut ticker = tokio::time::interval(every);
    // A tokio interval fires immediately; that first tick would be a pointless progress ack
    // before any work has happened.
    ticker.tick().await;
    loop {
        tokio::select! {
            // Finishing beats ticking: never send a progress ack for work already done.
            biased;
            output = &mut work => return output,
            _ = ticker.tick() => match msg.ack_with(AckKind::Progress).await {
                Ok(()) => tracing::debug!("task still running; extended redelivery deadline"),
                Err(e) => tracing::warn!(
                    error = %e,
                    "could not extend redelivery deadline; task may be redelivered while \
                     still running"
                ),
            },
        }
    }
}

/// Hand `msg` back to the stream for redelivery after `delay`, instead of settling it.
///
/// This is the broker half of retrying a task that failed for a reason time can fix — an
/// unsolved challenge, a rate-limited provider, a solver that was restarting. Acking such a
/// task (the only option before this existed) spent the failure permanently: the series was
/// simply missing from the run, with a log line as the only trace.
///
/// The consumer, not the stream, bounds the attempts: callers check [`delivery_count`] and
/// settle the message once retrying stops being worthwhile.
///
/// # Errors
/// [`BusError::Jetstream`] if the negative ack could not be sent — in which case the message
/// is still unsettled and the stream redelivers it once the ack deadline lapses.
pub async fn retry_later(msg: &jetstream::Message, delay: Duration) -> Result<(), BusError> {
    msg.ack_with(AckKind::Nak(Some(delay)))
        .await
        .map_err(|e| BusError::Jetstream(e.to_string()))
}

/// How many times this message has been delivered, including the current delivery.
///
/// Falls back to `1` when the stream metadata cannot be read: an unreadable count must not
/// make a task look fresh forever, so the safe reading is "this is the first attempt" only
/// for the decision the caller then bounds by its own limit.
#[must_use]
pub fn delivery_count(msg: &jetstream::Message) -> u64 {
    msg.info()
        .ok()
        .and_then(|info| u64::try_from(info.delivered).ok())
        .unwrap_or(1)
}

/// A `JetStream` context plus the underlying core-NATS client.
///
/// Durable task/event traffic goes through the `JetStream` context; ephemeral, best-effort
/// live pushes (per-user notifications) use the core client directly so they are never
/// retained for a disconnected subscriber.
#[derive(Clone)]
pub struct Bus {
    client: async_nats::Client,
    js: jetstream::Context,
}

/// Bus errors.
#[derive(Debug, Error)]
pub enum BusError {
    #[error("nats connect error: {0}")]
    Connect(String),
    #[error("jetstream error: {0}")]
    Jetstream(String),
    #[error("nats error: {0}")]
    Nats(String),
    #[error("serialization error: {0}")]
    Serde(#[from] serde_json::Error),
}

impl Bus {
    /// Connect to NATS and build a `JetStream` context.
    ///
    /// # Errors
    /// [`BusError::Connect`] if the server is unreachable.
    pub async fn connect(url: &str) -> Result<Self, BusError> {
        let client = async_nats::connect(url)
            .await
            .map_err(|e| BusError::Connect(e.to_string()))?;
        Ok(Self {
            js: jetstream::new(client.clone()),
            client,
        })
    }

    /// Round-trip the broker, for a readiness probe.
    ///
    /// Uses `flush` rather than the client's cached connection state: the client
    /// reconnects transparently in the background, so its state flag can read `Connected`
    /// while the server is in fact unreachable. A flush waits for the server to
    /// acknowledge, which is what a probe needs to assert.
    ///
    /// # Errors
    /// [`BusError::Nats`] if the round trip fails or the connection is down.
    pub async fn ping(&self) -> Result<(), BusError> {
        self.client
            .flush()
            .await
            .map_err(|e| BusError::Nats(e.to_string()))
    }

    /// The underlying `JetStream` context (for advanced use / consumers).
    #[must_use]
    pub fn jetstream(&self) -> &jetstream::Context {
        &self.js
    }

    /// Idempotently create the tasks and events streams.
    ///
    /// # Errors
    /// [`BusError::Jetstream`] on a provisioning failure.
    pub async fn ensure_streams(&self) -> Result<(), BusError> {
        self.js
            .get_or_create_stream(stream::Config {
                name: subjects::TASKS_STREAM.to_owned(),
                subjects: vec![subjects::TASKS_SUBJECT_WILDCARD.to_owned()],
                retention: stream::RetentionPolicy::WorkQueue,
                ..Default::default()
            })
            .await
            .map_err(|e| BusError::Jetstream(e.to_string()))?;
        self.js
            .get_or_create_stream(stream::Config {
                name: subjects::EVENTS_STREAM.to_owned(),
                subjects: vec![subjects::EVENTS_SUBJECT_WILDCARD.to_owned()],
                ..Default::default()
            })
            .await
            .map_err(|e| BusError::Jetstream(e.to_string()))?;
        Ok(())
    }

    /// Publish a scan task to its provider subject.
    ///
    /// # Errors
    /// [`BusError`] on serialization or publish failure.
    pub async fn publish_task(&self, msg: &ScanTaskMessage) -> Result<(), BusError> {
        let subject = subjects::task_subject(&msg.provider_slug);
        self.publish(subject, msg).await
    }

    /// Publish a progress event.
    ///
    /// # Errors
    /// [`BusError`] on failure.
    pub async fn publish_progress(&self, event: &ProgressEvent) -> Result<(), BusError> {
        self.publish(subjects::PROGRESS_SUBJECT.to_owned(), event)
            .await
    }

    /// Publish a new-chapter domain event.
    ///
    /// # Errors
    /// [`BusError`] on failure.
    pub async fn publish_chapter(&self, event: &ChapterDiscovered) -> Result<(), BusError> {
        self.publish(subjects::CHAPTER_DISCOVERED_SUBJECT.to_owned(), event)
            .await
    }

    /// Publish a provider-state-change event.
    ///
    /// # Errors
    /// [`BusError`] on failure.
    pub async fn publish_provider_state(
        &self,
        event: &ProviderStateChanged,
    ) -> Result<(), BusError> {
        self.publish(subjects::PROVIDER_STATE_SUBJECT.to_owned(), event)
            .await
    }

    /// Publish a live per-user notification over **core NATS** (non-durable, fire-and-forget).
    ///
    /// Unlike the `JetStream` publishers this does not await a broker ack and is not retained:
    /// if no API replica currently holds an SSE connection for the user, the message is simply
    /// dropped. The durable record is the `notifications` row, so this can never lose data —
    /// only a live badge update.
    ///
    /// # Errors
    /// [`BusError::Nats`] if the client cannot hand the message to the connection, or
    /// [`BusError::Serde`] on encoding failure.
    pub async fn publish_user_notification(
        &self,
        event: &UserNotification,
    ) -> Result<(), BusError> {
        let subject = subjects::user_notify_subject(event.user_id.as_uuid());
        let bytes = serde_json::to_vec(event)?;
        self.client
            .publish(subject, bytes::Bytes::from(bytes))
            .await
            .map_err(|e| BusError::Nats(e.to_string()))
    }

    /// Subscribe (core NATS) to a single user's live-notification subject. The returned
    /// [`async_nats::Subscriber`] is a `Stream` of raw messages whose payloads decode to
    /// [`UserNotification`]; it is unsubscribed automatically when dropped (e.g. when the
    /// SSE connection closes).
    ///
    /// # Errors
    /// [`BusError::Nats`] if the subscription cannot be established.
    pub async fn subscribe_user_notifications(
        &self,
        user_id: Uuid,
    ) -> Result<async_nats::Subscriber, BusError> {
        let subject = subjects::user_notify_subject(user_id);
        self.client
            .subscribe(subject)
            .await
            .map_err(|e| BusError::Nats(e.to_string()))
    }

    async fn publish<T: Serialize>(&self, subject: String, payload: &T) -> Result<(), BusError> {
        let bytes = serde_json::to_vec(payload)?;
        let ack = self
            .js
            .publish(subject, bytes::Bytes::from(bytes))
            .await
            .map_err(|e| BusError::Jetstream(e.to_string()))?;
        ack.await.map_err(|e| BusError::Jetstream(e.to_string()))?;
        Ok(())
    }

    /// Open (or reuse) a durable pull consumer on the tasks stream for the worker pool.
    ///
    /// # Errors
    /// [`BusError::Jetstream`] on failure.
    pub async fn task_consumer(&self) -> Result<PullConsumer, BusError> {
        self.durable_consumer(
            subjects::TASKS_STREAM,
            subjects::WORKER_CONSUMER,
            subjects::TASKS_SUBJECT_WILDCARD,
            TASK_ACK_WAIT,
        )
        .await
    }

    /// Open (or reuse) a durable pull consumer on the events stream, filtered to
    /// `filter_subject` (e.g. the chapter-discovered subject for the notifier).
    ///
    /// # Errors
    /// [`BusError::Jetstream`] on failure.
    pub async fn event_consumer(
        &self,
        durable: &str,
        filter_subject: &str,
    ) -> Result<PullConsumer, BusError> {
        self.durable_consumer(
            subjects::EVENTS_STREAM,
            durable,
            filter_subject,
            Duration::from_secs(30),
        )
        .await
    }

    async fn durable_consumer(
        &self,
        stream_name: &str,
        durable: &str,
        filter_subject: &str,
        ack_wait: Duration,
    ) -> Result<PullConsumer, BusError> {
        let stream = self
            .js
            .get_stream(stream_name)
            .await
            .map_err(|e| BusError::Jetstream(e.to_string()))?;
        let consumer = stream
            .get_or_create_consumer(
                durable,
                pull::Config {
                    durable_name: Some(durable.to_owned()),
                    filter_subject: filter_subject.to_owned(),
                    ack_policy: async_nats::jetstream::consumer::AckPolicy::Explicit,
                    ack_wait,
                    ..Default::default()
                },
            )
            .await
            .map_err(|e| BusError::Jetstream(e.to_string()))?;
        Ok(consumer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heartbeat_fits_comfortably_inside_the_redelivery_deadline() {
        // The whole point of the heartbeat is that a progress ack lands well before the
        // deadline lapses. If someone tightens `TASK_ACK_WAIT` without revisiting the
        // divisor, mid-task redelivery comes back silently — so assert the margin here
        // rather than trusting the comment.
        assert!(
            TASK_ACK_HEARTBEAT.as_secs() > 0,
            "a zero heartbeat would spin"
        );
        assert!(
            TASK_ACK_HEARTBEAT * 2 <= TASK_ACK_WAIT,
            "heartbeat {TASK_ACK_HEARTBEAT:?} leaves no margin under deadline {TASK_ACK_WAIT:?}"
        );
    }
}
