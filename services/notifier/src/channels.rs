//! Pluggable external notification channels (design §14).
//!
//! In-app `notifications` rows remain the source of truth for the reader UI; these
//! optional channels fan a genuinely-new-chapter alert out to operator-configured
//! endpoints (a generic JSON webhook, Discord's webhook format, and SMTP email). Every
//! back-end is hidden behind the [`NotificationChannel`] trait so adding a new one is a
//! drop-in, and each is constructed purely from config.

use std::fmt::Write as _;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde::Deserialize;
use tankovault_domain::{Feature, SeriesId};
use tankovault_email::{EmailMessage, EmailService};

/// Operator-configured external channel endpoints. All optional; a channel is only
/// constructed when its URL is present and non-empty.
#[derive(Debug, Deserialize)]
pub(crate) struct ChannelsConfig {
    /// A Discord "Incoming Webhook" URL. Receives the Discord message/embed shape.
    #[serde(default)]
    pub discord_webhook_url: Option<String>,
    /// A generic HTTP endpoint. Receives [`webhook_payload`] as a JSON `POST` body.
    #[serde(default)]
    pub webhook_url: Option<String>,
    /// Recipients of a new-chapter alert email. Empty disables the email channel.
    ///
    /// The relay, credentials and `From` address are **not** here: they come from the shared
    /// `TANKOVAULT_EMAIL__*` config that the API already uses, so one deployment has one SMTP
    /// configuration rather than two that can disagree.
    #[serde(default)]
    pub email_to: Vec<String>,
    /// Per-request timeout for channel deliveries.
    #[serde(default = "default_timeout_secs")]
    pub timeout_secs: u64,
}

impl Default for ChannelsConfig {
    fn default() -> Self {
        Self {
            discord_webhook_url: None,
            webhook_url: None,
            email_to: Vec::new(),
            timeout_secs: default_timeout_secs(),
        }
    }
}

fn default_timeout_secs() -> u64 {
    10
}

/// A single new-chapter alert, already resolved to display-ready fields.
#[derive(Debug, Clone)]
pub(crate) struct Alert {
    pub series_id: SeriesId,
    pub chapter_number: f64,
    pub chapter_title: Option<String>,
    /// RELATIVE chapter path (resolve against the provider `base_url` at read time).
    pub chapter_path: String,
    pub provider_slug: String,
}

impl Alert {
    /// A compact human summary line reused by every channel.
    pub(crate) fn summary(&self) -> String {
        let num = format_number(self.chapter_number);
        match self.chapter_title.as_deref() {
            Some(t) if !t.is_empty() => {
                format!("New chapter {num}: {t} ({})", self.provider_slug)
            }
            _ => format!("New chapter {num} ({})", self.provider_slug),
        }
    }
}

/// Format a chapter number without a trailing `.0` for whole numbers (`12.0` -> `12`,
/// `12.5` -> `12.5`).
pub(crate) fn format_number(n: f64) -> String {
    // Rust's default float `Display` already yields the shortest form, dropping a
    // trailing `.0` for whole numbers (`12.0` -> `12`, `12.5` -> `12.5`).
    format!("{n}")
}

/// The JSON body delivered to a generic [`WebhookChannel`].
pub(crate) fn webhook_payload(alert: &Alert) -> serde_json::Value {
    serde_json::json!({
        "type": "new_chapter",
        "series_id": alert.series_id,
        "chapter_number": alert.chapter_number,
        "chapter_title": alert.chapter_title,
        "chapter_path": alert.chapter_path,
        "provider_slug": alert.provider_slug,
        "summary": alert.summary(),
    })
}

/// The JSON body delivered to a Discord incoming webhook (message + a single embed).
pub(crate) fn discord_payload(alert: &Alert) -> serde_json::Value {
    let title = match alert.chapter_title.as_deref() {
        Some(t) if !t.is_empty() => t.to_owned(),
        _ => format!("Chapter {}", format_number(alert.chapter_number)),
    };
    serde_json::json!({
        "content": alert.summary(),
        "embeds": [{
            "title": title,
            "fields": [
                {"name": "Chapter", "value": format_number(alert.chapter_number), "inline": true},
                {"name": "Provider", "value": alert.provider_slug, "inline": true},
            ],
        }],
    })
}

