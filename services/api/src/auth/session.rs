//! The session itself: the rotating refresh-token cookie, its reuse detection, and the
//! access-token mint every sign-in path funnels through.

use axum::Json;
use axum::extract::State;
use axum_extra::extract::cookie::{Cookie, CookieJar, SameSite};
use secrecy::{ExposeSecret as _, SecretString};
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

/// How long after its own rotation a refresh token is still honoured — absorbs a raced or
/// interrupted rotation (two tabs, a dropped response) without treating an honest retry as
/// theft, which would revoke the whole session rather than just fail one request.
///
/// Safe because acceptance collapses the family to one token and requires a still-live token
/// in it; a genuine replay later still hits an already-revoked token, caught one cycle late
/// rather than never.
const ROTATION_GRACE: time::Duration = time::Duration::seconds(60);

/// The name and `Path` this deployment issues the refresh cookie under.
///
/// `__Host-` makes `Secure`, no `Domain`, and `Path=/` browser-enforced, closing a subdomain
/// cookie-tossing write a narrower path did not stop — but it requires `Secure`, so the
/// local-HTTP opt-out keeps the old unprefixed name instead, since a `__Host-` cookie without
/// `Secure` is silently dropped. The read side accepts only the name this deployment issues,
/// never both, or a sibling subdomain could plant the unprefixed name.
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
/// Presenting a token that was already rotated (reuse) revokes the whole token family. The one
/// exception is a token presented within a short grace window of its own rotation while its
/// family is still live: that is an interrupted or raced rotation by a client that never took
/// delivery of the successor, and it is served — collapsing the family to the single token it
/// issues — rather than counted as theft.
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
    // Wrapped the moment it's copied out of the jar, before it's hashed.
    let raw = jar
        .get(name)
        .map(|c| SecretString::from(c.value()))
        .ok_or(ApiError::Unauthorized)?;
    let token_hash = hash_refresh_token(&raw);

    let record = tankovault_db::repo::users::find_refresh(&state.pool, &token_hash)
        .await?
        .ok_or(ApiError::Unauthorized)?;

    // Reuse detection: presenting an already-rotated token usually means theft, but a raced or
    // interrupted rotation leaves an honest client holding the same evidence — see
    // `ROTATION_GRACE`'s doc before changing anything below.
    if let Some(revoked_at) = record.revoked_at {
        // Evaluated before the family is collapsed below — liveness is about the state this
        // request arrived into, not the one it leaves behind.
        let raced = OffsetDateTime::now_utc() - revoked_at <= ROTATION_GRACE
            && tankovault_db::repo::users::family_has_live_token(&state.pool, record.family_id)
                .await?;

        // Collapse either way: on theft that's the point; on a race it enforces "one live
        // token per family" by revoking a successor nobody took delivery of.
        tankovault_db::repo::users::revoke_family(&state.pool, record.family_id).await?;

        if !raced {
            // The highest-signal security event this service emits — a rotated token replayed
            // means two parties hold the same credential.
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

        // Audited as a success, separate from theft, so `token_reuse_detected` in the log
        // always means what it claims.
        tracing::info!(
            user_id = %record.user_id.as_uuid(),
            family_id = %record.family_id,
            "refresh token presented inside the rotation grace window; recovering the session"
        );
        audit_anonymous(
            &state,
            &client,
            Some(record.user_id),
            "auth.refresh",
            &record.user_id.as_uuid().to_string(),
            &serde_json::json!({
                "reason": "rotation_race_recovered",
                "family_id": record.family_id,
                "action_taken": "family_collapsed_and_reissued",
            }),
            AuditOutcome::Success,
        )
        .await;
    }

    if record.expires_at <= OffsetDateTime::now_utc() {
        return Err(ApiError::Unauthorized);
    }

    // Rotate: revoke the presented token, mint a new one in the family. A no-op on the grace
    // path since `revoke_token` is guarded by `revoked_at IS NULL`.
    tankovault_db::repo::users::revoke_token(&state.pool, record.id).await?;
    let user = tankovault_db::repo::users::get(&state.pool, record.user_id).await?;

    // A suspension applied since this session began must end it, or the holder of a live
    // refresh cookie could keep minting access tokens until the cookie itself expired.
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
    if let Some(raw) = jar.get(name).map(|c| SecretString::from(c.value()))
        && let Some(record) =
            tankovault_db::repo::users::find_refresh(&state.pool, &hash_refresh_token(&raw)).await?
    {
        tankovault_db::repo::users::revoke_family(&state.pool, record.family_id).await?;
    }
    // The removal must match the cookie's name and path, or the browser keeps its old copy.
    // `Secure` matters too: a `__Host-` removal without it is refused by the browser rather
    // than applied, so a stale cookie would sit in the jar until it expired on its own.
    Ok(jar.remove(removal_cookie(name, path, state.cookie_secure)))
}

