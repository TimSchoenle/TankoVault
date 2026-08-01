//! Outgoing-email (SMTP relay) settings.

use secrecy::{ExposeSecret as _, SecretString};
use serde::Deserialize;

/// Transport security for an SMTP relay, chosen explicitly rather than inferred from the port.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EmailSecurity {
    /// Implicit TLS from the first byte (SMTPS, typically port 465).
    Tls,
    /// Plain connection upgraded via `STARTTLS` (typically port 587).
    ///
    /// The default: broadly compatible, and OVH's documented Exchange endpoint.
    #[default]
    StartTls,
    /// No transport security (plaintext; only for a trusted local relay / tests).
    None,
}

/// Outgoing-email settings for transactional messages (welcome, password reset).
///
/// Either a single [`Self::url`] (lettre relay URL, takes precedence when set) or the explicit
/// host/port/credentials/security fields. Enabled only when a relay and [`Self::from`] are
/// both present; otherwise falls back to a no-op mailer.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct EmailConfig {
    /// Full lettre relay URL; takes precedence over the explicit fields below when set.
    ///
    /// [`SecretString`]: the documented form embeds the mailbox password, unlike the
    /// neighbouring host/port fields.
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
    /// SMTP envelope sender (`MAIL FROM`), which can differ from the visible [`Self::from`]
    /// header.
    ///
    /// Providers enforcing "send as" checks (notably OVH Exchange) reject a mismatched
    /// envelope sender; unset defaults to [`Self::username`] to satisfy that.
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

    /// The effective envelope sender: explicit override, else the authenticated username, else
    /// `None` (the mailer then uses the `From:` header).
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
