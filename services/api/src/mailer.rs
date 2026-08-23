//! Transactional-email composition and out-of-band delivery for the auth flows.
//!
//! All sends are fire-and-forget, so a mail outage never fails or slows a user-facing request.

use crate::state::AppState;
use tankovault_email::EmailMessage;

/// HTML-escape an interpolated value.
///
/// `username` is user-controlled and reaches the HTML bodies below. Unescaped, it can inject
/// an attacker-chosen link into a DKIM-signed message genuinely from this service — a
/// high-credibility phishing primitive.
///
/// `&` first, or the escapes escape each other.
fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(ch),
        }
    }
    out
}

/// Compose the welcome email sent after a successful registration.
///
/// `brand` is the operator's product name, and it is escaped into the HTML bodies below for the
/// same reason `username` is: it reaches markup, and an `&` in a perfectly innocent name would
/// otherwise produce a broken entity in a DKIM-signed message.
#[must_use]
pub fn welcome(brand: &str, email: &str, username: &str) -> EmailMessage {
    let username_html = esc(username);
    let brand_html = esc(brand);
    let text = format!(
        "Hi {username},\n\n\
         Welcome to {brand}! Your account is ready — sign in any time to start tracking \
         your series and get notified about new chapters.\n\n\
         If you didn't create this account, you can safely ignore this email.\n\n\
         — The {brand} team\n"
    );
    let html = format!(
        "<p>Hi {username_html},</p>\
         <p>Welcome to <strong>{brand_html}</strong>! Your account is ready — sign in any time \
         to start tracking your series and get notified about new chapters.</p>\
         <p>If you didn't create this account, you can safely ignore this email.</p>\
         <p>— The {brand_html} team</p>"
    );
    EmailMessage::text(email, format!("Welcome to {brand}"), text).with_html(html)
}

/// Compose the email-confirmation message carrying the one-time verification `link`.
#[must_use]
pub fn verification(brand: &str, email: &str, username: &str, link: &str) -> EmailMessage {
    let username_html = esc(username);
    let brand_html = esc(brand);
    let text = format!(
        "Hi {username},\n\n\
         Thanks for signing up for {brand}! Please confirm your email address to \
         activate your account by opening the link below. It expires in 24 hours:\n\n{link}\n\n\
         If you didn't create this account, you can safely ignore this email.\n\n\
         — The {brand} team\n"
    );
    let html = format!(
        "<p>Hi {username_html},</p>\
         <p>Thanks for signing up for <strong>{brand_html}</strong>! Please confirm your email \
         address to activate your account. The link expires in 24 hours.</p>\
         <p><a href=\"{link}\">Confirm your email</a></p>\
         <p>If you didn't create this account, you can safely ignore this email.</p>\
         <p>— The {brand_html} team</p>"
    );
    EmailMessage::text(email, format!("Confirm your {brand} email"), text).with_html(html)
}

/// Compose the password-reset email carrying the one-time `link`.
#[must_use]
pub fn password_reset(brand: &str, email: &str, link: &str) -> EmailMessage {
    let brand_html = esc(brand);
    let text = format!(
        "We received a request to reset your {brand} password.\n\n\
         Open the link below to choose a new one. It expires in 1 hour and can be used \
         once:\n\n{link}\n\n\
         If you didn't request this, you can ignore this email — your password stays \
         unchanged.\n\n\
         — The {brand} team\n"
    );
    let html = format!(
        "<p>We received a request to reset your {brand_html} password.</p>\
         <p>Click the button below to choose a new one. It expires in 1 hour and can be \
         used once.</p>\
         <p><a href=\"{link}\">Reset your password</a></p>\
         <p>If you didn't request this, you can ignore this email — your password stays \
         unchanged.</p>\
         <p>— The {brand_html} team</p>"
    );
    EmailMessage::text(email, format!("Reset your {brand} password"), text).with_html(html)
}

/// Compose the notice sent to the **old** address when the account's email is changed.
///
/// Sent to the address being replaced — the only inbox that can warn the legitimate owner an
/// attacker is walking off with their account. Deliberately carries no action link, since a
/// link in a "something changed" email is exactly the phishing shape it warns about.
#[must_use]
pub fn email_changed(
    brand: &str,
    old_email: &str,
    username: &str,
    new_email: &str,
) -> EmailMessage {
    let username_html = esc(username);
    let new_email_html = esc(new_email);
    let brand_html = esc(brand);
    let text = format!(
        "Hi {username},\n\n\
         The email address on your {brand} account was just changed to {new_email}. \
         Sign-in and password-reset messages will go there from now on.\n\n\
         If you did not make this change, someone else has access to your account. Contact \
         an administrator immediately — you will not be able to reset the password yourself, \
         because reset links now go to the new address.\n\n\
         — The {brand} team\n"
    );
    let html = format!(
        "<p>Hi {username_html},</p>\
         <p>The email address on your {brand_html} account was just changed to \
         <strong>{new_email_html}</strong>. Sign-in and password-reset messages will go there \
         from now on.</p>\
         <p>If you did not make this change, someone else has access to your account. Contact \
         an administrator immediately — you will not be able to reset the password yourself, \
         because reset links now go to the new address.</p>\
         <p>— The {brand_html} team</p>"
    );
    EmailMessage::text(
        old_email,
        format!("Your {brand} email address was changed"),
        text,
    )
    .with_html(html)
}

