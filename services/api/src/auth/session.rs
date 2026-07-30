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

/// The `__Host-`-prefixed cookie name, used whenever `cookie_secure` is on.
///
/// See [`refresh_cookie`] for the `Path=/` review that decided this.
const HOST_REFRESH_COOKIE: &str = "__Host-refresh_token";
/// The unprefixed name, used only by the local-HTTP development opt-out.
const DEV_REFRESH_COOKIE: &str = "refresh_token";
/// The path the `__Host-` prefix *requires*. Narrowing it silently breaks the cookie.
const HOST_REFRESH_PATH: &str = "/";
/// The narrow path the unprefixed development cookie keeps.
const DEV_REFRESH_PATH: &str = "/v1/auth";

/// The name and `Path` this deployment issues the refresh cookie under.
///
/// # The `__Host-` prefix, and the `Path=/` review it needed (SEC-7)
///
/// The prefix makes three properties **browser-enforced** instead of merely configured: the
/// cookie must carry `Secure`, must have no `Domain`, and must be set with `Path=/`. The first
/// is the one the audit asked for — `cookie_secure` is a boolean an operator can get wrong, and
/// a `__Host-` cookie without `Secure` is simply refused rather than silently downgraded. The
/// second turns out to matter more: with a `Domain`-scoped cookie, any sibling host on the
/// registrable domain (a compromised `blog.example.com`, a stray staging box) can *write*
/// `refresh_token` for `.example.com`, and the browser sends it here. That is cookie-tossing
/// session fixation, and the narrow `Path=/v1/auth` did **nothing** against it — an attacker
/// setting the cookie picks the path too.
///
/// The prefix's cost is `Path=/`, so the review the tracker asked for is: what newly carries
/// this cookie, and what reads it?
///
/// - **What newly carries it.** The SPA and the API share one origin through
///   `services/frontend`'s `/v1/*` reverse proxy (see that crate's "Why one origin"), so
///   `Path=/` means the cookie rides on the app shell, every hashed asset, every `/v1/**`
///   request, the long-lived `GET /v1/me/stream`, and `/scalar` where it is exposed. Under
///   `Path=/v1/auth` it rode on four endpoints.
/// - **What reads it.** Exactly the handlers in this module. `CookieJar` is extracted in
///   `auth/{login,register,session,verification}.rs` and nowhere else in the service; no
///   middleware in `tankovault_service` touches cookies, and the frontend proxy forwards the
///   header without inspecting it. So the wider path grants no handler a credential it did not
///   already have.
/// - **CSRF.** `SameSite=Strict` is kept, which means no cross-site request carries the cookie
///   at any path — and even if it did, the only state-changing cookie readers are `refresh` and
///   `logout`, neither of which is reachable cross-site under `Strict`. Widening `Path` does not
///   move this.
/// - **Leakage.** The cookie is `HttpOnly`, so script cannot read it at either path; `Secure` is
///   now browser-enforced, so it cannot travel in clear. What genuinely changes is *volume*: a
///   30-day credential is now transmitted on every request to the origin rather than a handful,
///   which matters to an intermediary configured to log the `Cookie` header. That is a
///   deployment concern (do not log request cookies) and it is strictly smaller than the
///   subdomain write the prefix closes.
///
/// Conclusion: implemented. The prefix's guarantees are worth more than the narrow path, which
/// was protecting against nothing the prefix does not protect against better.
///
/// # Why the name is conditional
///
/// A `__Host-` cookie must be `Secure`, and `cookie_secure = false` exists for local HTTP
/// development. Issuing `__Host-refresh_token` without `Secure` would have the browser drop it
/// outright: sign-in would appear to succeed and every reload would land signed out, with no
/// error anywhere. So the development opt-out keeps the old unprefixed name at the old narrow
/// path, and the read side accepts **only** the name this deployment issues — never both — or a
/// sibling subdomain could plant the unprefixed name and have it honoured, which is the exact
/// hole the prefix exists to close.
///
/// Flipping `cookie_secure` orphans every already-issued cookie under the other name, so the
/// first request after such a deploy is one forced re-authentication. That is a one-time cost on
/// a setting that is not expected to change after the first boot.
const fn refresh_cookie(cookie_secure: bool) -> (&'static str, &'static str) {
    if cookie_secure {
        (HOST_REFRESH_COOKIE, HOST_REFRESH_PATH)
    } else {
        (DEV_REFRESH_COOKIE, DEV_REFRESH_PATH)
    }
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
    client: ClientContext,
    jar: CookieJar,
) -> ApiResult<(CookieJar, Json<TokenResponse>)> {
    let (name, _) = refresh_cookie(state.cookie_secure);
    let raw = jar
        .get(name)
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
    let (name, path) = refresh_cookie(state.cookie_secure);
    if let Some(raw) = jar.get(name).map(|c| c.value().to_owned()) {
        if let Some(record) =
            tankovault_db::repo::users::find_refresh(&state.pool, &hash_refresh_token(&raw)).await?
        {
            tankovault_db::repo::users::revoke_family(&state.pool, record.family_id).await?;
        }
    }
    // The removal must match the name *and* path the cookie was set with, or the browser keeps
    // its copy: a `Set-Cookie` for `refresh_token` at `/v1/auth` does not clear
    // `__Host-refresh_token` at `/`.
    let removal = Cookie::build((name, "")).path(path).build();
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

    // `__Host-refresh_token` at `Path=/` in every deployment that marks cookies `Secure`; the
    // unprefixed name at `/v1/auth` only for the local-HTTP opt-out. `refresh_cookie` carries the
    // review behind that choice. No `Domain` is set — deliberately, and the prefix enforces it.
    let (name, path) = refresh_cookie(state.cookie_secure);
    let cookie = Cookie::build((name, raw_refresh))
        .http_only(true)
        .secure(state.cookie_secure)
        .same_site(SameSite::Strict)
        .path(path)
        .max_age(state.refresh_ttl)
        .build();

    let resp = TokenResponse {
        access_token: access,
        token_type: "Bearer",
        expires_in: state.access_ttl.whole_seconds(),
    };
    Ok((jar.add(cookie), resp))
}

