//! # notifier service
//!
//! Consumes `chapter.discovered` events and fans them out to watchers (design §14):
//! - only users watching the series with `notify = true`,
//! - skipping chapters at or below a user's read progress (no spam on rescans),
//! - deduplicated per `(user, series, chapter)` so overlapping providers never double-fire.

#![allow(unreachable_pub)]

mod channels;

use std::sync::Arc;

use channels::{Alert, NotificationChannel};
use futures::StreamExt;
use serde::Deserialize;
use tankovault_bus::Bus;
use tankovault_contracts::{ChapterDiscovered, UserNotification, subjects};
use tankovault_db::PgPool;
use tankovault_domain::Feature;
use tankovault_service::health::PostgresCheck;
use tankovault_service::{FeatureGate, Health, HttpStack, MetricsRegistry, PostgresFlagSource};
use time::OffsetDateTime;

#[derive(Debug, Deserialize)]
struct Config {
    database: tankovault_config::DatabaseConfig,
    nats: tankovault_config::NatsConfig,
    telemetry: tankovault_config::TelemetryConfig,
    #[serde(default)]
    channels: channels::ChannelsConfig,
    #[serde(default = "default_bind")]
    bind_addr: String,
    /// Edge hardening for the ops listener.
    #[serde(default)]
    security: tankovault_config::SecurityConfig,
    /// Prometheus metrics. Togglable; disabling installs no recorder.
    #[serde(default)]
    metrics: tankovault_config::MetricsConfig,
    /// Runtime feature flags — how often this replica re-reads the operator's decisions.
    #[serde(default)]
    features: tankovault_config::FeaturesConfig,
}

fn default_bind() -> String {
    "0.0.0.0:8082".to_owned()
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cfg: Config = tankovault_config::load()?;
    tankovault_service::init_tracing(&cfg.telemetry)?;
    let metrics = MetricsRegistry::install(&cfg.metrics)?;
    let shutdown = tankovault_service::install_shutdown();

    let pool = tankovault_db::connect(
        &cfg.database.url,
        cfg.database.max_connections,
        cfg.database.acquire_timeout_secs,
    )
    .await?;
    let bus = Bus::connect(&cfg.nats.url).await?;
    bus.ensure_streams().await?;

    // Ops listener for orchestrator probes and the metrics scrape, alongside the consumer.
    // Readiness names both dependencies: a notifier that cannot reach Postgres or NATS
    // cannot deliver anything, and previously reported itself healthy regardless.
    let ready_pool = pool.clone();
    let ready_bus = bus.clone();
    let health = Health::builder()
        .check(PostgresCheck::new(ready_pool))
        .check_fn("nats", move || {
            let bus = ready_bus.clone();
            async move { bus.ping().await.map_err(|e| e.to_string()) }
        })
        .build();

    // Serve the metrics scrape on its own port when configured, keeping it off the
    // request-facing listener.
    tankovault_service::spawn_metrics_server(metrics.clone(), shutdown.clone());

    let ops = HttpStack::new(&cfg.security, metrics.clone())
        .apply(axum::Router::new())
        .merge(tankovault_service::ops_router(health, metrics));
    let ops_bind = cfg.bind_addr.clone();
    let ops_shutdown = shutdown.clone();
    tokio::spawn(async move {
        if let Err(e) = tankovault_service::serve(&ops_bind, ops, ops_shutdown).await {
            tracing::error!(error = %e, "ops listener stopped");
        }
    });

    let external = Arc::new(channels::build(&cfg.channels));
    if external.is_empty() {
        tracing::info!("no external notification channels configured (in-app only)");
    } else {
        let names: Vec<&str> = external.iter().map(|c| c.name()).collect();
        tracing::info!(?names, "external notification channels configured");
    }

    // Configuration says which channels *exist*; the flags say which currently deliver. Loaded
    // before the consumer starts so the first event after a restart already respects the
    // operator's decisions.
    let features = FeatureGate::new(Arc::new(PostgresFlagSource::new(pool.clone())));
    features
        .spawn_refresh(cfg.features.refresh_interval(), shutdown.clone())
        .await;

    run_consumer(pool, bus, external, features, shutdown).await
}

async fn run_consumer(
    pool: PgPool,
    bus: Bus,
    channels: Arc<Vec<Box<dyn NotificationChannel>>>,
    features: FeatureGate,
    shutdown: tokio_util::sync::CancellationToken,
) -> anyhow::Result<()> {
    let consumer = bus
        .event_consumer(
            subjects::NOTIFIER_CONSUMER,
            subjects::CHAPTER_DISCOVERED_SUBJECT,
        )
        .await?;
    let mut messages = consumer.messages().await?;
    tracing::info!("notifier consuming chapter.discovered events");

    loop {
        // Finish the message in hand, then stop: acking after the fan-out is what makes
        // redelivery correct, and being killed between the two would drop a notification.
        let next = tokio::select! {
            () = shutdown.cancelled() => {
                tracing::info!("notifier consumer stopping");
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
                tracing::warn!(error = %e, "error pulling event");
                continue;
            }
        };
        match serde_json::from_slice::<ChapterDiscovered>(&msg.payload) {
            Ok(event) => {
                if let Err(e) = fan_out(&pool, &bus, &channels, &features, &event).await {
                    tracing::warn!(error = %e, "fan-out failed");
                }
            }
            Err(e) => tracing::warn!(error = %e, "undecodable event; dropping"),
        }
        if let Err(e) = msg.ack().await {
            tracing::warn!(error = %e, "failed to ack event");
        }
    }
    Ok(())
}

