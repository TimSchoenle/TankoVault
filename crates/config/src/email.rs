//! Outgoing-email (SMTP relay) settings.

use secrecy::{ExposeSecret as _, SecretString};
use serde::Deserialize;
use terrace_config::schema::Describe;

/// Transport security for an SMTP relay, set explicitly and never inferred from the port.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Describe)]
#[serde(rename_all = "lowercase")]
pub enum EmailSecurity {
    /// Implicit TLS from the first byte (SMTPS, typically port 465).
    Tls,
    /// Plain connection upgraded via `STARTTLS` (typically port 587).
    ///
    /// The default, and what OVH documents for its Exchange endpoint.
    #[default]
    StartTls,
    /// No transport security. Only for a trusted local relay or tests.
    None,
}

/// Outgoing-email settings for transactional messages such as the welcome and password-reset
/// mail.
///
/// The relay is either [`Self::url`], which wins when set, or the explicit host, port,
/// credential and security fields. Mail is sent only when a relay and [`Self::from`] are both
/// present; otherwise the mailer discards every message.
#[derive(Debug, Clone, Default, Deserialize, Describe)]
pub struct EmailConfig {
    /// Full lettre relay URL. When set, it wins over the explicit fields below.
    // A `SecretString` because the documented form embeds the mailbox password.
    #[config(secret)]
    #[serde(default)]
    pub url: Option<SecretString>,
    /// SMTP host (OVH Exchange: `pro3.mail.ovh.net` for STARTTLS or `ssl0.ovh.net` for TLS).
    #[serde(default)]
    pub host: Option<String>,
    /// SMTP port. Unset, it follows [`Self::security`]: 465 for `tls`, 587 for `starttls`, 25
    /// for `none`.
    #[serde(default)]
    pub port: Option<u16>,
    /// Login username. For OVH Exchange it is the full mailbox address.
    #[serde(default)]
    pub username: Option<String>,
    /// Login password or app password.
    #[config(secret)]
    #[serde(default)]
    pub password: Option<SecretString>,
    /// Transport security to use with the explicit host/port fields.
    #[config(values)]
    #[serde(default)]
    pub security: EmailSecurity,
    /// Default `From` mailbox, such as `TankoVault <no-reply@example.com>`. Required to send.
    #[serde(default)]
    pub from: Option<String>,
    /// SMTP envelope sender (`MAIL FROM`), which can differ from the visible [`Self::from`]
    /// header.
    ///
    /// Providers enforcing "send as" checks, such as OVH Exchange, reject a mismatched envelope
    /// sender, so unset means [`Self::username`].
    #[serde(default)]
    pub envelope_from: Option<String>,
    /// Public base URL of the web app, for absolute links in email such as the password-reset
    /// link. No trailing slash.
    #[serde(default = "EmailConfig::default_base_url")]
    pub base_url: String,
    /// Per-message send timeout, in seconds.
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

    /// Returns the configured port, or the default for [`Self::security`].
    #[must_use]
    pub fn effective_port(&self) -> u16 {
        self.port.unwrap_or(match self.security {
            EmailSecurity::Tls => 465,
            EmailSecurity::StartTls => 587,
            EmailSecurity::None => 25,
        })
    }

    /// Returns the envelope sender: the explicit override, else the login username.
    ///
    /// `None` makes the mailer use the `From:` header.
    #[must_use]
    pub fn effective_envelope_from(&self) -> Option<&str> {
        self.envelope_from
            .as_deref()
            .filter(|s| !s.is_empty())
            .or_else(|| self.username.as_deref().filter(|s| !s.is_empty()))
    }

    /// Returns whether a relay and a `From` address are both configured.
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