#[cfg(test)]
mod tests {
    use super::{
        DEV_REFRESH_COOKIE, DEV_REFRESH_PATH, HOST_REFRESH_COOKIE, HOST_REFRESH_PATH,
        refresh_cookie,
    };

    /// The `__Host-` prefix is only honoured at `Path=/`, and a browser that refuses the cookie
    /// says nothing about it (SEC-7).
    ///
    /// This is the failure mode worth a test: narrow the path back to `/v1/auth` while keeping
    /// the prefixed name and every server-side test still passes — the cookie is set, the
    /// response looks right — while every real browser drops it on the floor. Sign-in appears to
    /// succeed and `POST /v1/auth/refresh` answers `401` forever after, with no error anywhere to
    /// explain why. Nothing except this assertion connects the two halves.
    #[test]
    fn the_host_prefixed_name_is_only_ever_issued_at_the_root_path() {
        let (name, path) = refresh_cookie(true);
        assert_eq!(name, HOST_REFRESH_COOKIE);
        assert_eq!(
            path, HOST_REFRESH_PATH,
            "a `__Host-` cookie at any path but `/` is silently refused by the browser"
        );
        assert!(name.starts_with("__Host-"));
    }

    /// The development opt-out keeps the unprefixed name, because a `__Host-` cookie without
    /// `Secure` is refused too — and `cookie_secure = false` exists precisely to omit `Secure`.
    #[test]
    fn the_insecure_opt_out_keeps_the_unprefixed_name_and_the_narrow_path() {
        let (name, path) = refresh_cookie(false);
        assert_eq!(name, DEV_REFRESH_COOKIE);
        assert_eq!(path, DEV_REFRESH_PATH);
        assert!(
            !name.starts_with("__Host-"),
            "the prefix requires `Secure`, which this configuration deliberately omits"
        );
    }

    /// The two configurations must not share a name.
    ///
    /// The read side accepts only the one name this deployment issues. If both spellings ever
    /// collapsed to the same string, a `Secure` deployment would also honour a cookie a sibling
    /// subdomain could have written — which is the cookie-tossing hole the prefix exists to
    /// close, reopened by a rename.
    #[test]
    fn the_secure_and_insecure_names_are_distinct() {
        assert_ne!(refresh_cookie(true).0, refresh_cookie(false).0);
    }
}
