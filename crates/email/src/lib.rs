//! # tankovault-email
//!
//! An abstract transactional-email service for user-facing flows (welcome on
//! registration, password reset). Every back-end is hidden behind the [`EmailService`]
//! trait so callers depend on the abstraction, never on `lettre` directly:
//!
//! - [`SmtpMailer`] — an SMTP relay built from [`EmailConfig`]. It speaks either a full
//!   lettre relay URL or the explicit host/port/credentials/security fields, which makes
//!   pointing it at an **OVH-hosted Exchange** mailbox a matter of config
//!   (`pro*.mail.ovh.net:587` STARTTLS or `ssl0.ovh.net:465` implicit TLS).
//! - [`NoopMailer`] — logs and drops. Used automatically when email is not configured, so
//!   development and tests never require a live relay.
//!
//! Construct the right implementation for the current config with [`build`].

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use lettre::address::Envelope;
use lettre::message::header::ContentType;
use lettre::message::{Mailbox, MultiPart, SinglePart};
use lettre::transport::smtp::authentication::Credentials;
use lettre::{Address, AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor};
use tankovault_config::{EmailConfig, EmailSecurity};

/// Errors raised while building or sending email.
#[derive(Debug, thiserror::Error)]
pub enum EmailError {
    /// A `From`/`To` mailbox failed to parse.
    #[error("invalid email address: {0}")]
    Address(String),
    /// The relay could not be constructed from the supplied configuration.
    #[error("email transport configuration error: {0}")]
    Config(String),
    /// The RFC 5322 message could not be assembled.
    #[error("failed to build email message: {0}")]
    Build(String),
    /// The relay rejected the message or the connection failed.
    #[error("failed to send email: {0}")]
    Transport(String),
}

/// A single outbound email, already resolved to display-ready fields.
#[derive(Debug, Clone)]
pub struct EmailMessage {
    /// One or more recipient addresses (RFC 5322, e.g. `Reader <r@example.com>`).
    pub to: Vec<String>,
    /// Subject line.
    pub subject: String,
    /// Plain-text body (always sent; the fallback part when [`Self::html`] is set).
    pub text: String,
    /// Optional HTML body; when present the message is sent as `multipart/alternative`.
    pub html: Option<String>,
}

impl EmailMessage {
    /// A plain-text message to a single recipient.
    #[must_use]
    pub fn text(
        to: impl Into<String>,
        subject: impl Into<String>,
        body: impl Into<String>,
    ) -> Self {
        Self {
            to: vec![to.into()],
            subject: subject.into(),
            text: body.into(),
            html: None,
        }
    }

    /// Attach an HTML alternative body (a plain-text part is still required and sent).
    #[must_use]
    pub fn with_html(mut self, html: impl Into<String>) -> Self {
        self.html = Some(html.into());
        self
    }
}

/// A pluggable email delivery back-end. Implementations are cheap to clone/share and are
/// invoked once per outbound message.
#[async_trait]
pub trait EmailService: Send + Sync {
    /// Whether this back-end actually delivers mail. `false` for the [`NoopMailer`], which
    /// lets callers decide whether a feature that depends on email is available.
    fn is_enabled(&self) -> bool;

    /// Deliver `message`.
    ///
    /// # Errors
    /// Returns [`EmailError`] when an address fails to parse, the message cannot be
    /// assembled, or the relay rejects/refuses the send.
    async fn send(&self, message: EmailMessage) -> Result<(), EmailError>;
}

/// An SMTP-backed mailer. Built from [`EmailConfig`] (or a raw relay URL) and reused for
/// every send; a connection is opened per message (no background pool task), matching the
/// rest of the stack's runtime-independent construction.
pub struct SmtpMailer {
    transport: AsyncSmtpTransport<Tokio1Executor>,
    from: Mailbox,
    /// SMTP envelope sender (`MAIL FROM` / reverse-path). This is the identity the relay
    /// authorises the send against and is deliberately decoupled from the `From:` header:
    /// providers such as OVH-hosted Exchange reject a `MAIL FROM` that differs from the
    /// authenticated mailbox (`550 5.7.60`), so it defaults to the login username while the
    /// `From:` header may still show a different address.
    envelope_from: Address,
}

