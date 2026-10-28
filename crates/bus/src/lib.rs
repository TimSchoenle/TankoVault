//! Owns the `JetStream` plumbing shared by the control-plane, workers and notifier: connecting,
//! provisioning streams, publishing tasks and events, and opening pull consumers, all keyed off
//! [`tankovault_contracts::subjects`].
//!
//! It is also where a distributed trace crosses the broker. Every publish carries
//! [`TRACE_HEADER`] and every delivery continues it ([`trace_of`], and the span [`consume`]
//! opens per message), so a scan task runs under the sweep that queued it. Doing this here,
//! once, means a new subject inherits the behaviour without each publisher remembering it.

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
use tracing::Instrument as _;
use uuid::Uuid;

/// A message as it arrives from the broker: the serialized payload plus its ack handle.
///
/// Re-exported so services need no direct `async-nats` dependency.
pub use async_nats::jetstream::Message as BrokerMessage;

/// A durable pull consumer, re-exported for the same reason as [`BrokerMessage`].
pub use async_nats::jetstream::consumer::PullConsumer as BrokerConsumer;

/// The redelivery deadline for a claimed scan task.
///
/// It bounds how long a worker may go **silent**, not how long a task may take: see
/// [`with_ack_heartbeat`]. It is kept short so a crashed worker's task is redelivered quickly.
pub const TASK_ACK_WAIT: Duration = Duration::from_secs(300);

/// Returns the tasks stream config: a work queue, so a task is removed when a worker acks it.
///
/// It has no age or size limit on purpose. The reconciler compares a lane's open `scan_tasks`
/// rows against what the broker holds, and a limit that silently dropped queued tasks would turn
/// a long backlog into a republish storm.
fn tasks_stream_config() -> stream::Config {
    stream::Config {
        name: subjects::TASKS_STREAM.to_owned(),
        subjects: vec![subjects::TASKS_SUBJECT_WILDCARD.to_owned()],
        retention: stream::RetentionPolicy::WorkQueue,
        ..Default::default()
    }
}

/// Returns the events stream config: an event is kept until every consumer bound to its subject
/// has acked it, and never longer than `max_age`.
///
/// `max_age` is the backstop, not the cleanup. It bounds what `Interest` retention leaves
/// behind, such as events for a consumer that is down or retired but never deleted. A notifier
/// down for longer than `max_age` loses those chapter notifications. An event published while no
/// consumer is bound to its subject is dropped at once, so on a fresh installation the events
/// published before the notifier first creates its consumer are lost.
fn events_stream_config(max_age: Duration) -> stream::Config {
    stream::Config {
        name: subjects::EVENTS_STREAM.to_owned(),
        subjects: vec![subjects::EVENTS_SUBJECT_WILDCARD.to_owned()],
        retention: stream::RetentionPolicy::Interest,
        max_age,
        ..Default::default()
    }
}

/// How often an in-flight task reports progress.
///
/// It is derived from [`TASK_ACK_WAIT`], so tightening the deadline cannot leave the heartbeat
/// too slow and cause mid-task redelivery.
pub const TASK_ACK_HEARTBEAT: Duration = Duration::from_secs(TASK_ACK_WAIT.as_secs() / 5);

/// Runs `work` while holding off redelivery of `msg`.
///
/// `JetStream` redelivers an unacked message after `ack_wait`. For work that runs longer, an
/// [`AckKind::Progress`] resets the timer; raising the deadline instead would also delay crash
/// recovery. A failed heartbeat is logged, not fatal: idempotent writes are the correctness
/// backstop.
///
/// # Panics
/// If `every` is zero, which [`tokio::time::interval`] refuses.
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

/// Hands `msg` back to the stream for redelivery after `delay`, instead of settling it.
///
/// This is the broker half of retrying a task that failed for a reason time can fix: an
/// unsolved challenge, a rate-limited provider, a restarting solver. Acking such a task would
/// drop the series from the run.
///
/// The consumer, not the stream, bounds the attempts: callers check [`delivery_count`] and
/// settle the message once retrying stops being worthwhile. If the negative ack fails, the
/// message stays unsettled and is redelivered once the ack deadline lapses.
///
/// # Errors
/// [`BusError::Jetstream`].
pub async fn retry_later(msg: &jetstream::Message, delay: Duration) -> Result<(), BusError> {
    msg.ack_with(AckKind::Nak(Some(delay)))
        .await
        .map_err(|e| BusError::Jetstream(e.to_string()))
}