/// The `Set-Cookie` that clears the refresh cookie, built to the same rules as the one that
/// set it — separate from [`logout`] so those rules are assertable without a database.
///
/// `secure` is a parameter, not a literal `true`: pinning it would stop the local-HTTP opt-out
/// from ever clearing its cookie, since a `Secure` cookie is ignored over plain HTTP. Both
/// directions are asserted below.
fn removal_cookie(name: &'static str, path: &'static str, secure: bool) -> Cookie<'static> {
    Cookie::build((name, ""))
        .http_only(true)
        .secure(secure)
        .same_site(SameSite::Strict)
        .path(path)
        .build()
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

    // `__Host-refresh_token` at `Path=/` when cookies are `Secure`; the unprefixed name at
    // `/v1/auth` only for the local-HTTP opt-out. No `Domain` is set, deliberately.
    let (name, path) = refresh_cookie(state.cookie_secure);
    // Deliberate unwrapping — the refresh token exists to be handed to the browser in this
    // header, and `Cookie::build` needs an owned `String`.
    let cookie = Cookie::build((name, raw_refresh.expose_secret().to_owned()))
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
        refresh_cookie, removal_cookie,
    };

    /// A `__Host-` cookie without `Secure` is refused, and that applies to the removal too.
    ///
    /// The bug: the removal cookie omitted `Secure`, so the browser silently dropped it in any
    /// `cookie_secure` deployment — the family was revoked server-side, but the user's cookie
    /// jar still held a value that looked "logged in".
    ///
    /// `secure` tracks `cookie_secure` rather than being pinned to `true`, since a `Secure`
    /// removal sent over plain HTTP is itself ignored.
    #[test]
    fn the_logout_removal_carries_the_attributes_the_host_prefix_requires() {
        let secure = removal_cookie(HOST_REFRESH_COOKIE, HOST_REFRESH_PATH, true);
        assert_eq!(secure.secure(), Some(true), "__Host- removal needs Secure");
        assert_eq!(secure.path(), Some(HOST_REFRESH_PATH));
        assert_eq!(secure.http_only(), Some(true));
        assert!(
            secure.domain().is_none(),
            "a Domain would have the __Host- removal refused as well"
        );

        let dev = removal_cookie(DEV_REFRESH_COOKIE, DEV_REFRESH_PATH, false);
        assert_eq!(
            dev.secure(),
            Some(false),
            "a Secure removal over local HTTP is ignored, so logout would never clear it"
        );
    }

    /// The `__Host-` prefix is only honoured at `Path=/`, and a browser that refuses the cookie
    /// says nothing about it.
    ///
    /// Narrowing the path back while keeping the prefixed name passes every server-side test —
    /// the browser just silently drops the cookie, so sign-in appears to work and every
    /// refresh answers 401 forever after, with nothing to explain why.
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

    /// The two configurations must not share a name, or a `Secure` deployment would also
    /// honour a cookie a sibling subdomain could have written — reopening the cookie-tossing
    /// hole the prefix exists to close.
    #[test]
    fn the_secure_and_insecure_names_are_distinct() {
        assert_ne!(refresh_cookie(true).0, refresh_cookie(false).0);
    }
}
