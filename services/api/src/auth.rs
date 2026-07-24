//! Authentication handlers: register, login, refresh (rotating + reuse-detecting), logout.

use crate::error::{ApiError, ApiResult};
use crate::mailer;
use crate::state::AppState;
use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum_extra::extract::cookie::{Cookie, CookieJar, SameSite};
use serde::{Deserialize, Serialize};
use tankovault_auth::{
    generate_refresh_token, hash_password, hash_refresh_token, issue_access_token, verify_password,
};
use tankovault_domain::{User, UserRole};
use time::{Duration, OffsetDateTime};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::openapi::AUTH_TAG;

const REFRESH_COOKIE: &str = "refresh_token";
const REFRESH_PATH: &str = "/v1/auth";

/// How long a password-reset link stays valid. Short by design: long enough to arrive by
/// email and be clicked, short enough to limit the blast radius of a leaked inbox.
const RESET_TOKEN_TTL: Duration = Duration::hours(1);

/// How long an email-confirmation link stays valid. Longer than a reset link since a new
/// user may not check their inbox immediately, but still bounded so stale links expire.
const VERIFY_TOKEN_TTL: Duration = Duration::hours(24);

#[derive(Debug, Deserialize, ToSchema)]
pub struct RegisterRequest {
    pub email: String,
    pub username: String,
    pub password: String,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct LoginRequest {
    /// Email or username.
    pub login: String,
    pub password: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct TokenResponse {
    pub access_token: String,
    pub token_type: &'static str,
    pub expires_in: i64,
}

/// The result of [`register`]. Registration has two outcomes depending on whether email
/// delivery is configured:
///
/// - Email enabled: the account is created **unconfirmed**, a confirmation link is emailed,
///   and no session is issued (`verification_required = true`, `access_token` absent). The
///   user must click the link before they can sign in.
/// - Email not configured (dev/self-host without SMTP): confirmation cannot be delivered, so
///   the account is activated immediately and a session is issued exactly like [`login`]
///   (`verification_required = false`, `access_token` present).
#[derive(Debug, Serialize, ToSchema)]
pub struct RegisterResponse {
    /// `true` when a confirmation email was sent and the account must be verified before it
    /// can sign in. When `true`, no session was issued and `access_token` is absent.
    pub verification_required: bool,
    /// The issued bearer access token — present only when the account was activated
    /// immediately (email delivery not configured). Mirrors [`TokenResponse::access_token`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub access_token: Option<String>,
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
    let hash = hash_password(&req.password).map_err(|_| ApiError::Internal)?;
    let user = tankovault_db::repo::users::create(
        &state.pool,
        req.email.trim(),
        req.username.trim(),
        &hash,
        UserRole::User,
    )
    .await?;

    if state.mailer.is_enabled() {
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

    // No mailer: confirmation can't be delivered, so activate the account immediately and log
    // the user straight in, preserving the pre-confirmation sign-up experience for dev/CI.
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

/// Log in
///
/// Authenticates by email or username + password. Issues an access token and a rotating
/// refresh-token cookie.
#[utoipa::path(
    post,
    path = "/v1/auth/login",
    tag = AUTH_TAG,
    request_body = LoginRequest,
    responses(
        (status = 200, description = "Authenticated; access token issued", body = TokenResponse),
        (status = 401, description = "Invalid email/username or password", body = crate::error::ProblemDetails),
        (status = 403, description = "Email address not yet confirmed", body = crate::error::ProblemDetails),
    )
)]
pub async fn login(
    State(state): State<AppState>,
    jar: CookieJar,
    Json(req): Json<LoginRequest>,
) -> ApiResult<(CookieJar, Json<TokenResponse>)> {
    let creds = tankovault_db::repo::users::find_credentials(&state.pool, req.login.trim())
        .await?
        .ok_or(ApiError::Unauthorized)?;
    let ok =
        verify_password(&req.password, &creds.password_hash).map_err(|_| ApiError::Internal)?;
    if !ok {
        return Err(ApiError::Unauthorized);
    }
    // Password is correct, but an unconfirmed address may not sign in. Distinct from a bad
    // password (401) so the client can offer to resend the confirmation link. Accounts created
    // before confirmation existed, and those registered without a mailer, are already verified.
    if !creds.email_verified {
        return Err(ApiError::EmailNotVerified);
    }
    issue_session(&state, jar, &creds.user, Uuid::now_v7()).await
}

/// Refresh the access token
///
/// Reads the `refresh_token` `HttpOnly` cookie, rotates it, and issues a fresh access token.
/// Presenting a token that was already rotated (reuse) revokes the whole token family.
#[utoipa::path(
    post,
    path = "/v1/auth/refresh",
    tag = AUTH_TAG,
    responses(
        (status = 200, description = "Refreshed; new access token issued", body = TokenResponse),
        (status = 401, description = "Missing, expired, or already-rotated refresh token", body = crate::error::ProblemDetails),
    )
)]
pub async fn refresh(
    State(state): State<AppState>,
    jar: CookieJar,
) -> ApiResult<(CookieJar, Json<TokenResponse>)> {
    let raw = jar
        .get(REFRESH_COOKIE)
        .map(|c| c.value().to_owned())
        .ok_or(ApiError::Unauthorized)?;
    let token_hash = hash_refresh_token(&raw);

    let record = tankovault_db::repo::users::find_refresh(&state.pool, &token_hash)
        .await?
        .ok_or(ApiError::Unauthorized)?;

    // Reuse detection: a token presented after it was already rotated (revoked) means the
    // family is compromised — revoke the whole lineage.
    if record.revoked_at.is_some() {
        tankovault_db::repo::users::revoke_family(&state.pool, record.family_id).await?;
        return Err(ApiError::Unauthorized);
    }
    if record.expires_at <= OffsetDateTime::now_utc() {
        return Err(ApiError::Unauthorized);
    }

    // Rotate: revoke the presented token, mint a new one in the same family.
    tankovault_db::repo::users::revoke_token(&state.pool, record.id).await?;
    let user = tankovault_db::repo::users::get(&state.pool, record.user_id).await?;
    issue_session(&state, jar, &user, record.family_id).await
}