impl SmtpMailer {
    /// Build from [`EmailConfig`], preferring an explicit `url` and otherwise assembling
    /// the relay from the host/port/credentials/security fields.
    ///
    /// # Errors
    /// Returns [`EmailError::Config`] when no relay is configured or the relay cannot be
    /// built, and [`EmailError::Address`] when `from` is missing or unparseable.
    pub fn from_config(cfg: &EmailConfig) -> Result<Self, EmailError> {
        let from_raw = cfg
            .from
            .as_deref()
            .filter(|f| !f.is_empty())
            .ok_or_else(|| EmailError::Config("email.from is required to send mail".to_owned()))?;
        let from: Mailbox = from_raw
            .parse()
            .map_err(|e| EmailError::Address(format!("{from_raw:?}: {e}")))?;

        let timeout = Some(Duration::from_secs(cfg.timeout_secs));
        let transport = if let Some(url) = cfg.url.as_deref().filter(|u| !u.is_empty()) {
            AsyncSmtpTransport::<Tokio1Executor>::from_url(url)
                .map_err(|e| EmailError::Config(e.to_string()))?
                .timeout(timeout)
                .build()
        } else {
            let host = cfg
                .host
                .as_deref()
                .filter(|h| !h.is_empty())
                .ok_or_else(|| {
                    EmailError::Config("email.host or email.url is required".to_owned())
                })?;
            let mut builder = match cfg.security {
                EmailSecurity::Tls => AsyncSmtpTransport::<Tokio1Executor>::relay(host)
                    .map_err(|e| EmailError::Config(e.to_string()))?,
                EmailSecurity::StartTls => {
                    AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(host)
                        .map_err(|e| EmailError::Config(e.to_string()))?
                }
                EmailSecurity::None => {
                    AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(host)
                }
            }
            .port(cfg.effective_port())
            .timeout(timeout);

            if let (Some(user), Some(pass)) = (cfg.username.as_deref(), cfg.password.as_deref()) {
                if !user.is_empty() {
                    builder =
                        builder.credentials(Credentials::new(user.to_owned(), pass.to_owned()));
                }
            }
            builder.build()
        };

        let envelope_from = Self::resolve_envelope_from(cfg.effective_envelope_from(), &from)?;
        Ok(Self {
            transport,
            from,
            envelope_from,
        })
    }

    /// Resolve the SMTP envelope sender address: parse the configured override/login when
    /// present, otherwise reuse the `From:` header's address.
    fn resolve_envelope_from(
        configured: Option<&str>,
        from: &Mailbox,
    ) -> Result<Address, EmailError> {
        match configured {
            Some(raw) => raw
                .parse::<Mailbox>()
                .map(|mb| mb.email)
                .map_err(|e| EmailError::Address(format!("envelope sender {raw:?}: {e}"))),
            None => Ok(from.email.clone()),
        }
    }

    /// Build from a raw lettre relay URL plus a `From` mailbox.
    ///
    /// # Errors
    /// Returns [`EmailError::Config`] when the URL cannot be parsed and
    /// [`EmailError::Address`] when `from` is unparseable.
    pub fn from_url(url: &str, from: &str) -> Result<Self, EmailError> {
        let from: Mailbox = from
            .parse()
            .map_err(|e| EmailError::Address(format!("{from:?}: {e}")))?;
        let transport = AsyncSmtpTransport::<Tokio1Executor>::from_url(url)
            .map_err(|e| EmailError::Config(e.to_string()))?
            .build();
        let envelope_from = from.email.clone();
        Ok(Self {
            transport,
            from,
            envelope_from,
        })
    }

    /// Assemble the SMTP envelope (`MAIL FROM` reverse-path + `RCPT TO` forward-paths) for
    /// `message`. The reverse-path is [`Self::envelope_from`] rather than the `From:` header
    /// so relays that enforce a "send as" match against the authenticated mailbox accept it.
    fn build_envelope(&self, message: &EmailMessage) -> Result<Envelope, EmailError> {
        let mut recipients = Vec::with_capacity(message.to.len());
        for raw in &message.to {
            let mailbox: Mailbox = raw
                .parse()
                .map_err(|e| EmailError::Address(format!("{raw:?}: {e}")))?;
            recipients.push(mailbox.email);
        }
        Envelope::new(Some(self.envelope_from.clone()), recipients)
            .map_err(|e| EmailError::Address(e.to_string()))
    }