/// The subject line for a new-chapter alert email (reuses the shared summary).
pub(crate) fn email_subject(alert: &Alert) -> String {
    alert.summary()
}

/// The plain-text body of a new-chapter alert email.
pub(crate) fn email_body(alert: &Alert) -> String {
    let num = format_number(alert.chapter_number);
    let mut body = format!(
        "A new chapter is available.\n\nSeries: {}\nProvider: {}\nChapter: {num}\n",
        alert.series_id, alert.provider_slug,
    );
    if let Some(title) = alert.chapter_title.as_deref().filter(|t| !t.is_empty()) {
        let _ = writeln!(body, "Title: {title}");
    }
    let _ = writeln!(body, "Path: {}", alert.chapter_path);
    body
}

/// A pluggable delivery back-end. Implementations must be cheap to clone/share and are
/// invoked once per genuinely-new chapter.
#[async_trait]
pub(crate) trait NotificationChannel: Send + Sync {
    /// Stable identifier for logging/metrics.
    fn name(&self) -> &'static str;

    /// The feature that governs this channel at runtime.
    ///
    /// Configuration decides whether a channel *exists* (an SMTP relay was supplied); this
    /// flag decides whether it currently *delivers*. The two are genuinely different
    /// questions: an operator silencing outbound email during an incident should not have to
    /// delete the relay configuration and redeploy to do it, nor lose it when they want the
    /// channel back an hour later.
    fn feature(&self) -> Feature;

    /// Deliver `alert`. Errors are logged by the caller and never abort fan-out.
    ///
    /// # Errors
    /// Returns any transport/HTTP error so the caller can log it per channel.
    async fn deliver(&self, alert: &Alert) -> anyhow::Result<()>;
}

/// Generic JSON webhook: `POST` [`webhook_payload`] to a configured URL.
pub(crate) struct WebhookChannel {
    client: reqwest::Client,
    url: String,
}

impl WebhookChannel {
    pub(crate) fn new(client: reqwest::Client, url: String) -> Self {
        Self { client, url }
    }
}

#[async_trait]
impl NotificationChannel for WebhookChannel {
    fn name(&self) -> &'static str {
        "webhook"
    }

    fn feature(&self) -> Feature {
        Feature::NotificationsWebhook
    }

    async fn deliver(&self, alert: &Alert) -> anyhow::Result<()> {
        let resp = self
            .client
            .post(&self.url)
            .json(&webhook_payload(alert))
            .send()
            .await?;
        let status = resp.status();
        if !status.is_success() {
            anyhow::bail!("webhook returned HTTP {status}");
        }
        Ok(())
    }
}

/// Discord incoming-webhook channel.
pub(crate) struct DiscordChannel {
    client: reqwest::Client,
    url: String,
}

impl DiscordChannel {
    pub(crate) fn new(client: reqwest::Client, url: String) -> Self {
        Self { client, url }
    }
}

#[async_trait]
impl NotificationChannel for DiscordChannel {
    fn name(&self) -> &'static str {
        "discord"
    }

    fn feature(&self) -> Feature {
        Feature::NotificationsDiscord
    }

    async fn deliver(&self, alert: &Alert) -> anyhow::Result<()> {
        let resp = self
            .client
            .post(&self.url)
            .json(&discord_payload(alert))
            .send()
            .await?;
        let status = resp.status();
        if !status.is_success() {
            anyhow::bail!("discord webhook returned HTTP {status}");
        }
        Ok(())
    }
}

/// Email channel: a plain-text new-chapter alert to one or more operator recipients.
///
/// A thin adapter over [`tankovault_email::EmailService`], where it used to be a second,
/// private SMTP client: its own `AsyncSmtpTransport::from_url`, its own `Mailbox` parsing, its
/// own `Message` assembly, its own TLS configuration. Two SMTP stacks in one system means two
/// `From`/envelope policies, and the difference was not cosmetic — `crates/email` resolves the
/// **envelope sender from the SMTP login** rather than from the `From` header (relays that
/// enforce "send as", notably OVH Exchange, reject the mismatch with `550 5.7.60`), and this
/// copy did not. Operator alerts were therefore rejected by the same relay that happily
/// accepted password-reset mail from the API.
///
/// It also means this path inherits `crates/email`'s ten tests, where it had none.
pub(crate) struct EmailChannel {
    mailer: Arc<dyn EmailService>,
    to: Vec<String>,
}