/// Log out
///
/// Revokes the presented refresh-token family (if any) and clears the cookie.
#[utoipa::path(
    post,
    path = "/v1/auth/logout",
    tag = AUTH_TAG,
    responses((status = 200, description = "Logged out; refresh cookie cleared"))
)]
pub async fn logout(State(state): State<AppState>, jar: CookieJar) -> ApiResult<CookieJar> {
    if let Some(raw) = jar.get(REFRESH_COOKIE).map(|c| c.value().to_owned()) {
        if let Some(record) =
            tankovault_db::repo::users::find_refresh(&state.pool, &hash_refresh_token(&raw)).await?
        {
            tankovault_db::repo::users::revoke_family(&state.pool, record.family_id).await?;
        }
    }
    let removal = Cookie::build((REFRESH_COOKIE, ""))
        .path(REFRESH_PATH)
        .build();
    Ok(jar.remove(removal))
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct ForgotPasswordRequest {
    /// The account's email address. Unknown addresses are accepted silently.
    pub email: String,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct ResetPasswordRequest {
    /// The opaque token from the emailed reset link.
    pub token: String,
    /// The new password (same policy as registration: at least 8 characters).
    pub new_password: String,
}

/// Request a password-reset email
///
/// Always responds `202 Accepted`, whether or not the address is registered, so the
/// endpoint can't be used to probe which emails have accounts. When the address does exist
/// and email is configured, a single-use, time-limited reset link is sent.
#[utoipa::path(
    post,
    path = "/v1/auth/password/forgot",
    tag = AUTH_TAG,
    request_body = ForgotPasswordRequest,
    responses(
        (status = 202, description = "If the address is registered, a reset email has been sent"),
    )
)]
pub async fn forgot_password(
    State(state): State<AppState>,
    Json(req): Json<ForgotPasswordRequest>,
) -> ApiResult<StatusCode> {
    let email = req.email.trim();
    if let Some(user) = tankovault_db::repo::users::find_by_email(&state.pool, email).await? {
        // Reuse the high-entropy opaque-token generator; only the SHA-256 hash is stored.
        let raw = generate_refresh_token();
        let token_hash = hash_refresh_token(&raw);
        let expires_at = OffsetDateTime::now_utc() + RESET_TOKEN_TTL;
        tankovault_db::repo::users::insert_password_reset(
            &state.pool,
            user.id,
            &token_hash,
            expires_at,
        )
        .await?;

        let link = format!(
            "{}/reset-password?token={raw}",
            state.email_base_url.trim_end_matches('/'),
        );
        mailer::send_in_background(&state, mailer::password_reset(&user.email, &link));
    }
    // Uniform response regardless of whether the account exists.
    Ok(StatusCode::ACCEPTED)
}

