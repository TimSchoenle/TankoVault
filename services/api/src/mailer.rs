//! Transactional-email composition and out-of-band delivery for the auth flows.
//!
//! Message *bodies* are built here (kept out of the handlers so the wording lives in one
//! place); the actual send goes through the [`tankovault_email::EmailService`] on
//! [`AppState`]. All sends are fire-and-forget: a mail outage must never fail — or slow —
//! a user-facing request like sign-up or a reset request.

use crate::state::AppState;
use tankovault_email::EmailMessage;

/// Compose the welcome email sent after a successful registration.
#[must_use]
pub fn welcome(email: &str, username: &str) -> EmailMessage {
    let text = format!(
        "Hi {username},\n\n\
         Welcome to TankoVault! Your account is ready — sign in any time to start tracking \
         your series and get notified about new chapters.\n\n\
         If you didn't create this account, you can safely ignore this email.\n\n\
         — The TankoVault team\n"
    );
    let html = format!(
        "<p>Hi {username},</p>\
         <p>Welcome to <strong>TankoVault</strong>! Your account is ready — sign in any time \
         to start tracking your series and get notified about new chapters.</p>\
         <p>If you didn't create this account, you can safely ignore this email.</p>\
         <p>— The TankoVault team</p>"
    );
    EmailMessage::text(email, "Welcome to TankoVault", text).with_html(html)
}

/// Compose the password-reset email carrying the one-time `link`.
#[must_use]
pub fn password_reset(email: &str, link: &str) -> EmailMessage {
    let text = format!(
        "We received a request to reset your TankoVault password.\n\n\
         Open the link below to choose a new one. It expires in 1 hour and can be used \
         once:\n\n{link}\n\n\
         If you didn't request this, you can ignore this email — your password stays \
         unchanged.\n\n\
         — The TankoVault team\n"
    );
    let html = format!(
        "<p>We received a request to reset your TankoVault password.</p>\
         <p>Click the button below to choose a new one. It expires in 1 hour and can be \
         used once.</p>\
         <p><a href=\"{link}\">Reset your password</a></p>\
         <p>If you didn't request this, you can ignore this email — your password stays \
         unchanged.</p>\
         <p>— The TankoVault team</p>"
    );
    EmailMessage::text(email, "Reset your TankoVault password", text).with_html(html)
}

/// Send `message` on a detached task so the request path never blocks on (or fails
/// because of) the relay. No-ops cheaply when email is unconfigured.
pub fn send_in_background(state: &AppState, message: EmailMessage) {
    if !state.mailer.is_enabled() {
        return;
    }
    let mailer = state.mailer.clone();
    tokio::spawn(async move {
        if let Err(e) = mailer.send(message).await {
            tracing::warn!(error = %e, "failed to send transactional email");
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn welcome_addresses_the_user_and_sets_subject() {
        let msg = welcome("reader@example.com", "aster");
        assert_eq!(msg.to, vec!["reader@example.com".to_owned()]);
        assert_eq!(msg.subject, "Welcome to TankoVault");
        assert!(msg.text.contains("Hi aster,"));
        assert!(msg.html.as_deref().unwrap().contains("aster"));
    }

    #[test]
    fn password_reset_embeds_the_link_in_both_parts() {
        let link = "https://app.example.com/reset-password?token=abc";
        let msg = password_reset("reader@example.com", link);
        assert_eq!(msg.subject, "Reset your TankoVault password");
        assert!(msg.text.contains(link));
        assert!(msg.html.as_deref().unwrap().contains(link));
    }
}