    /// Assemble the RFC 5322 message for `message`.
    fn build_message(&self, message: &EmailMessage) -> Result<Message, EmailError> {
        if message.to.is_empty() {
            return Err(EmailError::Address("no recipients".to_owned()));
        }
        let mut builder = Message::builder().from(self.from.clone());
        for raw in &message.to {
            let mailbox: Mailbox = raw
                .parse()
                .map_err(|e| EmailError::Address(format!("{raw:?}: {e}")))?;
            builder = builder.to(mailbox);
        }
        let builder = builder.subject(&message.subject);

        let msg = match &message.html {
            Some(html) => builder
                .multipart(
                    MultiPart::alternative()
                        .singlepart(
                            SinglePart::builder()
                                .header(ContentType::TEXT_PLAIN)
                                .body(message.text.clone()),
                        )
                        .singlepart(
                            SinglePart::builder()
                                .header(ContentType::TEXT_HTML)
                                .body(html.clone()),
                        ),
                )
                .map_err(|e| EmailError::Build(e.to_string()))?,
            None => builder
                .header(ContentType::TEXT_PLAIN)
                .body(message.text.clone())
                .map_err(|e| EmailError::Build(e.to_string()))?,
        };
        Ok(msg)
    }
}

#[async_trait]
impl EmailService for SmtpMailer {
    fn is_enabled(&self) -> bool {
        true
    }

    async fn send(&self, message: EmailMessage) -> Result<(), EmailError> {
        let msg = self.build_message(&message)?;
        let envelope = self.build_envelope(&message)?;
        // Send with an explicit envelope so the SMTP `MAIL FROM` is `envelope_from` (the
        // authenticated login by default) rather than the `From:` header, which is what
        // "send as"-enforcing relays like OVH Exchange authorise against.
        let resp = self
            .transport
            .send_raw(&envelope, &msg.formatted())
            .await
            .map_err(|e| EmailError::Transport(e.to_string()))?;
        if !resp.is_positive() {
            return Err(EmailError::Transport(format!(
                "smtp relay returned a non-positive response: {:?}",
                resp.code()
            )));
        }
        Ok(())
    }
}

/// A mailer that delivers nothing: it logs the message it *would* have sent and returns
/// success. Selected automatically when email is unconfigured so the app runs without a
/// relay (development, CI, self-hosting without SMTP).
pub struct NoopMailer;

#[async_trait]
impl EmailService for NoopMailer {
    fn is_enabled(&self) -> bool {
        false
    }

    async fn send(&self, message: EmailMessage) -> Result<(), EmailError> {
        tracing::info!(
            to = ?message.to,
            subject = %message.subject,
            "email not configured; dropping message (no-op mailer)"
        );
        Ok(())
    }
}

