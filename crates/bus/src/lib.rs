//! # tankovault-bus
//!
//! Thin `JetStream` helpers shared by the control-plane (producer), workers (task
//! consumer), and notifier (event consumer): connect, provision the durable streams,
//! publish tasks/events, and open a durable pull consumer. Subject/stream naming comes
//! from [`tankovault_contracts::subjects`] so producers and consumers cannot drift.

use async_nats::jetstream::{self, AckKind, consumer::PullConsumer, consumer::pull, stream};
use futures::StreamExt;
use secrecy::{ExposeSecret as _, SecretString};
use serde::Serialize;
use std::time::Duration;
use tankovault_contracts::{
    ChapterDiscovered, ProgressEvent, ProviderStateChanged, ScanMode, ScanTaskMessage,
    UserNotification, subjects,
};
use thiserror::Error;
use uuid::Uuid;

/// A message as it arrives from the broker: the envelope carrying a serialized
/// [`ScanTaskMessage`] payload plus its ack handle.
///
/// Re-exported so services can name it without taking their own `async-nats` dependency —
/// this crate stays the single place the broker client is pinned.
pub use async_nats::jetstream::Message as BrokerMessage;

/// A durable pull consumer on one subject, re-exported for the same reason as
/// [`BrokerMessage`].
pub use async_nats::jetstream::consumer::PullConsumer as BrokerConsumer;

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

