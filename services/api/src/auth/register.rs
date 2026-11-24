//! Owns account creation.
//!
//! Two outcomes by design: with email delivery configured the account is created
//! **unconfirmed** and no session is issued; without it the account is active immediately, so
//! development and SMTP-less self-hosting keep working.

use axum::Json;
use axum::extract::State;
use axum_extra::extract::cookie::CookieJar;
use secrecy::SecretString;
use serde::{Deserialize, Serialize};
use tankovault_db::DbError;
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
    // Wrapped so the derived `Debug` can't print it; see `LoginRequest::password` for why
    // `value_type = String` keeps the schema unchanged.
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
    /// The issued bearer access token, present only when the account was activated
    /// immediately (email delivery not configured). Mirrors [`TokenResponse::access_token`].
    // Includes the `expose_option_onto_wire` opt-in that lets this secret reach the client.
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
/// is created **unconfirmed** and a confirmation link is emailed; the user must click it
/// before they can sign in, and no session is issued. When email is not configured the
/// account is activated immediately and a session is issued exactly like [`login`], so
/// development and SMTP-less self-hosting keep working.
///
/// With confirmation on, an already registered email gets the same response as a new one and
/// its owner is emailed a notice instead, so the endpoint does not reveal who has an account.
/// A username held by a confirmed account is still refused, since the user has to pick another;
/// one held only by an unconfirmed sign-up is released to the new registration.
#[utoipa::path(
    post,
    path = "/v1/auth/register",
    tag = AUTH_TAG,
    request_body = RegisterRequest,
    responses(
        (status = 200, description = "Account created; either a confirmation email was sent or (email unconfigured) an access token was issued", body = RegisterResponse),
        (status = 400, description = "Invalid email, username or password", body = crate::error::ProblemDetails),
        (status = 409, description = "Username held by a confirmed account, or (email confirmation off) email or username already taken", body = crate::error::ProblemDetails),
    )
)]
pub async fn register(
    State(state): State<AppState>,
    jar: CookieJar,
    Json(req): Json<RegisterRequest>,
) -> ApiResult<(CookieJar, Json<RegisterResponse>)> {
    validate_registration(&req)?;
    let email = req.email.trim();
    let username = req.username.trim();
    // Hashed before the email lookup so a taken address costs the same KDF time as a new one.
    let hash = super::kdf::hash_password(&req.password, &state.password_pepper).await?;

    // Confirmation needs both a mailer and an operator who wants the step; a missing mailer
    // is the involuntary version of switching it off.
    let verifying = state.mailer.is_enabled()
        && state
            .features
            .is_enabled(Feature::AccountsEmailVerification);

    // Without confirmation a fresh registration returns a session, which a taken email cannot
    // fake, so that mode keeps the plain 409.
    if verifying {
        // The username is checked before the email: otherwise a known username paired with a
        // registered email answers 200 and with an unused one 409, which probes emails.
        if username_confirmed(&state, username).await? {
            return Err(ApiError::Conflict("username already taken".to_owned()));
        }
        if notify_existing_owner(&state, email).await? {
            return Ok((jar, Json(RegisterResponse::verification_sent())));
        }
        // Released only after the email check, so a registered email leaves no trace. If an
        // unconfirmed sign-up kept its name, a second registration with that name would answer
        // 409 only when the first one's email had been new.
        tankovault_db::repo::users::release_unconfirmed_username(&state.pool, username).await?;
    }
    let user = match tankovault_db::repo::users::create(&state.pool, email, username, &hash).await {
        Ok(user) => user,
        Err(DbError::Conflict(_)) if verifying => {
            // The email may have been registered since the lookup above; only a username
            // collision is reported.
            if notify_existing_owner(&state, email).await? {
                return Ok((jar, Json(RegisterResponse::verification_sent())));
            }
            return Err(ApiError::Conflict("username already taken".to_owned()));
        }
        Err(e) => return Err(e.into()),
    };

    if verifying {
        // Send the link out of band and issue no session; the welcome email waits until
        // confirmed (see [`verify_email`]).
        send_verification_email(&state, &user).await?;
        return Ok((jar, Json(RegisterResponse::verification_sent())));
    }

    // Confirmation isn't in play: activate and sign in immediately, so dev/CI and
    // switched-off deployments keep a one-step sign-up.
    tankovault_db::repo::users::mark_email_verified(&state.pool, user.id).await?;
    mailer::send_in_background(
        &state,
        mailer::welcome(state.branding.name(), &user.email, &user.username),
    );
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

impl RegisterResponse {
    /// The answer to a registration awaiting confirmation, identical for a taken email.
    fn verification_sent() -> Self {
        Self {
            verification_required: true,
            access_token: None,
            expires_in: None,
        }
    }
}

/// Emails the owner of `email` that someone tried to register with it; `false` if unregistered.
async fn notify_existing_owner(state: &AppState, email: &str) -> ApiResult<bool> {
    let Some(owner) = tankovault_db::repo::users::find_by_email(&state.pool, email).await? else {
        return Ok(false);
    };
    mailer::send_in_background(
        state,
        mailer::registration_attempted(state.branding.name(), &owner.email, &owner.username),
    );
    Ok(true)
}

/// Whether `username` belongs to an account whose email has been confirmed.
///
/// `validate_username` rejects `@`, so [`find_credentials`] matches the username column only.
///
/// [`find_credentials`]: tankovault_db::repo::users::find_credentials
async fn username_confirmed(state: &AppState, username: &str) -> ApiResult<bool> {
    Ok(
        tankovault_db::repo::users::find_credentials(&state.pool, username)
            .await?
            .is_some_and(|holder| holder.email_verified),
    )
}

/// Validates every field a registration writes, in the order the user filled them in.
fn validate_registration(req: &RegisterRequest) -> ApiResult<()> {
    validate_email(req.email.trim())?;
    validate_username(req.username.trim())?;
    validate_password(&req.password)
}
