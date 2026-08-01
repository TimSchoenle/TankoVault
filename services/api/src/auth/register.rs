//! Account creation.
//!
//! Two outcomes by design: with email delivery configured the account is created
//! **unconfirmed** and no session is issued; without it the account is active immediately, so
//! development and SMTP-less self-hosting keep working.

use axum::Json;
use axum::extract::State;
use axum_extra::extract::cookie::CookieJar;
use secrecy::SecretString;
use serde::{Deserialize, Serialize};
use tankovault_auth::hash_password;
use tankovault_domain::Feature;
use utoipa::ToSchema;
use uuid::Uuid;

use super::session::issue_session_tokens;
use super::validate::{validate_email, validate_password, validate_username};
use super::verification::send_verification_email;
use crate::error::{ApiError, ApiResult};
use crate::mailer;
use crate::openapi::AUTH_TAG;
use crate::state::AppState;

#[derive(Debug, Deserialize, ToSchema)]
pub struct RegisterRequest {
    pub email: String,
    pub username: String,
    // Wrapped so the derived `Debug` on this struct cannot print it. See
    // `super::login::LoginRequest::password` for why `value_type = String` keeps the generated
    // schema unchanged, and why this rationale is not a doc comment.
    #[schema(value_type = String)]
    pub password: SecretString,
}

/// The result of [`register`]. Registration has two outcomes depending on whether email
/// delivery is configured:
///
/// - Email enabled: the account is created **unconfirmed**, a confirmation link is emailed,
///   and no session is issued (`verification_required = true`, `access_token` absent). The
///   user must click the link before they can sign in.
/// - Email not configured (dev/self-host without SMTP): confirmation cannot be delivered, so
///   the account is activated immediately and a session is issued exactly like
///   [`super::login::login`] (`verification_required = false`, `access_token` present).
#[derive(Debug, Serialize, ToSchema)]
pub struct RegisterResponse {
    /// `true` when a confirmation email was sent and the account must be verified before it
    /// can sign in. When `true`, no session was issued and `access_token` is absent.
    pub verification_required: bool,
    /// The issued bearer access token — present only when the account was activated
    /// immediately (email delivery not configured). Mirrors [`TokenResponse::access_token`].
    // Including its `expose_option_onto_wire` opt-in, which is what lets a secret reach the
    // client at all.
    #[serde(
        skip_serializing_if = "Option::is_none",
        serialize_with = "crate::secret::expose_option_onto_wire"
    )]
    #[schema(value_type = Option<String>)]
    pub access_token: Option<SecretString>,
    /// Access-token lifetime in seconds; present exactly when `access_token` is.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_in: Option<i64>,
}

/// Register a new account
///
/// Validates the request and creates the user. When email delivery is configured the account
/// is created **unconfirmed** and a confirmation link is emailed — the user must click it
/// before they can sign in, and no session is issued. When email is not configured the
/// account is activated immediately and a session is issued exactly like [`login`], so
/// development and SMTP-less self-hosting keep working.
#[utoipa::path(
    post,
    path = "/v1/auth/register",
    tag = AUTH_TAG,
    request_body = RegisterRequest,
    responses(
        (status = 200, description = "Account created; either a confirmation email was sent or (email unconfigured) an access token was issued", body = RegisterResponse),
        (status = 400, description = "Invalid email, username or password", body = crate::error::ProblemDetails),
        (status = 409, description = "Email or username already taken", body = crate::error::ProblemDetails),
    )
)]
pub async fn register(
    State(state): State<AppState>,
    jar: CookieJar,
    Json(req): Json<RegisterRequest>,
) -> ApiResult<(CookieJar, Json<RegisterResponse>)> {
    validate_registration(&req)?;
    let hash =
        hash_password(&req.password, &state.password_pepper).map_err(|_| ApiError::Internal)?;
    let user = tankovault_db::repo::users::create(
        &state.pool,
        req.email.trim(),
        req.username.trim(),
        &hash,
    )
    .await?;

    // Confirmation needs both a way to deliver the link *and* an operator who wants the step.
    // Turning `accounts.email_verification` off is how a deployment with working mail
    // deliberately skips it; a missing mailer is the involuntary version of the same thing.
    if state.mailer.is_enabled()
        && state
            .features
            .is_enabled(Feature::AccountsEmailVerification)
    {
        // Email delivery is available: require confirmation before the account can sign in.
        // Send the confirmation link out of band and issue no session — the welcome email is
        // deferred until the address is actually confirmed (see [`verify_email`]).
        send_verification_email(&state, &user).await?;
        return Ok((
            jar,
            Json(RegisterResponse {
                verification_required: true,
                access_token: None,
                expires_in: None,
            }),
        ));
    }

    // Confirmation is not in play: activate the account immediately and log the user straight
    // in, preserving the pre-confirmation sign-up experience for dev/CI and for deployments
    // that have switched the step off.
    tankovault_db::repo::users::mark_email_verified(&state.pool, user.id).await?;
    mailer::send_in_background(&state, mailer::welcome(&user.email, &user.username));
    let (jar, token) = issue_session_tokens(&state, jar, &user, Uuid::now_v7()).await?;
    Ok((
        jar,
        Json(RegisterResponse {
            verification_required: false,
            access_token: Some(token.access_token),
            expires_in: Some(token.expires_in),
        }),
    ))
}

/// Validate every field a registration writes, in the order the user filled them in.
fn validate_registration(req: &RegisterRequest) -> ApiResult<()> {
    validate_email(req.email.trim())?;
    validate_username(req.username.trim())?;
    validate_password(&req.password)
}
