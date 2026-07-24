//! Authentication handlers: register, login, refresh (rotating + reuse-detecting), logout.

use crate::error::{ApiError, ApiResult};
use crate::state::AppState;
use axum::Json;
use axum::extract::State;
use axum_extra::extract::cookie::{Cookie, CookieJar, SameSite};
use serde::{Deserialize, Serialize};
use tankovault_auth::{
    generate_refresh_token, hash_password, hash_refresh_token, issue_access_token, verify_password,
};
use tankovault_domain::{User, UserRole};
use time::OffsetDateTime;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::openapi::AUTH_TAG;

const REFRESH_COOKIE: &str = "refresh_token";
const REFRESH_PATH: &str = "/v1/auth";

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

/// Register a new account
///
/// Validates the request, creates the user, then issues an access token and a rotating
/// refresh-token cookie exactly like [`login`].
#[utoipa::path(
    post,
    path = "/v1/auth/register",
    tag = AUTH_TAG,
    request_body = RegisterRequest,
    responses(
        (status = 200, description = "Account created; access token issued", body = TokenResponse),
        (status = 400, description = "Invalid email, username or password", body = crate::error::ProblemDetails),
        (status = 409, description = "Email or username already taken", body = crate::error::ProblemDetails),
    )
)]
pub async fn register(
    State(state): State<AppState>,
    jar: CookieJar,
    Json(req): Json<RegisterRequest>,
) -> ApiResult<(CookieJar, Json<TokenResponse>)> {
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
    issue_session(&state, jar, &user, Uuid::now_v7()).await
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

/// Mint an access token + a rotating refresh cookie for `user` under `family_id`.
async fn issue_session(
    state: &AppState,
    jar: CookieJar,
    user: &User,
    family_id: Uuid,
) -> ApiResult<(CookieJar, Json<TokenResponse>)> {
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
    Ok((jar.add(cookie), Json(resp)))
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