/// Build the appropriate [`EmailService`] for `cfg`: a live [`SmtpMailer`] when a relay and
/// `From` address are configured, otherwise a [`NoopMailer`]. A misconfigured relay (e.g. an
/// unparseable URL) also degrades to the no-op mailer with a warning rather than aborting
/// service boot.
#[must_use]
pub fn build(cfg: &EmailConfig) -> Arc<dyn EmailService> {
    if !cfg.is_enabled() {
        tracing::info!("email not configured; transactional emails are disabled");
        return Arc::new(NoopMailer);
    }
    match SmtpMailer::from_config(cfg) {
        Ok(mailer) => {
            tracing::info!(from = ?cfg.from, "transactional email enabled (SMTP)");
            Arc::new(mailer)
        }
        Err(e) => {
            tracing::warn!(error = %e, "email misconfigured; transactional emails disabled");
            Arc::new(NoopMailer)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_returns_noop_when_unconfigured() {
        let mailer = build(&EmailConfig::default());
        assert!(!mailer.is_enabled());
    }

    #[test]
    fn build_returns_live_mailer_when_configured() {
        let cfg = EmailConfig {
            host: Some("localhost".to_owned()),
            port: Some(2525),
            security: EmailSecurity::None,
            from: Some("TankoVault <no-reply@example.com>".to_owned()),
            ..EmailConfig::default()
        };
        let mailer = build(&cfg);
        assert!(mailer.is_enabled());
    }

    #[test]
    fn from_config_requires_a_from_address() {
        let cfg = EmailConfig {
            host: Some("localhost".to_owned()),
            security: EmailSecurity::None,
            ..EmailConfig::default()
        };
        assert!(matches!(
            SmtpMailer::from_config(&cfg),
            Err(EmailError::Config(_))
        ));
    }

    #[test]
    fn builds_a_plain_text_message() {
        let mailer = SmtpMailer::from_url("smtp://localhost:2525", "TankoVault <a@example.com>")
            .expect("valid config");
        let msg = EmailMessage::text("reader@example.com", "Hello", "Body text");
        let formatted = String::from_utf8(mailer.build_message(&msg).expect("builds").formatted())
            .expect("utf8");
        assert!(formatted.contains("Subject: Hello"));
        assert!(formatted.contains("reader@example.com"));
        assert!(formatted.contains("Body text"));
        assert!(formatted.contains("From:"));
    }

    #[test]
    fn builds_a_multipart_html_message() {
        let mailer =
            SmtpMailer::from_url("smtp://localhost:2525", "a@example.com").expect("valid config");
        let msg = EmailMessage::text("reader@example.com", "Hi", "plain").with_html("<p>rich</p>");
        let formatted = String::from_utf8(mailer.build_message(&msg).expect("builds").formatted())
            .expect("utf8");
        assert!(formatted.contains("multipart/alternative"));
        assert!(formatted.contains("text/plain"));
        assert!(formatted.contains("text/html"));
    }

    #[test]
    fn rejects_an_unparseable_recipient() {
        let mailer =
            SmtpMailer::from_url("smtp://localhost:2525", "a@example.com").expect("valid config");
        let msg = EmailMessage::text("not-an-address", "Hi", "body");
        assert!(matches!(
            mailer.build_message(&msg),
            Err(EmailError::Address(_))
        ));
    }

    #[test]
    fn envelope_sender_defaults_to_login_not_from_header() {
        // OVH Exchange rejects a MAIL FROM that differs from the authenticated mailbox, so
        // when the login differs from the visible From the envelope must use the login.
        let cfg = EmailConfig {
            host: Some("ssl0.ovh.net".to_owned()),
            security: EmailSecurity::None,
            username: Some("login@my-domain.com".to_owned()),
            password: Some("secret".to_owned()),
            from: Some("TankoVault <no-reply@my-domain.com>".to_owned()),
            ..EmailConfig::default()
        };
        let mailer = SmtpMailer::from_config(&cfg).expect("valid config");
        let envelope = mailer
            .build_envelope(&EmailMessage::text("reader@example.com", "Hi", "body"))
            .expect("builds envelope");
        assert_eq!(
            envelope.from().map(ToString::to_string),
            Some("login@my-domain.com".to_owned())
        );
    }

    #[test]
    fn explicit_envelope_from_overrides_login() {
        let cfg = EmailConfig {
            host: Some("ssl0.ovh.net".to_owned()),
            security: EmailSecurity::None,
            username: Some("login@my-domain.com".to_owned()),
            password: Some("secret".to_owned()),
            envelope_from: Some("bounce@my-domain.com".to_owned()),
            from: Some("TankoVault <no-reply@my-domain.com>".to_owned()),
            ..EmailConfig::default()
        };
        let mailer = SmtpMailer::from_config(&cfg).expect("valid config");
        let envelope = mailer
            .build_envelope(&EmailMessage::text("reader@example.com", "Hi", "body"))
            .expect("builds envelope");
        assert_eq!(
            envelope.from().map(ToString::to_string),
            Some("bounce@my-domain.com".to_owned())
        );
    }

    #[test]
    fn envelope_falls_back_to_from_header_without_login() {
        // No username / envelope override configured (e.g. a raw relay URL): the envelope
        // sender simply mirrors the From header address.
        let mailer = SmtpMailer::from_url("smtp://localhost:2525", "TankoVault <a@example.com>")
            .expect("valid config");
        let envelope = mailer
            .build_envelope(&EmailMessage::text("reader@example.com", "Hi", "body"))
            .expect("builds envelope");
        assert_eq!(
            envelope.from().map(ToString::to_string),
            Some("a@example.com".to_owned())
        );
    }
}
