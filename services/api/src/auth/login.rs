//! Sign-in.
//!
//! Both branches — known and unknown identifier — verify a password hash, so the response
//! time does not disclose whether an account exists (SEC-10).

use axum::Json;
use axum::extract::State;
use axum_extra::extract::cookie::CookieJar;
use secrecy::SecretString;
use serde::{Deserialize, Serialize};
use tankovault_auth::verify_password;
use tankovault_domain::Feature;
use tankovault_service::AuditOutcome;
use utoipa::ToSchema;
use uuid::Uuid;

use super::session::issue_session;
use crate::audit::audit_anonymous;
use crate::error::{ApiError, ApiResult};
use crate::openapi::AUTH_TAG;
use crate::state::{AppState, ClientContext};

/// A real argon2id hash, verified against on the unknown-identifier branch of [`login`] so
/// both branches cost the same.
///
/// It must carry the *same* parameters the live hasher uses (`m=19456,t=2,p=1` —
/// `crates/auth::password`'s `Params::default()`), or the constant-time property is lost to
/// a parameter mismatch. The plaintext is irrelevant and no account has it; only the work
/// factor matters. Pinned by a test that fails if the two ever drift.
const DUMMY_PASSWORD_HASH: &str = "$argon2id$v=19$m=19456,t=2,p=1$\
    c29tZXNhbHRzb21lc2FsdA$YQNhOkeeqk3xJHTvR0mCFcRXA3vsSPT/9ObTNfMlLKw";