/// Returns how many times this message has been delivered, including the current delivery.
///
/// Falls back to `1` when the stream metadata cannot be read. A message whose metadata is never
/// readable therefore always reads as a first attempt, and [`consume`] retries it without bound.
#[must_use]
pub fn delivery_count(msg: &jetstream::Message) -> u64 {
    msg.info()
        .ok()
        .and_then(|info| u64::try_from(info.delivered).ok())
        .unwrap_or(1)
}

/// Returns whether a consumer operation failed because the consumer does not exist.
///
/// `async-nats` reports this as a `JetStream` API error code, not a distinct error kind, so the
/// check reads the code. It is the one failure an idempotent delete treats as success.
fn is_consumer_not_found(err: &stream::ConsumerError) -> bool {
    matches!(
        err.kind(),
        stream::ConsumerErrorKind::JetStream(e)
            if e.error_code() == jetstream::ErrorCode::CONSUMER_NOT_FOUND
    )
}

/// The header carrying a message's trace, so a consumed task continues the request that
/// queued it.
///
/// The name is Sentry's: `sentry-tracing` and every other Sentry SDK read it, and the value is
/// opaque here. It is written on every publish and read on every delivery.
pub const TRACE_HEADER: &str = "sentry-trace";

/// Returns the trace this message belongs to, or `None` for one published with tracing off.
///
/// Public because the worker has its own fair-share queue instead of [`consume`], and still
/// has to continue the trace.
#[must_use]
pub fn trace_of(msg: &BrokerMessage) -> Option<&str> {
    trace_in(msg.headers.as_ref())
}

/// Reads [`TRACE_HEADER`] from a header map, so [`trace_of`] is testable without a broker.
fn trace_in(headers: Option<&async_nats::HeaderMap>) -> Option<&str> {
    headers?
        .get(TRACE_HEADER)
        .map(async_nats::HeaderValue::as_str)
}

/// Returns the outbound trace headers, or `None` when there is no trace to carry.
///
/// Never an empty map: a message published with headers goes on the wire as `HPUB` with a
/// `NATS/1.0` block, so an empty map would change every message in a deployment without
/// Sentry.
fn outbound_headers() -> Option<async_nats::HeaderMap> {
    let mut headers: Option<async_nats::HeaderMap> = None;
    for (name, value) in tankovault_service::trace_headers() {
        headers
            .get_or_insert_with(async_nats::HeaderMap::new)
            .insert(name, value);
    }
    headers
}

/// Holds a `JetStream` context and the underlying core-NATS client.
///
/// Durable task and event traffic goes through `JetStream`. Best-effort live pushes, the
/// per-user notifications, use the core client so they are never retained for a disconnected
/// subscriber.
#[derive(Clone)]
pub struct Bus {
    client: async_nats::Client,
    js: jetstream::Context,
}