/// Whether a consumer operation failed because the consumer is not there.
///
/// `async-nats` models this as a `JetStream` API error code rather than a distinct error
/// kind, so the check has to reach for the code. It is the one failure that means "nothing
/// to do" for an idempotent delete.
fn is_consumer_not_found(err: &stream::ConsumerError) -> bool {
    matches!(
        err.kind(),
        stream::ConsumerErrorKind::JetStream(e)
            if e.error_code() == jetstream::ErrorCode::CONSUMER_NOT_FOUND
    )
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
    /// `url` is a [`SecretString`] because `nats://user:pass@host` is a supported form; the
    /// compose deployment happens not to use it, but the type has to describe the field, not
    /// one deployment's value. It is exposed exactly here, into the client builder.
    ///
    /// # Errors
    /// [`BusError::Connect`] if the server is unreachable.
    pub async fn connect(url: &SecretString) -> Result<Self, BusError> {
        let client = async_nats::connect(url.expose_secret())
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

    /// Idempotently create — or bring into line — the tasks and events streams.
    ///
    /// `create_or_update_stream` rather than `get_or_create_stream`: the latter returns an
    /// existing stream untouched, so widening the tasks stream's subject binding from
    /// `scan.tasks.*` to `scan.tasks.>` would never take effect on an installation that
    /// already has the stream, and every tiered task would be published to a subject no
    /// stream captures.
    ///
    /// # Errors
    /// [`BusError::Jetstream`] on a provisioning failure.
    pub async fn ensure_streams(&self) -> Result<(), BusError> {
        self.js
            .create_or_update_stream(stream::Config {
                name: subjects::TASKS_STREAM.to_owned(),
                subjects: vec![subjects::TASKS_SUBJECT_WILDCARD.to_owned()],
                retention: stream::RetentionPolicy::WorkQueue,
                ..Default::default()
            })
            .await
            .map_err(|e| BusError::Jetstream(e.to_string()))?;
        self.js
            .create_or_update_stream(stream::Config {
                name: subjects::EVENTS_STREAM.to_owned(),
                subjects: vec![subjects::EVENTS_SUBJECT_WILDCARD.to_owned()],
                ..Default::default()
            })
            .await
            .map_err(|e| BusError::Jetstream(e.to_string()))?;
        Ok(())
    }

    /// Publish a scan task to its provider's queue for the run's scan mode.
    ///
    /// # Errors
    /// [`BusError`] on serialization or publish failure.
    pub async fn publish_task(&self, msg: &ScanTaskMessage) -> Result<(), BusError> {
        let subject = subjects::task_subject(&msg.provider_slug, msg.mode);
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

    /// Open (or reuse) the worker pool's durable pull consumer for **one provider's tasks
    /// in one scan mode**.
    ///
    /// One consumer per provider is what makes the queue fair: each provider gets its own
    /// FIFO lane, and a worker chooses which lane to serve next instead of taking whatever
    /// the stream happens to hold at its head. With a single wildcard consumer, a full
    /// catalogue scan of one provider — which fans out into one task per series — is served
    /// to completion before any other provider's first task is even looked at.
    ///
    /// Splitting each provider again by scan mode is what makes the queue *prioritisable*:
    /// the worker can drain every fast lane before looking at a full one.
    ///
    /// The full-scan lane also binds [`subjects::legacy_task_subject`], the untiered subject
    /// tasks were published to before the split. It is the conservative side to put it on —
    /// an old task is backfill-grade work, and folding it in here means the upgrade strands
    /// nothing rather than leaving messages in the stream that no filter can reach.
    ///
    /// # Errors
    /// [`BusError::Jetstream`] on failure, including a `slug` that is not a legal subject
    /// token — see [`subjects::is_valid_provider_slug`].
    pub async fn provider_task_consumer(
        &self,
        provider_slug: &str,
        mode: ScanMode,
    ) -> Result<PullConsumer, BusError> {
        if !subjects::is_valid_provider_slug(provider_slug) {
            return Err(BusError::Jetstream(format!(
                "provider slug {provider_slug:?} is not a legal subject token"
            )));
        }
        let mut filters = vec![subjects::task_subject(provider_slug, mode)];
        if mode == ScanMode::Full {
            filters.push(subjects::legacy_task_subject(provider_slug));
        }
        self.durable_consumer(
            subjects::TASKS_STREAM,
            &subjects::worker_consumer(provider_slug, mode),
            filters,
            TASK_ACK_WAIT,
        )
        .await
    }

    /// The lanes that already have a worker consumer on the tasks stream.
    ///
    /// The worker builds its lanes from the provider table, but the stream outlives any one
    /// row: tasks published for a provider that has since been renamed or deleted are still
    /// in the stream, and only a lane with the matching filter can drain them. Folding these
    /// lanes into the set keeps those tasks from being stranded forever.
    ///
    /// # Errors
    /// [`BusError::Jetstream`] if the stream or its consumer list cannot be read.
    pub async fn task_consumer_lanes(&self) -> Result<Vec<(ScanMode, String)>, BusError> {
        let stream = self
            .js
            .get_stream(subjects::TASKS_STREAM)
            .await
            .map_err(|e| BusError::Jetstream(e.to_string()))?;
        let mut names = stream.consumer_names();
        let mut lanes = Vec::new();
        while let Some(name) = names.next().await {
            let name = name.map_err(|e| BusError::Jetstream(e.to_string()))?;
            if let Some((mode, slug)) = subjects::worker_consumer_lane(&name) {
                lanes.push((mode, slug.to_owned()));
            }
        }
        Ok(lanes)
    }

    /// Delete the pre-fairness wildcard task consumer, if it is still present.
    ///
    /// A work-queue stream refuses a consumer whose filter subject overlaps an existing
    /// one, and `scan.tasks.*` overlaps every per-provider filter — so on the first start
    /// after the upgrade, this removal is what lets the lanes be created at all. Nothing is
    /// lost by it: work-queue retention drops a message when it is *acked*, not when a
    /// consumer goes away, so everything still pending is picked up by the new lanes.
    ///
    /// Idempotent — a missing consumer is the expected steady state.
    ///
    /// # Errors
    /// [`BusError::Jetstream`] if the stream cannot be read or the delete fails for a reason
    /// other than the consumer already being gone.
    pub async fn retire_wildcard_task_consumer(&self) -> Result<(), BusError> {
        let stream = self
            .js
            .get_stream(subjects::TASKS_STREAM)
            .await
            .map_err(|e| BusError::Jetstream(e.to_string()))?;
        match stream
            .delete_consumer(subjects::LEGACY_WILDCARD_WORKER_CONSUMER)
            .await
        {
            Ok(_) => {
                tracing::info!(
                    consumer = subjects::LEGACY_WILDCARD_WORKER_CONSUMER,
                    "removed the wildcard task consumer; scan tasks are now served per provider"
                );
                Ok(())
            }
            Err(e) if is_consumer_not_found(&e) => Ok(()),
            Err(e) => Err(BusError::Jetstream(e.to_string())),
        }
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
            vec![filter_subject.to_owned()],
            Duration::from_secs(30),
        )
        .await
    }

    /// Open (or reuse) a durable pull consumer over one or more subjects.
    ///
    /// A single filter goes in `filter_subject` and several in `filter_subjects`; the two
    /// fields are mutually exclusive on the wire, and the single-subject form is what every
    /// server version understands.
    async fn durable_consumer(
        &self,
        stream_name: &str,
        durable: &str,
        mut filters: Vec<String>,
        ack_wait: Duration,
    ) -> Result<PullConsumer, BusError> {
        let stream = self
            .js
            .get_stream(stream_name)
            .await
            .map_err(|e| BusError::Jetstream(e.to_string()))?;
        let (filter_subject, filter_subjects) = if filters.len() == 1 {
            (filters.remove(0), Vec::new())
        } else {
            (String::new(), filters)
        };
        let consumer = stream
            .get_or_create_consumer(
                durable,
                pull::Config {
                    durable_name: Some(durable.to_owned()),
                    filter_subject,
                    filter_subjects,
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

/// What a handler wants done with a message that did not succeed.
///
/// Returned rather than decided inside [`consume`], because only the handler's own layer can
/// tell "the provider blocked us, try again in five minutes" from "this markup will fail
/// identically forever".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Disposition {
    /// Done with this message; settle it.
    Ack,
    /// Hand it back for redelivery, subject to [`ConsumePolicy::max_deliveries`].
    Retry,
}

/// How [`consume`] treats a handler failure.
///
/// The delivery *count* is the budget, not the number of retries, and the two differ by one.
/// [`consume`] hands a message back only while `delivery_count < max_deliveries`, so
/// `max_deliveries: 1` — the [`Default`] — means the first failure is also the last, and a
/// handler returning [`Disposition::Retry`] under it is settled immediately.
///
/// ```
/// use std::time::Duration;
/// use tankovault_bus::{ConsumePolicy, TASK_ACK_HEARTBEAT, TASK_ACK_WAIT};
///
/// // What the worker wants: three attempts with a widening gap.
/// let worker = ConsumePolicy {
///     max_deliveries: 3,
///     backoff: |deliveries| Duration::from_secs(30 * deliveries),
///     heartbeat: Some(TASK_ACK_HEARTBEAT),
/// };
/// assert_eq!((worker.backoff)(1), Duration::from_secs(30));
/// assert_eq!((worker.backoff)(3), Duration::from_secs(90));
///
/// // The default retries *zero* times. This reads like a disabled policy and is the right
/// // default: a handler whose failures are not time-fixable (a malformed payload, a deleted
/// // row) gains nothing from redelivery except occupying the consumer.
/// assert_eq!(ConsumePolicy::default().max_deliveries, 1);
/// assert!(ConsumePolicy::default().heartbeat.is_none());
///
/// // The heartbeat has to be a fraction of the ack wait, not merely smaller than it: one lost
/// // extension must not let the deadline lapse while the handler is still running, which is
/// // what would turn a slow scan into a duplicate one.
/// assert!(TASK_ACK_HEARTBEAT * 2 < TASK_ACK_WAIT);
/// ```
pub struct ConsumePolicy {
    /// Deliveries after which a retryable failure is settled anyway, so one poisoned message
    /// cannot occupy a consumer forever.
    pub max_deliveries: u64,
    /// Delay before redelivery, given the current delivery count.
    pub backoff: fn(u64) -> Duration,
    /// Extend the redelivery deadline every this often while the handler runs. `None` for
    /// handlers that always finish well inside [`TASK_ACK_WAIT`].
    pub heartbeat: Option<Duration>,
}

impl Default for ConsumePolicy {
    /// One delivery, no retry: what a handler whose failures are not time-fixable wants.
    fn default() -> Self {
        Self {
            max_deliveries: 1,
            backoff: |_| Duration::from_secs(60),
            heartbeat: None,
        }
    }
}

/// Drive a durable pull consumer until `shutdown` is cancelled or the stream ends.
///
/// This exists because the same loop was hand-rolled three times with three different
/// meanings, and one of them was wrong: the notifier acked a message whose fan-out had
/// **failed**, which is at-most-once delivery — a notification lost with a `warn!` as its only
/// trace — while the control-plane's aggregator had no shutdown arm at all and could not drain
/// on `SIGTERM`. Delivery semantics are not something three call sites should each decide.
///
/// The contract:
/// - Cancellation is checked only *between* messages, never mid-handler. Being killed between
///   the work and the ack is precisely what redelivery is for, but it is still a duplicate,
///   so the message in hand is always finished first.
/// - A handler returning `Ok(Disposition::Ack)` settles the message; `Ok(Disposition::Retry)`
///   or `Err(_)` hands it back with [`retry_later`] until `max_deliveries`, then settles it.
/// - An undecodable payload is dropped and acked. It will never decode, so redelivering it
///   only blocks the consumer.
/// - A failed ack is logged, not fatal: the stream redelivers, and handlers are expected to be
///   idempotent, which is the same assumption every one of these call sites already made.
///
/// # Errors
/// [`BusError::Jetstream`] if the message stream itself cannot be opened.
pub async fn consume<T, F, Fut>(
    consumer: BrokerConsumer,
    shutdown: tokio_util::sync::CancellationToken,
    policy: ConsumePolicy,
    what: &str,
    handler: F,
) -> Result<(), BusError>
where
    T: serde::de::DeserializeOwned,
    F: Fn(T, BrokerMessage) -> Fut,
    Fut: std::future::Future<Output = anyhow::Result<Disposition>>,
{
    let mut messages = consumer
        .messages()
        .await
        .map_err(|e| BusError::Jetstream(e.to_string()))?;
    tracing::info!(subject = what, "consuming");

    loop {
        let next = tokio::select! {
            () = shutdown.cancelled() => {
                tracing::info!(subject = what, "consumer stopping");
                return Ok(());
            }
            next = messages.next() => match next {
                Some(next) => next,
                None => break,
            },
        };
        let msg = match next {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!(subject = what, error = %e, "error pulling message");
                continue;
            }
        };

        let Ok(decoded) = serde_json::from_slice::<T>(&msg.payload) else {
            tracing::warn!(subject = what, "undecodable message; dropping");
            ack_or_warn(&msg, what).await;
            continue;
        };

        let deliveries = delivery_count(&msg);
        let outcome = match policy.heartbeat {
            Some(every) => with_ack_heartbeat(&msg, every, handler(decoded, msg.clone())).await,
            None => handler(decoded, msg.clone()).await,
        };

        let retry = match outcome {
            Ok(Disposition::Ack) => false,
            Ok(Disposition::Retry) => true,
            Err(e) => {
                tracing::warn!(subject = what, error = %e, deliveries, "handler failed");
                true
            }
        };

        if retry && deliveries < policy.max_deliveries {
            let delay = (policy.backoff)(deliveries);
            if let Err(e) = retry_later(&msg, delay).await {
                tracing::warn!(
                    subject = what,
                    error = %e,
                    "could not requeue; it will be redelivered when the ack deadline lapses"
                );
            }
            continue;
        }
        if retry {
            tracing::warn!(
                subject = what,
                deliveries,
                "giving up after {} deliveries; settling the message",
                policy.max_deliveries
            );
        }
        ack_or_warn(&msg, what).await;
    }
    Ok(())
}

/// Settle `msg`, logging rather than failing if the ack does not land.
async fn ack_or_warn(msg: &jetstream::Message, what: &str) {
    if let Err(e) = msg.ack().await {
        tracing::warn!(subject = what, error = %e, "failed to ack message");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_policy_does_not_retry() {
        // A caller that has not thought about redelivery gets at-most-one-attempt, not an
        // accidental infinite requeue.
        let policy = ConsumePolicy::default();
        assert_eq!(policy.max_deliveries, 1);
        assert!(policy.heartbeat.is_none());
    }

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
