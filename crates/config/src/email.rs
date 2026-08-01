//! Outgoing-email (SMTP relay) settings.

use secrecy::{ExposeSecret as _, SecretString};
use serde::Deserialize;

/// Transport security for an SMTP relay.
///
/// Chosen explicitly rather than inferred from the port so an operator's intent is always
/// unambiguous. For an OVH-hosted Exchange mailbox the usual choices are
/// [`Self::Tls`] on port `465` (`ssl0.ovh.net`) or [`Self::StartTls`] on port `587`
/// (`pro*.mail.ovh.net`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EmailSecurity {
    /// Implicit TLS from the first byte (SMTPS, typically port 465).
    Tls,
    /// Plain connection upgraded via the `STARTTLS` command (typically port 587).
    ///
    /// The default: STARTTLS on 587 is the most broadly compatible option and matches OVH's
    /// documented Exchange submission endpoint (`pro*.mail.ovh.net:587`).
    #[default]
    StartTls,
    /// No transport security (plaintext; only for a trusted local relay / tests).
    None,
}

/// Outgoing-email settings for transactional messages (welcome, password reset).
///
/// Two mutually exclusive ways to point at a relay:
/// 1. A single [`Self::url`] in lettre's `AsyncSmtpTransport::from_url` format
///    (e.g. `smtps://user:pass@ssl0.ovh.net:465`), which encodes host, port, TLS and
///    credentials at once and takes precedence when set.
/// 2. The explicit [`Self::host`]/[`Self::port`]/[`Self::username`]/[`Self::password`]/
///    [`Self::security`] fields, which read more naturally for an OVH Exchange mailbox.
///
/// The channel is only enabled when a relay (`url` or `host`) and a [`Self::from`] address
/// are both present; otherwise the app falls back to a no-op mailer that logs and drops.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct EmailConfig {
    /// Full lettre relay URL; takes precedence over the explicit fields below when set.
    ///
    /// [`SecretString`]: the documented form embeds the mailbox password
    /// (`smtps://user:pass@ssl0.ovh.net:465`), so this field is a credential even though the
    /// two neighbouring host/port fields are not.
    #[serde(default)]
    pub url: Option<SecretString>,
    /// SMTP host (OVH Exchange: `pro3.mail.ovh.net` for STARTTLS or `ssl0.ovh.net` for TLS).
    #[serde(default)]
    pub host: Option<String>,
    /// SMTP port. Defaults per [`Self::security`] when omitted (465 / 587 / 25).
    #[serde(default)]
    pub port: Option<u16>,
    /// Login username (for OVH Exchange this is the full mailbox address).
    #[serde(default)]
    pub username: Option<String>,
    /// Login password / app password.
    #[serde(default)]
    pub password: Option<SecretString>,
    /// Transport security to use with the explicit host/port fields.
    #[serde(default)]
    pub security: EmailSecurity,
    /// Default `From` mailbox, e.g. `TankoVault <no-reply@example.com>`. Required to send.
    #[serde(default)]
    pub from: Option<String>,
    /// SMTP envelope sender (the `MAIL FROM` / `Return-Path`) used at the protocol level,
    /// which can differ from the visible [`Self::from`] header.
    ///
    /// Providers that enforce "send as" checks — notably **OVH-hosted Exchange** — reject a
    /// message whose envelope sender is not the authenticated mailbox (SMTP `550 5.7.60
    /// Client does not have permissions to send as this sender`). Leave this unset to default
    /// to [`Self::username`] (the authenticated login), which is what those providers require
    /// while still letting the `From:` header show a different address; set it explicitly only
    /// to override that reverse-path.
    #[serde(default)]
    pub envelope_from: Option<String>,
    /// Public base URL of the web app, used to build absolute links inside emails
    /// (e.g. the password-reset link). No trailing slash.
    #[serde(default = "EmailConfig::default_base_url")]
    pub base_url: String,
    /// Per-message send timeout, seconds.
    #[serde(default = "EmailConfig::default_timeout_secs")]
    pub timeout_secs: u64,
}

impl EmailConfig {
    fn default_base_url() -> String {
        "http://localhost:8080".to_owned()
    }

    fn default_timeout_secs() -> u64 {
        15
    }

    /// The effective port, applying the security-specific default when none is configured.
    #[must_use]
    pub fn effective_port(&self) -> u16 {
        self.port.unwrap_or(match self.security {
            EmailSecurity::Tls => 465,
            EmailSecurity::StartTls => 587,
            EmailSecurity::None => 25,
        })
    }

    /// The SMTP envelope sender (`MAIL FROM`), preferring an explicit [`Self::envelope_from`]
    /// and otherwise falling back to the authenticated [`Self::username`]. Returns `None` when
    /// neither is set, in which case the mailer uses the `From:` header address.
    #[must_use]
    pub fn effective_envelope_from(&self) -> Option<&str> {
        self.envelope_from
            .as_deref()
            .filter(|s| !s.is_empty())
            .or_else(|| self.username.as_deref().filter(|s| !s.is_empty()))
    }

    /// Whether enough is configured to actually send mail (a relay plus a `From` address).
    #[must_use]
    pub fn is_enabled(&self) -> bool {
        let has_relay = self
            .url
            .as_ref()
            .is_some_and(|u| !u.expose_secret().is_empty())
            || self.host.as_deref().is_some_and(|h| !h.is_empty());
        has_relay && self.from.as_deref().is_some_and(|f| !f.is_empty())
    }
}

#[cfg(test)]
mod tests {
    use super::{EmailConfig, EmailSecurity};

    #[test]
    fn email_disabled_without_relay_or_from() {
        // Nothing configured → disabled.
        assert!(!EmailConfig::default().is_enabled());
        // Relay but no `From` → still disabled.
        let cfg = EmailConfig {
            host: Some("pro3.mail.ovh.net".to_owned()),
            ..EmailConfig::default()
        };
        assert!(!cfg.is_enabled());
    }

    #[test]
    fn email_enabled_with_host_and_from() {
        let cfg = EmailConfig {
            host: Some("pro3.mail.ovh.net".to_owned()),
            from: Some("no-reply@example.com".to_owned()),
            ..EmailConfig::default()
        };
        assert!(cfg.is_enabled());
    }

    #[test]
    fn email_port_defaults_follow_security() {
        let starttls = EmailConfig::default();
        assert_eq!(starttls.security, EmailSecurity::StartTls);
        assert_eq!(starttls.effective_port(), 587);

        let tls = EmailConfig {
            security: EmailSecurity::Tls,
            ..EmailConfig::default()
        };
        assert_eq!(tls.effective_port(), 465);

        let explicit = EmailConfig {
            port: Some(2525),
            security: EmailSecurity::None,
            ..EmailConfig::default()
        };
        assert_eq!(explicit.effective_port(), 2525);
    }
}
