//! # tankovault-bus
//!
//! Thin `JetStream` helpers shared by the control-plane (producer), workers (task
//! consumer), and notifier (event consumer): connect, provision the durable streams,
//! publish tasks/events, and open a durable pull consumer. Subject/stream naming comes
//! from [`tankovault_contracts::subjects`] so producers and consumers cannot drift.

use async_nats::jetstream::{self, consumer::PullConsumer, consumer::pull, stream};
use serde::Serialize;
use std::time::Duration;
use tankovault_contracts::{
    ChapterDiscovered, ProgressEvent, ProviderStateChanged, ScanTaskMessage, UserNotification,
    subjects,
};
use thiserror::Error;
use uuid::Uuid;

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
            // A `catalog_page` task fans out one child per catalogue entry (many DB writes +
            // publishes), so its processing can outlast the JetStream default ack deadline.
            // A generous window prevents mid-processing redelivery (which would re-run the
            // fan-out); the idempotent child insert is the correctness backstop.
            Duration::from_secs(300),
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
