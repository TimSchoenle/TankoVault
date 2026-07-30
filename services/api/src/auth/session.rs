//! The session itself: the rotating refresh-token cookie, its reuse detection, and the
//! access-token mint every sign-in path funnels through.

use axum::Json;
use axum::extract::State;
use axum_extra::extract::cookie::{Cookie, CookieJar, SameSite};
use tankovault_auth::{generate_refresh_token, hash_refresh_token, issue_access_token};
use tankovault_domain::User;
use tankovault_service::AuditOutcome;
use time::OffsetDateTime;
use uuid::Uuid;

use super::login::TokenResponse;
use crate::audit::audit_anonymous;
use crate::error::{ApiError, ApiResult};
use crate::openapi::AUTH_TAG;
use crate::state::{AppState, ClientContext};

const REFRESH_COOKIE: &str = "refresh_token";
const REFRESH_PATH: &str = "/v1/auth";

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
    client: ClientContext,
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
        // The single highest-signal security event this service can emit: a rotated
        // refresh token being replayed means two parties hold the same credential.
        // Previously it revoked the family and returned 401 silently, leaving the
        // operator no way to know a token had been stolen.
        tracing::warn!(
            user_id = %record.user_id.as_uuid(),
            family_id = %record.family_id,
            "refresh token reuse detected; revoking family"
        );
        audit_anonymous(
            &state,
            &client,
            Some(record.user_id),
            "auth.refresh",
            &record.user_id.as_uuid().to_string(),
            &serde_json::json!({
                "reason": "token_reuse_detected",
                "family_id": record.family_id,
                "action_taken": "family_revoked",
            }),
            AuditOutcome::Denied,
        )
        .await;
        return Err(ApiError::Unauthorized);
    }
    if record.expires_at <= OffsetDateTime::now_utc() {
        return Err(ApiError::Unauthorized);
    }

    // Rotate: revoke the presented token, mint a new one in the same family.
    tankovault_db::repo::users::revoke_token(&state.pool, record.id).await?;
    let user = tankovault_db::repo::users::get(&state.pool, record.user_id).await?;

    // A suspension applied since this session began must end it. Without this check the
    // holder of a live refresh cookie could keep minting access tokens indefinitely, and a
    // suspension would only take effect once the cookie itself expired — weeks later.
    if !user.status.may_authenticate() {
        tankovault_db::repo::users::revoke_family(&state.pool, record.family_id).await?;
        audit_anonymous(
            &state,
            &client,
            Some(user.id),
            "auth.refresh",
            &user.id.as_uuid().to_string(),
            &serde_json::json!({
                "reason": "account_suspended",
                "action_taken": "family_revoked",
            }),
            AuditOutcome::Denied,
        )
        .await;
        return Err(ApiError::Suspended);
    }

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

/// Mint an access token + a rotating refresh cookie for `user` under `family_id`, returning
/// the JSON body wrapper used by [`login`], [`refresh`] and [`verify_email`].
pub(super) async fn issue_session(
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
pub(super) async fn issue_session_tokens(
    state: &AppState,
    jar: CookieJar,
    user: &User,
    family_id: Uuid,
) -> ApiResult<(CookieJar, TokenResponse)> {
    let access = issue_access_token(&state.jwt_secret, user.id, &user.username, state.access_ttl)
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