/// Errors from the broker client.
#[derive(Debug, Error)]
pub enum BusError {
    /// The connection never came up: an unreachable address, a rejected credential, or a
    /// refused TLS handshake.
    #[error("nats connect error: {0}")]
    Connect(String),
    /// `JetStream` refused the operation on a live connection, most often because the
    /// subject's stream has not been created.
    #[error("jetstream error: {0}")]
    Jetstream(String),
    /// A core-NATS publish or subscribe failed on a live connection.
    ///
    /// Core NATS carries only ephemeral traffic, so nothing durable was lost.
    #[error("nats error: {0}")]
    Nats(String),
    /// A payload could not be encoded to or decoded from JSON: a contract mismatch between
    /// two services, not a broker fault.
    #[error("serialization error: {0}")]
    Serde(#[from] serde_json::Error),
}

impl Bus {
    /// Connects to NATS and builds a `JetStream` context.
    ///
    /// `url` is a [`SecretString`] because `nats://user:pass@host` carries a per-account
    /// credential. A separate account per service is what stops `notifier` from publishing
    /// scan tasks; the accounts are NATS server configuration, which this client cannot assert.
    ///
    /// `tls` is present under `internal.identity = "mtls"`. The connection is then mutually
    /// authenticated with the HTTP tier's material, and TLS is **required**, not just offered:
    /// otherwise a server that does not ask for TLS gets a plaintext connection carrying every
    /// user notification.
    ///
    /// # Errors
    /// [`BusError::Connect`] if the server is unreachable or the TLS material is unusable.
    pub async fn connect(
        url: &SecretString,
        tls: Option<&tankovault_config::ResolvedTls>,
    ) -> Result<Self, BusError> {
        let mut options = async_nats::ConnectOptions::new();
        if let Some(paths) = tls {
            options = options
                .require_tls(true)
                .add_client_certificate(paths.cert.clone(), paths.key.clone())
                .add_root_certificates(paths.ca.clone());
        }

        let client = options
            .connect(url.expose_secret())
            .await
            .map_err(|e| BusError::Connect(e.to_string()))?;
        Ok(Self {
            js: jetstream::new(client.clone()),
            client,
        })
    }

    /// Round-trips the broker, for a readiness probe.
    ///
    /// It flushes instead of reading the client's cached state: the client reconnects in the
    /// background, so the state can read `Connected` while the server is unreachable. A flush
    /// waits for the server to acknowledge.
    ///
    /// # Errors
    /// [`BusError::Nats`] if the round trip fails or the connection is down.
    pub async fn ping(&self) -> Result<(), BusError> {
        self.client
            .flush()
            .await
            .map_err(|e| BusError::Nats(e.to_string()))
    }

    /// Returns the underlying `JetStream` context.
    #[must_use]
    pub fn jetstream(&self) -> &jetstream::Context {
        &self.js
    }

    /// Creates the tasks and events streams, or brings existing ones into line with their
    /// config.
    ///
    /// It uses `create_or_update_stream` because `get_or_create_stream` returns an existing
    /// stream untouched: a changed subject binding or `max_age` would never reach an installation
    /// that already has the stream. Every service that calls this rewrites `events_max_age`, so
    /// they must all be given the same value.
    ///
    /// # Errors
    /// [`BusError::Jetstream`] for a zero `events_max_age`, or if the server refuses the update.
    /// `JetStream` refuses a retention change into or out of `WorkQueue`, so moving an existing
    /// stream to another retention policy needs a new stream.
    pub async fn ensure_streams(&self, events_max_age: Duration) -> Result<(), BusError> {
        // Zero is JetStream's "unlimited", so it is refused instead of unbounding the stream.
        if events_max_age.is_zero() {
            return Err(BusError::Jetstream(
                "events stream max age must be greater than zero".to_owned(),
            ));
        }
        self.js
            .create_or_update_stream(tasks_stream_config())
            .await
            .map_err(|e| BusError::Jetstream(e.to_string()))?;
        self.js
            .create_or_update_stream(events_stream_config(events_max_age))
            .await
            .map_err(|e| BusError::Jetstream(e.to_string()))?;
        Ok(())
    }

    /// Publishes a scan task to its provider's lane for the run's scan mode.
    ///
    /// # Errors
    /// [`BusError::Serde`] if the task cannot be serialised, [`BusError::Jetstream`] if the publish
    /// fails.
    pub async fn publish_task(&self, msg: &ScanTaskMessage) -> Result<(), BusError> {
        let subject = subjects::task_subject(&msg.provider_slug, msg.mode);
        self.publish(subject, msg).await
    }

    /// Publishes a progress event.
    ///
    /// # Errors
    /// [`BusError::Serde`] or [`BusError::Jetstream`].
    pub async fn publish_progress(&self, event: &ProgressEvent) -> Result<(), BusError> {
        self.publish(subjects::PROGRESS_SUBJECT.to_owned(), event)
            .await
    }

    /// Publishes a new-chapter event.
    ///
    /// # Errors
    /// [`BusError::Serde`] or [`BusError::Jetstream`].
    pub async fn publish_chapter(&self, event: &ChapterDiscovered) -> Result<(), BusError> {
        self.publish(subjects::CHAPTER_DISCOVERED_SUBJECT.to_owned(), event)
            .await
    }