impl EmailChannel {
    /// Build from the shared mailer and the configured recipients.
    fn new(mailer: Arc<dyn EmailService>, to: Vec<String>) -> Self {
        Self { mailer, to }
    }
}

#[async_trait]
impl NotificationChannel for EmailChannel {
    fn name(&self) -> &'static str {
        "email"
    }

    fn feature(&self) -> Feature {
        Feature::NotificationsEmail
    }

    async fn deliver(&self, alert: &Alert) -> anyhow::Result<()> {
        let message = EmailMessage {
            to: self.to.clone(),
            subject: email_subject(alert),
            text: email_body(alert),
            html: None,
        };
        self.mailer.send(message).await?;
        Ok(())
    }
}

/// Build the set of enabled channels from config (empty when none are configured).
pub(crate) fn build(
    cfg: &ChannelsConfig,
    email: &tankovault_config::EmailConfig,
) -> Vec<Box<dyn NotificationChannel>> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(cfg.timeout_secs))
        .build()
        .unwrap_or_default();

    let mut channels: Vec<Box<dyn NotificationChannel>> = Vec::new();
    if let Some(url) = cfg.webhook_url.clone().filter(|u| !u.is_empty()) {
        channels.push(Box::new(WebhookChannel::new(client.clone(), url)));
    }
    if let Some(url) = cfg.discord_webhook_url.clone().filter(|u| !u.is_empty()) {
        channels.push(Box::new(DiscordChannel::new(client.clone(), url)));
    }
    // The mailer comes from the same `EmailConfig` and the same builder the API uses, so an
    // unconfigured deployment gets `NoopMailer` and this channel degrades identically.
    if email.is_enabled() {
        if cfg.email_to.is_empty() {
            tracing::warn!("email is configured but the notifier has no recipients; skipping");
        } else {
            channels.push(Box::new(EmailChannel::new(
                tankovault_email::build(email),
                cfg.email_to.clone(),
            )));
        }
    } else if !cfg.email_to.is_empty() {
        tracing::warn!(
            "the notifier has email recipients but no relay is configured; set              TANKOVAULT_EMAIL__HOST and TANKOVAULT_EMAIL__FROM"
        );
    }
    channels
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// A server that accepts one `POST /hook` and records it.
    async fn accepting_server() -> MockServer {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/hook"))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;
        server
    }

    /// The JSON body of the one request the server received.
    async fn sole_request_body(server: &MockServer) -> serde_json::Value {
        let requests = server.received_requests().await.expect("request recording");
        assert_eq!(requests.len(), 1, "expected exactly one request");
        serde_json::from_slice(&requests[0].body).expect("a JSON request body")
    }

    fn sample() -> Alert {
        Alert {
            series_id: SeriesId::new(),
            chapter_number: 12.0,
            chapter_title: Some("The Duel".to_owned()),
            chapter_path: "/manga/x/ch-12".to_owned(),
            provider_slug: "kunmanga".to_owned(),
        }
    }

    #[test]
    fn format_number_trims_whole_numbers() {
        assert_eq!(format_number(12.0), "12");
        assert_eq!(format_number(12.5), "12.5");
        assert_eq!(format_number(0.0), "0");
    }

    #[test]
    fn summary_includes_title_and_provider() {
        let s = sample().summary();
        assert_eq!(s, "New chapter 12: The Duel (kunmanga)");
    }

    #[test]
    fn summary_without_title_omits_colon() {
        let mut a = sample();
        a.chapter_title = None;
        assert_eq!(a.summary(), "New chapter 12 (kunmanga)");
        a.chapter_title = Some(String::new());
        assert_eq!(a.summary(), "New chapter 12 (kunmanga)");
    }

    #[test]
    fn webhook_payload_carries_all_fields() {
        let a = sample();
        let v = webhook_payload(&a);
        assert_eq!(v["type"], "new_chapter");
        assert_eq!(v["chapter_number"], 12.0);
        assert_eq!(v["chapter_title"], "The Duel");
        assert_eq!(v["chapter_path"], "/manga/x/ch-12");
        assert_eq!(v["provider_slug"], "kunmanga");
        assert_eq!(v["summary"], "New chapter 12: The Duel (kunmanga)");
    }

    #[test]
    fn discord_payload_has_content_and_embed() {
        let v = discord_payload(&sample());
        assert_eq!(v["content"], "New chapter 12: The Duel (kunmanga)");
        assert_eq!(v["embeds"][0]["title"], "The Duel");
        assert_eq!(v["embeds"][0]["fields"][0]["value"], "12");
        assert_eq!(v["embeds"][0]["fields"][1]["value"], "kunmanga");
    }

    #[test]
    fn discord_payload_falls_back_to_generic_title() {
        let mut a = sample();
        a.chapter_title = None;
        let v = discord_payload(&a);
        assert_eq!(v["embeds"][0]["title"], "Chapter 12");
    }

    #[test]
    fn build_respects_configured_urls() {
        let cfg = ChannelsConfig {
            discord_webhook_url: Some("https://discord.example/hook".to_owned()),
            webhook_url: Some(String::new()),
            timeout_secs: 5,
            ..ChannelsConfig::default()
        };
        let built = build(&cfg, &tankovault_config::EmailConfig::default());
        assert_eq!(built.len(), 1);
        assert_eq!(built[0].name(), "discord");

        assert!(
            build(
                &ChannelsConfig::default(),
                &tankovault_config::EmailConfig::default()
            )
            .is_empty()
        );
    }

    #[test]
    fn email_subject_matches_summary() {
        assert_eq!(
            email_subject(&sample()),
            "New chapter 12: The Duel (kunmanga)"
        );
    }

    #[test]
    fn email_body_contains_key_fields() {
        let a = sample();
        let body = email_body(&a);
        assert!(body.contains("Provider: kunmanga"));
        assert!(body.contains("Chapter: 12\n"));
        assert!(body.contains("Title: The Duel"));
        assert!(body.contains("Path: /manga/x/ch-12"));
        assert!(body.contains(&a.series_id.to_string()));
    }

    #[test]
    fn email_body_omits_empty_title() {
        let mut a = sample();
        a.chapter_title = None;
        assert!(!email_body(&a).contains("Title:"));
        a.chapter_title = Some(String::new());
        assert!(!email_body(&a).contains("Title:"));
    }

    /// A relay is configured but the notifier names nobody: no channel, and a warning.
    /// Building one would send operator alerts to an empty recipient list.
    #[test]
    fn email_needs_both_a_relay_and_recipients() {
        let relay = tankovault_config::EmailConfig {
            host: Some("smtp.example.com".to_owned()),
            from: Some("TankoVault <alerts@example.com>".to_owned()),
            ..tankovault_config::EmailConfig::default()
        };
        assert!(relay.is_enabled());

        // Relay, no recipients.
        assert!(build(&ChannelsConfig::default(), &relay).is_empty());

        // Recipients, no relay — the mirror image, and equally not a working channel.
        let with_recipients = ChannelsConfig {
            email_to: vec!["reader@example.com".to_owned()],
            ..ChannelsConfig::default()
        };
        assert!(build(&with_recipients, &tankovault_config::EmailConfig::default()).is_empty());

        // Both: one email channel.
        let built = build(&with_recipients, &relay);
        assert_eq!(built.len(), 1);
        assert_eq!(built[0].name(), "email");
        assert_eq!(built[0].feature(), Feature::NotificationsEmail);
    }

    /// The subject and body are this module's contribution; the transport is
    /// `crates/email`'s, and is tested there.
    #[test]
    fn the_alert_message_carries_every_recipient_and_the_chapter_subject() {
        let channel = EmailChannel::new(
            tankovault_email::build(&tankovault_config::EmailConfig::default()),
            vec![
                "reader@example.com".to_owned(),
                "two@example.com".to_owned(),
            ],
        );
        assert_eq!(channel.to.len(), 2);
        assert_eq!(
            email_subject(&sample()),
            "New chapter 12: The Duel (kunmanga)"
        );
        assert!(email_body(&sample()).contains("kunmanga"));
    }

    /// The payload builders above are pure and were already covered; what was not was the step
    /// that puts one on the wire. Nothing connected [`webhook_payload`] to the body an operator's
    /// endpoint actually receives, so the delivered document is asserted to *be* the builder's
    /// output rather than re-spelled here — the failure this catches is `deliver` sending
    /// something else, not the builder changing shape, which its own test owns (F-09).
    #[tokio::test]
    async fn the_generic_webhook_posts_the_payload_builder_output_as_json() {
        let server = accepting_server().await;
        let alert = sample();

        WebhookChannel::new(reqwest::Client::new(), format!("{}/hook", server.uri()))
            .deliver(&alert)
            .await
            .expect("a 204 is a successful delivery");

        assert_eq!(sole_request_body(&server).await, webhook_payload(&alert));
        let requests = server.received_requests().await.expect("request recording");
        assert_eq!(
            requests[0]
                .headers
                .get("content-type")
                .map(|v| v.to_str().expect("ASCII header")),
            Some("application/json"),
            "a receiver that dispatches on Content-Type would drop this"
        );
    }

    /// The same for Discord, which is a *different* document to the same transport — the two
    /// channels share `deliver`'s shape and nothing but a test would notice one being handed the
    /// other's builder.
    #[tokio::test]
    async fn the_discord_webhook_posts_the_discord_document() {
        let server = accepting_server().await;
        let alert = sample();

        DiscordChannel::new(reqwest::Client::new(), format!("{}/hook", server.uri()))
            .deliver(&alert)
            .await
            .expect("a 204 is a successful delivery");

        assert_eq!(sole_request_body(&server).await, discord_payload(&alert));
    }

    /// A rejected delivery has to *be* an error, and name the status: the caller logs whatever
    /// comes back per channel and never aborts fan-out, so this string is the entire diagnostic
    /// an operator gets for a webhook that has been quietly `401`ing for a week.
    #[tokio::test]
    async fn a_rejected_delivery_is_an_error_naming_the_channel_and_the_status() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/hook"))
            .respond_with(ResponseTemplate::new(401))
            .expect(2)
            .mount(&server)
            .await;
        let url = format!("{}/hook", server.uri());

        let webhook = WebhookChannel::new(reqwest::Client::new(), url.clone())
            .deliver(&sample())
            .await
            .expect_err("a 401 is a failed delivery")
            .to_string();
        assert!(webhook.contains("401"), "no status in: {webhook}");
        assert!(webhook.contains("webhook"), "no channel in: {webhook}");

        let discord = DiscordChannel::new(reqwest::Client::new(), url)
            .deliver(&sample())
            .await
            .expect_err("a 401 is a failed delivery")
            .to_string();
        assert!(discord.contains("401"), "no status in: {discord}");
        assert!(discord.contains("discord"), "no channel in: {discord}");
    }

    /// `timeout_secs` is the only knob on [`ChannelsConfig`] whose effect is invisible in the
    /// constructed value, and a configuration knob that silently does nothing is worse than an
    /// absent one — the operator who sets it believes deliveries are bounded. Driven through
    /// [`build`] rather than by constructing a client here, because the wiring being asserted is
    /// exactly the line in `build` that could drop it.
    #[tokio::test]
    async fn the_configured_timeout_reaches_the_client_that_delivers() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/hook"))
            .respond_with(ResponseTemplate::new(204).set_delay(Duration::from_secs(30)))
            .mount(&server)
            .await;

        let cfg = ChannelsConfig {
            webhook_url: Some(format!("{}/hook", server.uri())),
            timeout_secs: 1,
            ..ChannelsConfig::default()
        };
        let channels = build(&cfg, &tankovault_config::EmailConfig::default());
        assert_eq!(channels.len(), 1);

        let started = Instant::now();
        let err = channels[0]
            .deliver(&sample())
            .await
            .expect_err("the delivery outlives the timeout");
        assert!(
            err.downcast_ref::<reqwest::Error>()
                .is_some_and(reqwest::Error::is_timeout),
            "not a timeout: {err}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "the 1s timeout was not applied: {:?}",
            started.elapsed()
        );
    }
}
