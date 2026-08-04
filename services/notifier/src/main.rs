//! Consumes `chapter.discovered` events and fans them out to watchers: only users with
//! `notify = true`, skipping already-read chapters, deduplicated per `(user, series,
//! chapter)` so overlapping providers never double-fire.

mod channels;

use std::sync::Arc;
use std::time::Duration;

use channels::{Alert, NotificationChannel};
use serde::Deserialize;
use tankovault_bus::Bus;
use tankovault_contracts::{ChapterDiscovered, UserNotification, subjects};
use tankovault_db::PgPool;
use tankovault_domain::Feature;
use tankovault_service::health::PostgresCheck;
use tankovault_service::{
    CancellationToken, FeatureGate, Health, HttpStack, MetricsRegistry, PostgresFlagSource,
};
use time::OffsetDateTime;

#[derive(Debug, Deserialize)]
struct Config {
    database: tankovault_config::DatabaseConfig,
    nats: tankovault_config::NatsConfig,
    telemetry: tankovault_config::TelemetryConfig,
    #[serde(default)]
    channels: channels::ChannelsConfig,
    /// The shared `TANKOVAULT_EMAIL__*` relay configuration, identical to the API's — must
    /// stay shared, or the envelope-sender policy silently diverges from the API's mail.
    #[serde(default)]
    email: tankovault_config::EmailConfig,
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
    // Runs before config/telemetry: `scratch` images have no shell or wget, so the binary
    // must probe itself for Docker's HEALTHCHECK.
    if tankovault_service::healthcheck::requested() {
        let cfg: Config = tankovault_config::load()?;
        tankovault_service::run_healthcheck_and_exit(&cfg.bind_addr);
    }

    let boot = tankovault_config::load_watched::<Config>()?;
    // Both are process-global and installed once, which is why `telemetry.*` and `metrics.*`
    // are the two blocks a configuration reload cannot apply.
    tankovault_service::init_tracing(&boot.value.telemetry)?;
    let metrics =
        MetricsRegistry::install(&boot.value.metrics, &boot.value.telemetry.service_name)?;
    let shutdown = tankovault_service::install_shutdown();
    // Own port keeps the scrape off the request-facing listener. Outside the reloadable
    // runtime so a reload does not rebind it.
    tankovault_service::spawn_metrics_server(metrics.clone(), shutdown.clone());

    tankovault_service::run_reloading(boot, &shutdown, |cfg, generation| {
        serve_once(cfg, metrics.clone(), generation)
    })
    .await
}

/// Build and run everything a configuration change rebuilds: the pool, the bus connection, the
/// notification channels, the ops listener and the event consumer.
///
/// Returns when `shutdown` is cancelled — by the OS signal, or by the supervisor because the
/// configuration changed and this runtime is being replaced. A rotated Discord or webhook URL
/// takes effect here, on the rebuild, rather than at the next restart.
async fn serve_once(
    cfg: Arc<Config>,
    metrics: MetricsRegistry,
    shutdown: CancellationToken,
) -> anyhow::Result<()> {
    let pool = tankovault_db::connect(
        &cfg.database.url,
        cfg.database.max_connections,
        cfg.database.acquire_timeout_secs,
    )
    .await?;
    tankovault_service::metrics::spawn_pool_sampler(pool.clone(), shutdown.clone());
    let bus = Bus::connect(&cfg.nats.url).await?;
    bus.ensure_streams().await?;

    // Readiness names both dependencies: a notifier that can't reach Postgres or NATS
    // can't deliver anything.
    let ready_pool = pool.clone();
    let ready_bus = bus.clone();
    let health = Health::builder()
        .check(PostgresCheck::new(ready_pool))
        .check_fn("nats", move || {
            let bus = ready_bus.clone();
            async move { bus.ping().await.map_err(|e| e.to_string()) }
        })
        .build();

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

    let external = Arc::new(channels::build(&cfg.channels, &cfg.email));
    if external.is_empty() {
        tracing::info!("no external notification channels configured (in-app only)");
    } else {
        let names: Vec<&str> = external.iter().map(|c| c.name()).collect();
        tracing::info!(?names, "external notification channels configured");
    }

    // Configuration says which channels exist; flags say which currently deliver. Loaded
    // before the consumer starts so the first post-restart event respects them.
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

    // Must retry on failure: acking unconditionally is at-most-once delivery and could
    // lose notifications permanently. The dedup claim inside `fan_out` makes redelivery safe.
    let policy = tankovault_bus::ConsumePolicy {
        max_deliveries: 4,
        // Minutes, not seconds: the failures worth retrying here are a busy database or a
        // third-party webhook, and hammering either is how a blip becomes an outage.
        backoff: |deliveries| match deliveries {
            0 | 1 => Duration::from_secs(30),
            2 => Duration::from_secs(120),
            _ => Duration::from_secs(600),
        },
        // A fan-out over ten thousand watchers can outrun the ack deadline.
        heartbeat: Some(tankovault_bus::TASK_ACK_HEARTBEAT),
    };

    tankovault_bus::consume(
        consumer,
        shutdown,
        policy,
        subjects::CHAPTER_DISCOVERED_SUBJECT,
        move |event: ChapterDiscovered, _msg| {
            let pool = pool.clone();
            let bus = bus.clone();
            let channels = Arc::clone(&channels);
            let features = features.clone();
            async move {
                let started = std::time::Instant::now();
                let result = fan_out(&pool, &bus, &channels, &features, &event).await;
                metrics::histogram!("notification_fanout_duration_seconds")
                    .record(started.elapsed().as_secs_f64());
                let outcome = if result.is_ok() { "ok" } else { "error" };
                metrics::counter!("notification_events_total", "result" => outcome).increment(1);
                result?;
                Ok(tankovault_bus::Disposition::Ack)
            }
        },
    )
    .await?;
    Ok(())
}