    /// Publishes a provider-state-change event.
    ///
    /// # Errors
    /// [`BusError::Serde`] or [`BusError::Jetstream`].
    pub async fn publish_provider_state(
        &self,
        event: &ProviderStateChanged,
    ) -> Result<(), BusError> {
        self.publish(subjects::PROVIDER_STATE_SUBJECT.to_owned(), event)
            .await
    }

    /// Publishes a live per-user notification over **core NATS**, fire-and-forget.
    ///
    /// It awaits no broker ack and is not retained: if no API replica holds an SSE connection
    /// for the user, the message is dropped. The durable record is the `notifications` row, so
    /// a drop loses only a live badge update.
    ///
    /// # Errors
    /// [`BusError::Serde`] if encoding fails, [`BusError::Nats`] if the client cannot hand the
    /// message to the connection.
    pub async fn publish_user_notification(
        &self,
        event: &UserNotification,
    ) -> Result<(), BusError> {
        let subject = subjects::user_notify_subject(event.user_id.as_uuid());
        let payload = bytes::Bytes::from(serde_json::to_vec(event)?);
        match outbound_headers() {
            Some(headers) => {
                self.client
                    .publish_with_headers(subject, headers, payload)
                    .await
            }
            None => self.client.publish(subject, payload).await,
        }
        .map_err(|e| BusError::Nats(e.to_string()))
    }

    /// Subscribes over core NATS to one user's live-notification subject.
    ///
    /// The subscriber yields raw messages whose payloads decode to [`UserNotification`], and
    /// unsubscribes when dropped, such as when the SSE connection closes.
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
        let payload = bytes::Bytes::from(bytes);
        let published = match outbound_headers() {
            Some(headers) => {
                self.js
                    .publish_with_headers(subject, headers, payload)
                    .await
            }
            None => self.js.publish(subject, payload).await,
        };
        let ack = published.map_err(|e| BusError::Jetstream(e.to_string()))?;
        ack.await.map_err(|e| BusError::Jetstream(e.to_string()))?;
        Ok(())
    }

    /// Opens or reuses the worker pool's durable pull consumer for **one provider's tasks in
    /// one scan mode**.
    ///
    /// A consumer per provider makes the queue fair: each provider gets its own FIFO lane, and a
    /// worker chooses which lane to serve next. A single wildcard consumer would serve one
    /// provider's full catalogue scan, one task per series, before any other provider's first
    /// task.
    ///
    /// Splitting each provider by scan mode makes the queue prioritisable: the worker can drain
    /// every fast lane before a full one.
    ///
    /// The full-scan lane also binds [`subjects::legacy_task_subject`], the untiered subject, so
    /// a task published there is still reachable. It is backfill-grade work, which is why it
    /// joins the full lane.
    ///
    /// # Errors
    /// [`BusError::Jetstream`], including for a slug that fails
    /// [`subjects::is_valid_provider_slug`].
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

    /// Returns the lanes that already have a worker consumer on the tasks stream.
    ///
    /// The worker builds its lanes from the provider table, but the stream outlives any one
    /// row: tasks for a renamed or deleted provider stay in the stream, and only a lane with the
    /// matching filter can drain them. Adding these lanes keeps those tasks from being stranded.
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

    /// Returns how many messages the broker holds for one provider lane: waiting to be pulled,
    /// plus claimed and not yet acked.
    ///
    /// The full lane's legacy filter is included, because both subjects sit on one consumer.
    /// This is the broker half of reconciling dispatch against `scan_tasks`. The broker can say
    /// how many messages a lane holds, never which, so the caller learns the size of a deficit
    /// and chooses which rows to attribute it to.
    ///
    /// `None` means the lane has no consumer, which must not be read as an empty lane. The
    /// worker pool opens its lanes on start, so before it first runs every lane is absent, and
    /// reading that as "the broker holds nothing" would republish every open task.
    ///
    /// # Errors
    /// [`BusError::Jetstream`] if the stream or the consumer cannot be read for a reason other than
    /// the consumer being absent.
    pub async fn lane_backlog(
        &self,
        provider_slug: &str,
        mode: ScanMode,
    ) -> Result<Option<u64>, BusError> {
        let stream = self
            .js
            .get_stream(subjects::TASKS_STREAM)
            .await
            .map_err(|e| BusError::Jetstream(e.to_string()))?;
        match stream
            .consumer_info(subjects::worker_consumer(provider_slug, mode))
            .await
        {
            Ok(info) => Ok(Some(info.num_pending.saturating_add(
                u64::try_from(info.num_ack_pending).unwrap_or(u64::MAX),
            ))),
            Err(e)
                if matches!(
                    e.kind(),
                    jetstream::context::ConsumerInfoErrorKind::NotFound
                ) =>
            {
                Ok(None)
            }
            Err(e) => Err(BusError::Jetstream(e.to_string())),
        }
    }