/// Reset a password with a token
///
/// Consumes a valid, unexpired, unused reset token, sets the new password, and revokes all
/// of the user's active sessions so any stolen refresh token dies with the old credential.
#[utoipa::path(
    post,
    path = "/v1/auth/password/reset",
    tag = AUTH_TAG,
    request_body = ResetPasswordRequest,
    responses(
        (status = 200, description = "Password changed; existing sessions revoked"),
        (status = 400, description = "Invalid/expired token or weak password", body = crate::error::ProblemDetails),
    )
)]
pub async fn reset_password(
    State(state): State<AppState>,
    Json(req): Json<ResetPasswordRequest>,
) -> ApiResult<StatusCode> {
    if req.new_password.len() < 8 {
        return Err(ApiError::BadRequest(
            "password must be at least 8 characters".into(),
        ));
    }

    let token_hash = hash_refresh_token(&req.token);
    let record = tankovault_db::repo::users::find_password_reset(&state.pool, &token_hash)
        .await?
        .ok_or_else(|| ApiError::BadRequest("invalid or expired reset token".into()))?;
    if record.used_at.is_some() || record.expires_at <= OffsetDateTime::now_utc() {
        return Err(ApiError::BadRequest(
            "invalid or expired reset token".into(),
        ));
    }

    // Single-use guard: the atomic `used_at` flip also closes the race between two
    // concurrent resets presenting the same token — the loser sees `0` rows and fails.
    let consumed =
        tankovault_db::repo::users::consume_password_reset(&state.pool, record.id).await?;
    if consumed == 0 {
        return Err(ApiError::BadRequest(
            "invalid or expired reset token".into(),
        ));
    }

    let hash = hash_password(&req.new_password).map_err(|_| ApiError::Internal)?;
    tankovault_db::repo::users::update_password(&state.pool, record.user_id, &hash).await?;
    tankovault_db::repo::users::revoke_all_for_user(&state.pool, record.user_id).await?;
    Ok(StatusCode::OK)
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct VerifyEmailRequest {
    /// The opaque token from the emailed confirmation link.
    pub token: String,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct ResendVerificationRequest {
    /// The account's email address. Unknown or already-confirmed addresses are accepted
    /// silently so the endpoint can't be used to probe which emails have accounts.
    pub email: String,
}

/// Confirm an email address with a token
///
/// Consumes a valid, unexpired, unused confirmation token, marks the address verified, and
/// signs the user in — issuing an access token and a rotating refresh cookie exactly like
/// [`login`] so clicking the link lands the user in the app.
#[utoipa::path(
    post,
    path = "/v1/auth/verify-email",
    tag = AUTH_TAG,
    request_body = VerifyEmailRequest,
    responses(
        (status = 200, description = "Email confirmed; session issued", body = TokenResponse),
        (status = 400, description = "Invalid or expired confirmation token", body = crate::error::ProblemDetails),
    )
)]
pub async fn verify_email(
    State(state): State<AppState>,
    jar: CookieJar,
    Json(req): Json<VerifyEmailRequest>,
) -> ApiResult<(CookieJar, Json<TokenResponse>)> {
    let token_hash = hash_refresh_token(&req.token);
    let record = tankovault_db::repo::users::find_email_verification(&state.pool, &token_hash)
        .await?
        .ok_or_else(|| ApiError::BadRequest("invalid or expired confirmation token".into()))?;
    if record.used_at.is_some() || record.expires_at <= OffsetDateTime::now_utc() {
        return Err(ApiError::BadRequest(
            "invalid or expired confirmation token".into(),
        ));
    }

    // Single-use guard: the atomic `used_at` flip closes the race between two concurrent
    // confirmations presenting the same token — the loser sees `0` rows and fails.
    let consumed =
        tankovault_db::repo::users::consume_email_verification(&state.pool, record.id).await?;
    if consumed == 0 {
        return Err(ApiError::BadRequest(
            "invalid or expired confirmation token".into(),
        ));
    }

    tankovault_db::repo::users::mark_email_verified(&state.pool, record.user_id).await?;
    let user = tankovault_db::repo::users::get(&state.pool, record.user_id).await?;
    // Now that the address is confirmed, send the welcome email that registration deferred.
    mailer::send_in_background(&state, mailer::welcome(&user.email, &user.username));
    issue_session(&state, jar, &user, Uuid::now_v7()).await
}