#[derive(Debug, Deserialize, ToSchema)]
pub struct LoginRequest {
    /// Email or username.
    pub login: String,
    // A `SecretString`, so the derived `Debug` on this struct renders `[REDACTED]` here. The
    // login path already audits every outcome and records the *identifier*; the submitted
    // password must never join it, and the wrapper is what makes a future `?req` safe rather
    // than merely absent today.
    //
    // `value_type = String` keeps the generated schema — and therefore `openapi.json` and
    // `crates/api-client` — byte-identical: this is a server-side representation change, not
    // a contract change. Deliberately a `//` comment and not a `///` one: utoipa publishes doc
    // comments as the field's `description`, and how this server holds the value in memory is
    // not something to tell every API consumer.
    #[schema(value_type = String)]
    pub password: SecretString,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct TokenResponse {
    // Wrapped in transit through the handler and unwrapped exactly once, by
    // `crate::secret::expose_onto_wire`, at the boundary where it is handed to the client —
    // which is the one place doing so is the entire point of the response.
    #[serde(serialize_with = "crate::secret::expose_onto_wire")]
    #[schema(value_type = String)]
    pub access_token: SecretString,
    pub token_type: &'static str,
    pub expires_in: i64,
}

/// Log in
///
/// Authenticates by email or username + password. Issues an access token and a rotating
/// refresh-token cookie.
///
/// Every outcome — success, unknown identifier, bad password, unverified address — is
/// audited. An authentication log that only records successes cannot answer the question
/// anyone actually asks it after an incident.
#[utoipa::path(
    post,
    path = "/v1/auth/login",
    tag = AUTH_TAG,
    request_body = LoginRequest,
    responses(
        (status = 200, description = "Authenticated; access token issued", body = TokenResponse),
        (status = 401, description = "Invalid email/username or password", body = crate::error::ProblemDetails),
        (status = 403, description = "Email address not yet confirmed", body = crate::error::ProblemDetails),
        (status = 429, description = "Too many attempts; retry later", body = crate::error::ProblemDetails),
    )
)]
pub async fn login(
    State(state): State<AppState>,
    client: ClientContext,
    jar: CookieJar,
    Json(req): Json<LoginRequest>,
) -> ApiResult<(CookieJar, Json<TokenResponse>)> {
    let login = req.login.trim();

    // The identifier is recorded on failures so a brute-force campaign can be attributed
    // to the account it targets. That is the point of an authentication log, and the
    // identifier is already stored in `users`; the *IP* is the field gated behind the
    // operator's privacy toggle, and the sink applies that.
    let Some(creds) = tankovault_db::repo::users::find_credentials(&state.pool, login).await?
    else {
        // Pay the argon2 cost anyway. Returning here directly took ~1 ms against ~30-60 ms
        // for a known identifier — two orders of magnitude, readable without statistics, so
        // an attacker could enumerate the whole user base by timing a breach corpus. The
        // result is discarded; only the elapsed time matters.
        let _ = verify_password(&req.password, DUMMY_PASSWORD_HASH, &state.password_pepper);
        audit_anonymous(
            &state,
            &client,
            None,
            "auth.login",
            login,
            &serde_json::json!({ "reason": "unknown_identifier" }),
            AuditOutcome::Failure,
        )
        .await;
        return Err(ApiError::Unauthorized);
    };

    let ok = verify_password(&req.password, &creds.password_hash, &state.password_pepper)
        .map_err(|_| ApiError::Internal)?;
    if !ok {
        audit_anonymous(
            &state,
            &client,
            Some(creds.user.id),
            "auth.login",
            login,
            &serde_json::json!({ "reason": "bad_password" }),
            AuditOutcome::Failure,
        )
        .await;
        return Err(ApiError::Unauthorized);
    }

    // The credential is valid, but the account may not be permitted to act. Checked here as
    // well as in the `AuthUser` extractor: refusing at the door is what makes the refusal
    // *legible* — the user is told they are suspended instead of signing in successfully and
    // then having every subsequent request fail.
    if !creds.user.status.may_authenticate() {
        audit_anonymous(
            &state,
            &client,
            Some(creds.user.id),
            "auth.login",
            login,
            &serde_json::json!({ "reason": "account_suspended" }),
            AuditOutcome::Denied,
        )
        .await;
        return Err(ApiError::Suspended);
    }

    // Password is correct, but an unconfirmed address may not sign in. Distinct from a bad
    // password (401) so the client can offer to resend the confirmation link. Accounts created
    // before confirmation existed, and those registered without a mailer, are already verified.
    //
    // Skipped entirely when the operator has switched confirmation off: leaving it enforced
    // would strand every account that registered while it was on, with no way to confirm.
    if !creds.email_verified
        && state
            .features
            .is_enabled(Feature::AccountsEmailVerification)
    {
        audit_anonymous(
            &state,
            &client,
            Some(creds.user.id),
            "auth.login",
            login,
            &serde_json::json!({ "reason": "email_unverified" }),
            AuditOutcome::Denied,
        )
        .await;
        return Err(ApiError::EmailNotVerified);
    }

    audit_anonymous(
        &state,
        &client,
        Some(creds.user.id),
        "auth.login",
        login,
        &serde_json::json!({}),
        AuditOutcome::Success,
    )
    .await;

    // Best-effort: the directory's "last seen" column is not worth failing a sign-in over.
    if let Err(e) = tankovault_db::repo::users::touch_last_login(&state.pool, creds.user.id).await {
        tracing::warn!(error = %e, "failed to record last login");
    }

    issue_session(&state, jar, &creds.user, Uuid::now_v7()).await
}

#[cfg(test)]
mod tests {
    use super::DUMMY_PASSWORD_HASH;
    use secrecy::{SecretSlice, SecretString};
    use tankovault_auth::{hash_password, verify_password};

    /// The dummy hash must **parse** and must carry the live hasher's parameters. If it does
    /// not parse, `verify_password` errors immediately and the unknown-identifier branch is
    /// fast again — restoring the enumeration oracle without any visible symptom.
    #[test]
    fn the_dummy_hash_is_a_real_argon2id_hash_with_the_live_parameters() {
        let live = hash_password(&SecretString::from("any password"), &SecretSlice::default());
        let live = live.expect("the live hasher produces a PHC string");
        let params = |h: &str| {
            h.split('$')
                .nth(3)
                .expect("PHC string has a parameter segment")
                .to_owned()
        };
        assert_eq!(
            params(DUMMY_PASSWORD_HASH),
            params(&live),
            "the dummy hash's work factor must match the live hasher's, or the two login \
             branches take measurably different time again"
        );

        // Parses, and does the work rather than erroring out of it.
        assert!(
            !verify_password(
                &SecretString::from("whatever"),
                DUMMY_PASSWORD_HASH,
                &SecretSlice::default()
            )
            .expect("the dummy hash parses as a PHC string"),
            "the dummy hash must not verify against an arbitrary password"
        );
    }
}