async fn fan_out(
    pool: &PgPool,
    bus: &Bus,
    channels: &[Box<dyn NotificationChannel>],
    features: &FeatureGate,
    event: &ChapterDiscovered,
) -> anyhow::Result<()> {
    // With in-app notifications off, no rows are written and no live push is sent — but the
    // per-watcher walk and the dedup claim still run. The claim is this system's record that a
    // chapter has been *announced*, and it is what keeps the external channels firing once per
    // genuinely-new chapter instead of on every rescan; skipping it would turn a Discord
    // webhook into a duplicate every scan cycle for as long as the flag stayed off.
    let in_app = features.is_enabled(Feature::NotificationsInApp);

    let watchers =
        tankovault_db::repo::tracking::watchers_for_series(pool, event.series_id).await?;
    let mut notified_any = false;
    for watcher in watchers {
        // Don't notify for a chapter the user has already read past (rescan safety).
        if let Some(last_read) = watcher.last_read_number {
            if event.chapter_number <= last_read {
                continue;
            }
        }
        // Dedup across overlapping providers.
        let claimed = tankovault_db::repo::tracking::dedup_claim(
            pool,
            watcher.user_id,
            event.series_id,
            event.chapter_number,
        )
        .await?;
        if !claimed {
            continue;
        }

        // Claimed, so this chapter is new to this watcher whatever happens next.
        notified_any = true;
        if !in_app {
            continue;
        }

        let payload = serde_json::json!({
            "series_id": event.series_id,
            "chapter_number": event.chapter_number,
            "chapter_title": event.chapter_title,
            "chapter_path": event.chapter_path,
            "provider_slug": event.provider_slug,
        });
        let notification_id = tankovault_db::repo::tracking::notification_create(
            pool,
            watcher.user_id,
            "new_chapter",
            &payload,
        )
        .await?;

        // The live push is a separate feature from the durable row: a deployment can keep the
        // notification list while shedding the SSE fan-out under load.
        if features.is_enabled(Feature::NotificationsLive) {
            push_live(pool, bus, watcher.user_id, notification_id, &payload).await;
        }
    }

    // Fire external channels once per genuinely-new chapter (i.e. when at least one
    // watcher got a fresh in-app notification), so rescans never re-alert operators.
    if notified_any && !channels.is_empty() {
        let alert = Alert {
            series_id: event.series_id,
            chapter_number: event.chapter_number,
            chapter_title: event.chapter_title.clone(),
            chapter_path: event.chapter_path.clone(),
            provider_slug: event.provider_slug.clone(),
        };
        dispatch_external(channels, features, &alert).await;
    }
    Ok(())
}

/// Best-effort live push of a freshly-created in-app notification to the user's SSE stream
/// (design §14, §17.4). Carries the user's current unread count so the client can set its
/// badge without a round-trip. A failure here never affects the durable notification row.
async fn push_live(
    pool: &PgPool,
    bus: &Bus,
    user_id: tankovault_domain::UserId,
    notification_id: tankovault_domain::NotificationId,
    payload: &serde_json::Value,
) {
    let unread_count =
        match tankovault_db::repo::tracking::notifications_unread_count(pool, user_id).await {
            Ok(n) => n,
            Err(e) => {
                tracing::warn!(error = %e, "unread-count query failed; skipping live push");
                return;
            }
        };
    let live = UserNotification {
        user_id,
        notification_id: notification_id.as_uuid(),
        kind: "new_chapter".to_owned(),
        payload: payload.clone(),
        created_at: OffsetDateTime::now_utc(),
        unread_count,
    };
    if let Err(e) = bus.publish_user_notification(&live).await {
        tracing::warn!(error = %e, "live notification push failed");
    }
}

/// Best-effort delivery to every *currently enabled* external channel; failures are logged,
/// never fatal.
///
/// A channel switched off is skipped silently at `debug` rather than logged as a problem: it is
/// a deliberate operator decision, and warning about it every chapter would bury the delivery
/// failures this log line exists to surface.
async fn dispatch_external(
    channels: &[Box<dyn NotificationChannel>],
    features: &FeatureGate,
    alert: &Alert,
) {
    for channel in channels {
        if !features.is_enabled(channel.feature()) {
            tracing::debug!(
                channel = channel.name(),
                "channel is switched off; skipping"
            );
            continue;
        }
        if let Err(e) = channel.deliver(alert).await {
            tracing::warn!(channel = channel.name(), error = %e, "external delivery failed");
        }
    }
}
