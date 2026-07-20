//! Pluggable external notification channels (design §14).
//!
//! In-app `notifications` rows remain the source of truth for the reader UI; these
//! optional channels fan a genuinely-new-chapter alert out to operator-configured
//! endpoints (a generic JSON webhook, Discord's webhook format, and SMTP email). Every
//! back-end is hidden behind the [`NotificationChannel`] trait so adding a new one is a
//! drop-in, and each is constructed purely from config.

use std::fmt::Write as _;
use std::time::Duration;

use async_trait::async_trait;
use tankovault_domain::SeriesId;
use lettre::message::Mailbox;
use lettre::message::header::ContentType;
use lettre::{AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor};
use serde::Deserialize;

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
    /// SMTP relay URL for the email channel, in lettre's
    /// `AsyncSmtpTransport::from_url` format (e.g. `smtps://user:pass@smtp.host:465`);
    /// TLS, port, and credentials are all encoded in the URL.
    #[serde(default)]
    pub email_smtp_url: Option<String>,
    /// `From` address for outgoing email (e.g. `TankoVault <alerts@example.com>`).
    #[serde(default)]
    pub email_from: Option<String>,
    /// Recipients of a new-chapter alert email. Empty disables the email channel.
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
            email_smtp_url: None,
            email_from: None,
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

/// Email channel: sends a plain-text new-chapter alert to one or more recipients over
/// SMTP (config-driven, operator-level — like the webhook/Discord channels).
pub(crate) struct EmailChannel {
    transport: AsyncSmtpTransport<Tokio1Executor>,
    from: Mailbox,
    to: Vec<Mailbox>,
}

impl EmailChannel {
    /// Build from a lettre relay URL, a `From` address, and one or more recipients.
    ///
    /// # Errors
    /// Returns an error when the URL or any address fails to parse, or when no
    /// recipient is supplied.
    pub(crate) fn from_config(smtp_url: &str, from: &str, to: &[String]) -> anyhow::Result<Self> {
        let transport = AsyncSmtpTransport::<Tokio1Executor>::from_url(smtp_url)?.build();
        let from: Mailbox = from.parse()?;
        let to = to
            .iter()
            .map(|addr| addr.parse::<Mailbox>().map_err(anyhow::Error::from))
            .collect::<anyhow::Result<Vec<_>>>()?;
        if to.is_empty() {
            anyhow::bail!("email channel requires at least one recipient");
        }
        Ok(Self {
            transport,
            from,
            to,
        })
    }

    /// Build the RFC 5322 message for `alert` (each recipient appended to `To`).
    fn message(&self, alert: &Alert) -> anyhow::Result<Message> {
        let mut builder = Message::builder().from(self.from.clone());
        for recipient in &self.to {
            builder = builder.to(recipient.clone());
        }
        let msg = builder
            .subject(email_subject(alert))
            .header(ContentType::TEXT_PLAIN)
            .body(email_body(alert))?;
        Ok(msg)
    }
}

#[async_trait]
impl NotificationChannel for EmailChannel {
    fn name(&self) -> &'static str {
        "email"
    }

    async fn deliver(&self, alert: &Alert) -> anyhow::Result<()> {
        let resp = self.transport.send(self.message(alert)?).await?;
        if !resp.is_positive() {
            anyhow::bail!(
                "smtp relay returned a non-positive response: {:?}",
                resp.code()
            );
        }
        Ok(())
    }
}

/// Build the set of enabled channels from config (empty when none are configured).
pub(crate) fn build(cfg: &ChannelsConfig) -> Vec<Box<dyn NotificationChannel>> {
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
    if let (Some(url), Some(from)) = (
        cfg.email_smtp_url.clone().filter(|u| !u.is_empty()),
        cfg.email_from.clone().filter(|f| !f.is_empty()),
    ) {
        if cfg.email_to.is_empty() {
            tracing::warn!("email channel has an smtp url + from but no recipients; skipping");
        } else {
            match EmailChannel::from_config(&url, &from, &cfg.email_to) {
                Ok(channel) => channels.push(Box::new(channel)),
                Err(e) => tracing::warn!(error = %e, "email channel misconfigured; skipping"),
            }
        }
    }
    channels
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let built = build(&cfg);
        assert_eq!(built.len(), 1);
        assert_eq!(built[0].name(), "discord");

        assert!(build(&ChannelsConfig::default()).is_empty());
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

    #[test]
    fn email_channel_builds_a_multi_recipient_message() {
        let ch = EmailChannel::from_config(
            "smtp://localhost:2525",
            "TankoVault <alerts@example.com>",
            &[
                "reader@example.com".to_owned(),
                "two@example.com".to_owned(),
            ],
        )
        .expect("valid config");
        let formatted =
            String::from_utf8(ch.message(&sample()).expect("builds").formatted()).expect("utf8");
        assert!(formatted.contains("Subject: New chapter 12"));
        assert!(formatted.contains("reader@example.com"));
        assert!(formatted.contains("two@example.com"));
        assert!(formatted.contains("From:"));
    }

    #[test]
    fn email_channel_requires_a_recipient() {
        assert!(EmailChannel::from_config("smtp://localhost:2525", "a@b.com", &[]).is_err());
    }

    #[test]
    fn build_adds_email_when_fully_configured() {
        let cfg = ChannelsConfig {
            email_smtp_url: Some("smtp://localhost:2525".to_owned()),
            email_from: Some("alerts@example.com".to_owned()),
            email_to: vec!["reader@example.com".to_owned()],
            ..ChannelsConfig::default()
        };
        let built = build(&cfg);
        assert_eq!(built.len(), 1);
        assert_eq!(built[0].name(), "email");

        // Missing recipients: no channel is constructed.
        let cfg = ChannelsConfig {
            email_smtp_url: Some("smtp://localhost:2525".to_owned()),
            email_from: Some("alerts@example.com".to_owned()),
            ..ChannelsConfig::default()
        };
        assert!(build(&cfg).is_empty());
    }
}