    /// Deletes the wildcard task consumer, if present.
    ///
    /// A work-queue stream refuses a consumer whose filter overlaps an existing one, and
    /// `scan.tasks.*` overlaps the untiered subject every full-scan lane binds, so those lanes
    /// cannot be created while it exists. Nothing is lost: work-queue retention drops a message
    /// when it is acked, not when a consumer goes away, so the lanes pick up everything still
    /// pending.
    ///
    /// Idempotent: a missing consumer is the steady state.
    ///
    /// # Errors
    /// [`BusError::Jetstream`] if the stream cannot be read or the delete fails for a reason other
    /// than the consumer already being gone.
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

    /// Opens or reuses a durable pull consumer on the events stream, filtered to
    /// `filter_subject`.
    ///
    /// # Errors
    /// [`BusError::Jetstream`].
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

    /// Opens or reuses a durable pull consumer over one or more subjects.
    ///
    /// A single filter goes in `filter_subject` and several in `filter_subjects`. The two fields
    /// are mutually exclusive on the wire, and every server version understands the single
    /// form.
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

/// Says what a handler wants done with a message.
///
/// The handler returns it because only its own layer can tell "the provider blocked us, try
/// again in five minutes" from "this markup will fail identically forever".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Disposition {
    /// Settles the message.
    Ack,
    /// Hands the message back for redelivery, subject to [`ConsumePolicy::max_deliveries`].
    Retry,
}

/// Sets how [`consume`] treats a handler failure.
///
/// The budget is the delivery count, not the number of retries, and the two differ by one.
/// [`consume`] hands a message back only while `delivery_count < max_deliveries`, so the
/// [`Default`] of `max_deliveries: 1` makes the first failure the last: a
/// [`Disposition::Retry`] under it is settled immediately.
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
/// // The default retries zero times: a handler whose failures time cannot fix, such as a
/// // malformed payload or a deleted row, gains nothing from redelivery.
/// assert_eq!(ConsumePolicy::default().max_deliveries, 1);
/// assert!(ConsumePolicy::default().heartbeat.is_none());
///
/// // The heartbeat is a fraction of the ack wait, so one lost extension cannot let the
/// // deadline lapse while the handler runs and turn a slow scan into a duplicate.
/// assert!(TASK_ACK_HEARTBEAT * 2 < TASK_ACK_WAIT);
/// ```
pub struct ConsumePolicy {
    /// The deliveries after which a retryable failure is settled anyway, so one poisoned
    /// message cannot occupy a consumer forever.
    pub max_deliveries: u64,
    /// The delay before redelivery, given the current delivery count.
    pub backoff: fn(u64) -> Duration,
    /// How often to extend the redelivery deadline while the handler runs.
    ///
    /// `None` for handlers that always finish well inside the consumer's ack wait.
    pub heartbeat: Option<Duration>,
}

impl Default for ConsumePolicy {
    /// Returns one delivery and no retry, for handlers whose failures time cannot fix.
    fn default() -> Self {
        Self {
            max_deliveries: 1,
            backoff: |_| Duration::from_secs(60),
            heartbeat: None,
        }
    }
}

