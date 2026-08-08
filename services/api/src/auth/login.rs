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

/// How far a sign-in got.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum LoginStatus {
    /// Done — `session` carries the tokens and the refresh cookie is set.
    Authenticated,
    /// The password verified and a second factor is owed. `mfa` says how to present one, and
    /// **no session was issued**.
    MfaRequired,
}

/// What `POST /v1/auth/login` answers.
///
/// A discriminated struct rather than two response bodies on two status codes, or an untagged
/// union: one status and one schema means the generated client gets a single struct to
/// deserialise instead of a `oneOf` enum, which this generator renders awkwardly enough that it
/// has caused trouble here before. `status` is the field a client branches on; the two payloads
/// are mutually exclusive and each absent when the other is present.
///
/// [`TokenResponse`] is left untouched by this, so `refresh` and `register` — which answer with
/// a session and never a challenge — keep the shape they had.
#[derive(Debug, Serialize, ToSchema)]
pub struct LoginResponse {
    pub status: LoginStatus,
    // `nullable = false` on both payloads is load-bearing, not decoration. Without it utoipa
    // publishes an `Option<T>` as `oneOf [null, $ref]`, and the client generator turns *that*
    // into an untagged two-variant enum whose first variant is a bare `serde_json::Value` —
    // a type no caller can pattern-match usefully. Absent-vs-present is already expressed by
    // the field being outside `required`, which is what a client actually reads.
    /// Present iff `status` is `authenticated`.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub session: Option<TokenResponse>,
    /// Present iff `status` is `mfa_required`.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub mfa: Option<super::mfa::MfaChallenge>,
}

/// Log in
///
/// Authenticates by email or username + password.
///
/// Answers one of two things, told apart by `status`. An account with **no** second factor gets
/// a session outright, as before. An account **with** one gets a challenge and no session at
/// all — no access token, no refresh cookie — and finishes at `POST /v1/auth/mfa/verify`. That
/// is the whole point: a caller who has the password but not the factor must come away with
/// strictly less than they arrived with.
///
/// Every outcome — success, unknown identifier, bad password, unverified address, second factor
/// owed — is audited. An authentication log that only records successes cannot answer the
/// question anyone actually asks it after an incident.
#[utoipa::path(
    post,
    path = "/v1/auth/login",
    tag = AUTH_TAG,
    request_body = LoginRequest,
    responses(
        (status = 200, description = "Authenticated, or a second factor is owed", body = LoginResponse),
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
) -> ApiResult<(CookieJar, Json<LoginResponse>)> {
    let login = req.login.trim();
    let creds = verify_credentials(&state, &client, login, &req.password).await?;
    let uid = Some(creds.user.id);

    // The second leg, if this account has a factor. Note what is *not* done first: no session
    // is issued and then withdrawn, and `touch_last_login` is deferred to the leg that actually
    // signs in, so a half-finished attempt does not read as a login in the directory.
    if tankovault_db::repo::users::mfa::is_enrolled(&state.pool, creds.user.id).await? {
        let challenge = super::mfa::open_challenge(&state, creds.user.id).await?;
        audit_anonymous(
            &state,
            &client,
            uid,
            "auth.login",
            login,
            &serde_json::json!({ "reason": "mfa_required" }),
            AuditOutcome::Success,
        )
        .await;
        return Ok((
            jar,
            Json(LoginResponse {
                status: LoginStatus::MfaRequired,
                session: None,
                mfa: Some(challenge),
            }),
        ));
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

    let (jar, session) =
        crate::auth::session::issue_session_tokens(&state, jar, &creds.user, Uuid::now_v7())
            .await?;
    Ok((
        jar,
        Json(LoginResponse {
            status: LoginStatus::Authenticated,
            session: Some(session),
            mfa: None,
        }),
    ))
}

/// The first leg: resolve the identifier, verify the password, and refuse an account that may
/// not sign in at all.
///
/// Split out of [`login`] so the handler reads as "check the password, then check the second
/// factor" rather than as five refusal branches with a flow buried among them. Every branch here
/// is a refusal; the caller only ever sees credentials that passed all of them.
///
/// # Errors
/// [`ApiError::Unauthorized`] for an unknown identifier or a wrong password — deliberately the
/// same answer, and deliberately the same cost, since a faster "no such account" is an account
/// enumeration oracle. [`ApiError::Suspended`] and [`ApiError::EmailNotVerified`] are distinct
/// because the client can act on each.
async fn verify_credentials(
    state: &AppState,
    client: &ClientContext,
    login: &str,
    password: &SecretString,
) -> ApiResult<tankovault_db::repo::users::Credentials> {
    // The identifier is recorded on failures so a brute-force campaign can be attributed to
    // its target; the IP is separately gated behind the operator's privacy toggle.
    let Some(creds) = tankovault_db::repo::users::find_credentials(&state.pool, login).await?
    else {
        // Pay the argon2 cost anyway, or the timing gap between known and unknown identifiers
        // lets an attacker enumerate accounts. Result discarded; only elapsed time matters.
        let _ = verify_password(password, DUMMY_PASSWORD_HASH, &state.password_pepper);
        return Err(refuse(
            state,
            client,
            None,
            login,
            "unknown_identifier",
            AuditOutcome::Failure,
            ApiError::Unauthorized,
        )
        .await);
    };
    let uid = Some(creds.user.id);

    let ok = verify_password(password, &creds.password_hash, &state.password_pepper)
        .map_err(|_| ApiError::Internal)?;
    if !ok {
        return Err(refuse(
            state,
            client,
            uid,
            login,
            "bad_password",
            AuditOutcome::Failure,
            ApiError::Unauthorized,
        )
        .await);
    }

    // Checked here too (not just the `AuthUser` extractor), so a suspended account is told
    // so at sign-in rather than signing in and failing every request after.
    if !creds.user.status.may_authenticate() {
        return Err(refuse(
            state,
            client,
            uid,
            login,
            "account_suspended",
            AuditOutcome::Denied,
            ApiError::Suspended,
        )
        .await);
    }

    // Distinct from a bad password so the client can offer to resend the confirmation link.
    // Skipped when the operator has switched confirmation off, or existing accounts would be
    // stranded with no way to confirm.
    if !creds.email_verified
        && state
            .features
            .is_enabled(Feature::AccountsEmailVerification)
    {
        return Err(refuse(
            state,
            client,
            uid,
            login,
            "email_unverified",
            AuditOutcome::Denied,
            ApiError::EmailNotVerified,
        )
        .await);
    }

    Ok(creds)
}

/// Audit a refused sign-in and hand back the error to return.
///
/// Every refusal goes through here, so the shape of the record cannot drift between branches —
/// and so the list of reasons a sign-in can fail is readable as a list rather than as five
/// twelve-line blocks that differ in one string each.
async fn refuse(
    state: &AppState,
    client: &ClientContext,
    user_id: Option<tankovault_domain::UserId>,
    login: &str,
    reason: &str,
    outcome: AuditOutcome,
    error: ApiError,
) -> ApiError {
    audit_anonymous(
        state,
        client,
        user_id,
        "auth.login",
        login,
        &serde_json::json!({ "reason": reason }),
        outcome,
    )
    .await;
    error
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