async fn fan_out(
    pool: &PgPool,
    bus: &Bus,
    channels: &[Box<dyn NotificationChannel>],
    features: &FeatureGate,
    event: &ChapterDiscovered,
) -> anyhow::Result<()> {
    // In-app off still runs the dedup claim: it's the system's announcement record, and
    // skipping it would make external channels re-fire every rescan.
    let in_app = features.is_enabled(Feature::NotificationsInApp);

    // Three set-based statements for the whole fan-out, not one per watcher — at ten
    // thousand watchers, per-watcher cost ~30 000 round trips for one chapter.
    let watchers =
        tankovault_db::repo::tracking::watchers_for_series(pool, event.series_id).await?;

    // Rescan safety: never notify an already-read chapter. Must use `ReadProgress::covers`,
    // not `chapter_number > last_read_number` — a part release (`152.5`) belongs to the
    // chapter it floors to, so a direct comparison would announce it as new again.
    let unread_by: Vec<tankovault_domain::UserId> = watchers
        .into_iter()
        .filter(|w| {
            w.progress
                .is_none_or(|progress| !progress.covers(event.chapter_number))
        })
        .map(|w| w.user_id)
        .collect();

    // Dedup across overlapping providers: `claimed` is exactly the users this chapter is
    // genuinely new to.
    let claimed = tankovault_db::repo::tracking::dedup_claim_many(
        pool,
        &unread_by,
        event.series_id,
        event.chapter_number,
    )
    .await?;
    let notified_any = !claimed.is_empty();

    if in_app && !claimed.is_empty() {
        // One immutable document for the whole fan-out — only `user_id` varies.
        let payload = serde_json::json!({
            "series_id": event.series_id,
            "chapter_number": event.chapter_number,
            "chapter_title": event.chapter_title,
            "chapter_path": event.chapter_path,
            "provider_slug": event.provider_slug,
        });
        let created = tankovault_db::repo::tracking::notifications_create_many(
            pool,
            &claimed,
            "new_chapter",
            &payload,
        )
        .await?;
        metrics::counter!("notifications_created_total").increment(created.len() as u64);

        // The live push is a separate feature from the durable row: a deployment can keep the
        // notification list while shedding the SSE fan-out under load.
        if features.is_enabled(Feature::NotificationsLive) {
            push_live(pool, bus, &created, &payload).await;
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

/// Best-effort live push of freshly-created in-app notifications to users' SSE streams,
/// each carrying the user's current unread count. Never affects the durable notification
/// rows on failure.
///
/// Counts come from one grouped query for the batch. A miss (a race with a concurrent
/// "mark all read") is treated as `0` rather than panicking.
async fn push_live(
    pool: &PgPool,
    bus: &Bus,
    created: &[(tankovault_domain::UserId, tankovault_domain::NotificationId)],
    payload: &serde_json::Value,
) {
    let users: Vec<tankovault_domain::UserId> = created.iter().map(|(u, _)| *u).collect();
    let counts =
        match tankovault_db::repo::tracking::notifications_unread_counts(pool, &users).await {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(error = %e, "unread-count query failed; skipping live push");
                return;
            }
        };
    let created_at = OffsetDateTime::now_utc();
    // This per-watcher `payload.clone()` is intentional: removing it would need an `Arc`
    // or borrowed field in a published wire DTO — not worth it against a publish that
    // re-serialises the same document anyway.
    for &(user_id, notification_id) in created {
        let live = UserNotification {
            user_id,
            notification_id: notification_id.as_uuid(),
            kind: "new_chapter".to_owned(),
            payload: payload.clone(),
            created_at,
            unread_count: counts.get(&user_id).copied().unwrap_or(0),
        };
        if let Err(e) = bus.publish_user_notification(&live).await {
            tracing::warn!(error = %e, "live notification push failed");
        }
    }
}

/// Best-effort delivery to every *currently enabled* external channel; failures are logged,
/// never fatal.
///
/// A disabled channel is skipped at `debug`, not `warn`: it's a deliberate operator
/// decision, and warning on it would bury genuine delivery failures.
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
            delivered(channel.name(), "skipped");
            continue;
        }
        let started = std::time::Instant::now();
        let result = channel.deliver(alert).await;
        metrics::histogram!(
            "notification_channel_duration_seconds",
            "channel" => channel.name()
        )
        .record(started.elapsed().as_secs_f64());

        if let Err(e) = result {
            tracing::warn!(channel = channel.name(), error = %e, "external delivery failed");
            delivered(channel.name(), "error");
        } else {
            delivered(channel.name(), "ok");
        }
    }
}

/// Count one channel's delivery attempt.
///
/// The only signal a *fast-failing* relay produces. A relay that hangs stalls the consumer and
/// shows up as an undraining backlog; one that refuses immediately is logged, acked and
/// forgotten, so without this, "delivering nothing" and "nothing to deliver" are identical
/// from outside the process.
fn delivered(channel: &'static str, result: &'static str) {
    metrics::counter!(
        "notifications_delivered_total",
        "channel" => channel,
        "result" => result,
    )
    .increment(1);
}