/// Resend the email-confirmation link
///
/// Always responds `202 Accepted`, whether or not the address is registered or already
/// confirmed, so the endpoint can't be used to probe which emails have accounts. A fresh
/// link is only sent when the address exists, is still unconfirmed, and email is configured.
#[utoipa::path(
    post,
    path = "/v1/auth/verify-email/resend",
    tag = AUTH_TAG,
    request_body = ResendVerificationRequest,
    responses(
        (status = 202, description = "If the address is registered and unconfirmed, a confirmation email has been sent"),
    )
)]
pub async fn resend_verification(
    State(state): State<AppState>,
    Json(req): Json<ResendVerificationRequest>,
) -> ApiResult<StatusCode> {
    let email = req.email.trim();
    if let Some((user, verified)) =
        tankovault_db::repo::users::find_by_email_with_verification(&state.pool, email).await?
    {
        if !verified && state.mailer.is_enabled() {
            send_verification_email(&state, &user).await?;
        }
    }
    // Uniform response regardless of whether the account exists or is already confirmed.
    Ok(StatusCode::ACCEPTED)
}

/// Issue and email a fresh single-use confirmation link for `user`. Reuses the high-entropy
/// opaque-token generator; only the SHA-256 hash is stored, and delivery is fire-and-forget.
async fn send_verification_email(state: &AppState, user: &User) -> ApiResult<()> {
    let raw = generate_refresh_token();
    let token_hash = hash_refresh_token(&raw);
    let expires_at = OffsetDateTime::now_utc() + VERIFY_TOKEN_TTL;
    tankovault_db::repo::users::insert_email_verification(
        &state.pool,
        user.id,
        &token_hash,
        expires_at,
    )
    .await?;
    let link = format!(
        "{}/verify-email?token={raw}",
        state.email_base_url.trim_end_matches('/'),
    );
    mailer::send_in_background(
        state,
        mailer::verification(&user.email, &user.username, &link),
    );
    Ok(())
}

/// Mint an access token + a rotating refresh cookie for `user` under `family_id`, returning
/// the JSON body wrapper used by [`login`], [`refresh`] and [`verify_email`].
async fn issue_session(
    state: &AppState,
    jar: CookieJar,
    user: &User,
    family_id: Uuid,
) -> ApiResult<(CookieJar, Json<TokenResponse>)> {
    let (jar, resp) = issue_session_tokens(state, jar, user, family_id).await?;
    Ok((jar, Json(resp)))
}

/// The core of [`issue_session`]: persist a rotating refresh token, set its cookie, and mint
/// an access token, returning the raw [`TokenResponse`] so callers ([`register`]) can embed it
/// in a different envelope.
async fn issue_session_tokens(
    state: &AppState,
    jar: CookieJar,
    user: &User,
    family_id: Uuid,
) -> ApiResult<(CookieJar, TokenResponse)> {
    let access = issue_access_token(
        &state.jwt_secret,
        user.id,
        &user.username,
        user.role,
        state.access_ttl,
    )
    .map_err(|_| ApiError::Internal)?;

    let raw_refresh = generate_refresh_token();
    let refresh_hash = hash_refresh_token(&raw_refresh);
    let expires_at = OffsetDateTime::now_utc() + state.refresh_ttl;
    tankovault_db::repo::users::insert_refresh(
        &state.pool,
        user.id,
        family_id,
        &refresh_hash,
        expires_at,
    )
    .await?;

    let cookie = Cookie::build((REFRESH_COOKIE, raw_refresh))
        .http_only(true)
        .secure(state.cookie_secure)
        .same_site(SameSite::Strict)
        .path(REFRESH_PATH)
        .max_age(state.refresh_ttl)
        .build();

    let resp = TokenResponse {
        access_token: access,
        token_type: "Bearer",
        expires_in: state.access_ttl.whole_seconds(),
    };
    Ok((jar.add(cookie), resp))
}

fn validate_registration(req: &RegisterRequest) -> ApiResult<()> {
    let email = req.email.trim();
    let username = req.username.trim();
    if !email.contains('@') || email.len() < 3 {
        return Err(ApiError::BadRequest("invalid email".into()));
    }
    if username.len() < 3 || username.len() > 32 {
        return Err(ApiError::BadRequest(
            "username must be 3–32 characters".into(),
        ));
    }
    if req.password.len() < 8 {
        return Err(ApiError::BadRequest(
            "password must be at least 8 characters".into(),
        ));
    }
    Ok(())
}
