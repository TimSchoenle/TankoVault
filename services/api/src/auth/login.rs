//! Sign-in.
//!
//! Both branches — known and unknown identifier — verify a password hash, so the response
//! time does not disclose whether an account exists.

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

/// A real argon2id hash used on the unknown-identifier branch of [`login`] so both branches
/// cost the same.
///
/// Must carry the same parameters as the live hasher (`m=19456,t=2,p=1`), or the
/// constant-time property breaks silently. Pinned by a test that fails if the two drift.
const DUMMY_PASSWORD_HASH: &str = "$argon2id$v=19$m=19456,t=2,p=1$\
    c29tZXNhbHRzb21lc2FsdA$YQNhOkeeqk3xJHTvR0mCFcRXA3vsSPT/9ObTNfMlLKw";

#[derive(Debug, Deserialize, ToSchema)]
pub struct LoginRequest {
    /// Email or username.
    pub login: String,
    // `SecretString` so `Debug`/logs can't leak it. `value_type = String` keeps the schema
    // unchanged; kept as `//` not `///` since utoipa would publish this as the field description.
    #[schema(value_type = String)]
    pub password: SecretString,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct TokenResponse {
    // Unwrapped exactly once, by `expose_onto_wire`, at the boundary handing it to the client.
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

    // The identifier is recorded on failures so a brute-force campaign can be attributed to
    // its target; the IP is separately gated behind the operator's privacy toggle.
    let Some(creds) = tankovault_db::repo::users::find_credentials(&state.pool, login).await?
    else {
        // Pay the argon2 cost anyway, or the timing gap between known and unknown identifiers
        // lets an attacker enumerate accounts. Result discarded; only elapsed time matters.
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

    // Checked here too (not just the `AuthUser` extractor), so a suspended account is told
    // so at sign-in rather than signing in and failing every request after.
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

    // Distinct from a bad password so the client can offer to resend the confirmation link.
    // Skipped when the operator has switched confirmation off, or existing accounts would be
    // stranded with no way to confirm.
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