/// Send `message` on a detached task so the request path never blocks on (or fails
/// because of) the relay. No-ops cheaply when email is unconfigured.
pub fn send_in_background(state: &AppState, message: EmailMessage) {
    if !state.mailer.is_enabled() {
        return;
    }
    let mailer = state.mailer.clone();
    tokio::spawn(tankovault_service::in_current_trace(async move {
        if let Err(e) = mailer.send(message).await {
            tracing::warn!(error = %e, "failed to send transactional email");
        }
    }));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The name a stock deployment carries, so these assertions read as the shipped output.
    const BRAND: &str = "TankoVault";

    #[test]
    fn welcome_addresses_the_user_and_sets_subject() {
        let msg = welcome(BRAND, "reader@example.com", "aster");
        assert_eq!(msg.to, vec!["reader@example.com".to_owned()]);
        assert_eq!(msg.subject, "Welcome to TankoVault");
        assert!(msg.text.contains("Hi aster,"));
        assert!(msg.html.as_deref().unwrap().contains("aster"));
    }

    #[test]
    fn verification_embeds_the_link_and_addresses_the_user() {
        let link = "https://app.example.com/verify-email?token=abc";
        let msg = verification(BRAND, "reader@example.com", "aster", link);
        assert_eq!(msg.subject, "Confirm your TankoVault email");
        assert!(msg.text.contains("Hi aster,"));
        assert!(msg.text.contains(link));
        assert!(msg.html.as_deref().unwrap().contains(link));
    }

    #[test]
    fn password_reset_embeds_the_link_in_both_parts() {
        let link = "https://app.example.com/reset-password?token=abc";
        let msg = password_reset(BRAND, "reader@example.com", link);
        assert_eq!(msg.subject, "Reset your TankoVault password");
        assert!(msg.text.contains(link));
        assert!(msg.html.as_deref().unwrap().contains(link));
    }

    /// Every message names the deployment, not this project — subject and both bodies.
    #[test]
    fn a_renamed_deployment_names_itself_everywhere() {
        let link = "https://app.example.com/verify";
        for msg in [
            welcome("MangaBox", "reader@example.com", "aster"),
            verification("MangaBox", "reader@example.com", "aster", link),
            password_reset("MangaBox", "reader@example.com", link),
            email_changed("MangaBox", "old@example.com", "aster", "new@example.com"),
        ] {
            assert!(msg.subject.contains("MangaBox"), "{}", msg.subject);
            assert!(!msg.text.contains("TankoVault"), "{}", msg.text);
            let html = msg.html.as_deref().unwrap();
            assert!(html.contains("MangaBox"), "{html}");
            assert!(!html.contains("TankoVault"), "{html}");
        }
    }

    /// Regression: an unescaped username let a user inject an arbitrary anchor into a
    /// DKIM-signed message from this service.
    #[test]
    fn a_username_cannot_inject_markup_into_the_html_body() {
        let hostile = r#"x</p><a href="https://evil.tld">reset</a><p>"#;
        for msg in [
            welcome(BRAND, "reader@example.com", hostile),
            verification(BRAND, "reader@example.com", hostile, "https://app/verify"),
            email_changed(BRAND, "old@example.com", hostile, "new@example.com"),
        ] {
            let html = msg.html.as_deref().unwrap();
            assert!(!html.contains("<a href=\"https://evil.tld\">"), "{html}");
            assert!(html.contains("&lt;/p&gt;"), "{html}");
        }
    }

    /// The product name reaches the same HTML bodies the username does, so it takes the same
    /// escaping — an operator naming their deployment "Tom & Jerry" must not emit a broken
    /// entity, and configuration is not a licence to write raw markup into signed mail.
    #[test]
    fn the_product_name_is_escaped_into_the_html_body() {
        let msg = welcome("<em>Ink</em> & Vault", "reader@example.com", "aster");
        let html = msg.html.as_deref().unwrap();
        assert!(!html.contains("<em>"), "{html}");
        assert!(
            html.contains("&lt;em&gt;Ink&lt;/em&gt; &amp; Vault"),
            "{html}"
        );
    }

    #[test]
    fn esc_covers_the_five_html_metacharacters() {
        assert_eq!(esc(r#"&<>"'"#), "&amp;&lt;&gt;&quot;&#39;");
        // `&` must be handled first or the other escapes get double-escaped.
        assert_eq!(esc("a&b"), "a&amp;b");
        assert_eq!(esc("plain"), "plain");
    }

    /// The notice goes to the address being *replaced* — that is the only inbox that can warn
    /// the legitimate owner — and carries no link, because a link is the phishing shape.
    #[test]
    fn the_change_notice_targets_the_old_address_and_carries_no_link() {
        let msg = email_changed(BRAND, "old@example.com", "aster", "attacker@evil.tld");
        assert_eq!(msg.to, vec!["old@example.com".to_owned()]);
        assert!(msg.text.contains("attacker@evil.tld"));
        assert!(!msg.html.as_deref().unwrap().contains("<a href"));
    }
}