/// Drives a durable pull consumer until `shutdown` is cancelled or the stream ends.
///
/// Delivery semantics live here, not at each call site:
/// - Cancellation is checked only between messages, never mid-handler. A kill between the work
///   and the ack is what redelivery covers, but it still duplicates work, so the message in
///   hand is always finished first.
/// - `Ok(Disposition::Ack)` settles the message. `Ok(Disposition::Retry)` or `Err(_)` hands it
///   back with [`retry_later`] until `max_deliveries`, then settles it.
/// - An undecodable payload is acked and dropped. It will never decode, so redelivering it only
///   blocks the consumer.
/// - A failed ack is logged, not fatal: the stream redelivers, and handlers are idempotent.
///
/// # Errors
/// [`BusError::Jetstream`] if the message stream cannot be opened.
///
/// # Panics
/// On the first message if `policy.heartbeat` is `Some(Duration::ZERO)`; see
/// [`with_ack_heartbeat`].
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
        // One span per delivery, continuing the publisher's trace. `sentry-tracing` turns a
        // root span into a transaction, and nothing else is on the stack here, so the work
        // appears under the request that queued it. The span is created with Sentry off too:
        // the handler's log lines gain the subject and the delivery count.
        let span = if let Some(trace) = trace_of(&msg) {
            tracing::info_span!(
                "bus_delivery",
                "sentry.name" = what,
                "sentry.op" = "queue.process",
                "sentry.trace" = trace,
                subject = what,
                deliveries,
            )
        } else {
            tracing::info_span!(
                "bus_delivery",
                "sentry.name" = what,
                "sentry.op" = "queue.process",
                subject = what,
                deliveries,
            )
        };
        let work = handler(decoded, msg.clone()).instrument(span);
        let outcome = match policy.heartbeat {
            Some(every) => with_ack_heartbeat(&msg, every, work).await,
            None => work.await,
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

/// Settles `msg`, logging instead of failing if the ack does not land.
async fn ack_or_warn(msg: &jetstream::Message, what: &str) {
    if let Err(e) = msg.ack().await {
        tracing::warn!(subject = what, error = %e, "failed to ack message");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A deployment with Sentry off publishes no header block at all.
    ///
    /// An empty `HeaderMap` would still send `HPUB` with a `NATS/1.0` block, so "no trace" has
    /// to mean no headers, not empty ones.
    #[test]
    fn no_trace_means_no_header_block_at_all() {
        assert!(outbound_headers().is_none());
    }

    #[test]
    fn the_trace_header_is_read_back_off_a_delivery() {
        assert_eq!(trace_in(None), None);

        let empty = async_nats::HeaderMap::new();
        assert_eq!(trace_in(Some(&empty)), None);

        let mut carried = async_nats::HeaderMap::new();
        carried.insert(
            TRACE_HEADER,
            "d49d9bf66f13450b81f65bc51cf49c03-a1b2c3d4e5f60718-1",
        );
        assert_eq!(
            trace_in(Some(&carried)),
            Some("d49d9bf66f13450b81f65bc51cf49c03-a1b2c3d4e5f60718-1")
        );
    }

    /// The events stream drops consumed events and keeps nothing forever.
    ///
    /// The bug: default `Limits` retention with no bound keeps every event ever published on
    /// disk, and the broker restores all of them on start.
    #[test]
    fn the_events_stream_drops_consumed_events_and_is_age_bounded() {
        let max_age = Duration::from_secs(3600);
        let events = events_stream_config(max_age);
        assert_eq!(events.retention, stream::RetentionPolicy::Interest);
        assert_eq!(events.max_age, max_age);
    }

    /// The tasks stream stays a work queue with no age limit, because the reconciler reads its
    /// per-lane backlog as the truth for what is still queued.
    #[test]
    fn the_tasks_stream_is_an_unbounded_work_queue() {
        let tasks = tasks_stream_config();
        assert_eq!(tasks.retention, stream::RetentionPolicy::WorkQueue);
        assert_eq!(tasks.max_age, Duration::ZERO);
    }

    #[test]
    fn the_default_policy_does_not_retry() {
        let policy = ConsumePolicy::default();
        assert_eq!(policy.max_deliveries, 1);
        assert!(policy.heartbeat.is_none());
    }

    #[test]
    fn heartbeat_fits_comfortably_inside_the_redelivery_deadline() {
        // A progress ack has to land well before the deadline lapses.
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
