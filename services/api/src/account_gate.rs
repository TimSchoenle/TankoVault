//! The deployment-wide account requirement: with `accounts.required` on, nothing outside the
//! sign-in surface answers a caller who has no account.
//!
//! The inverse of [`tankovault_service::flags::enforce`], which withdraws a route when its
//! feature is *off*. This one places a requirement on the *caller*, so it is a mounted layer
//! rather than a row in `route_features` — a rule keyed on path would have to name every public
//! route, and the one nobody remembered to name would be the hole the flag exists to close.
//!
//! # Why a layer and not an extractor
//!
//! Default-closed. Every route is gated unless [`is_sign_in_surface`] names it, so a public
//! route added later is private on a private deployment without anyone editing a table.
//! Putting it in the `Option<AuthUser>` extractor instead would gate only the routes that
//! happen to ask for a principal — and `/v1/tags`, `/v1/providers` and the legal documents do
//! not ask for one.
//!
//! # What "has an account" means here
//!
//! A verified access-token signature naming a user, and nothing further: no database round trip,
//! no suspension check, no permission resolution. Deliberately — this gate decides whether the
//! caller is *the public*, not what they may do, and every route that grants anything still
//! resolves the principal properly through [`crate::state::AuthUser`], which refuses a suspended
//! or erased account there. The cost of the difference is bounded by the access token's lifetime:
//! a suspended account can keep reading the public catalogue until its 15-minute token expires,
//! while being refused everywhere its own data lives. Paying a query per public request on every
//! walled deployment to close that window is the worse trade.

use crate::error::ApiError;
use crate::state::AppState;
use axum::extract::{MatchedPath, Request, State};
use axum::http::HeaderMap;
use axum::http::header::AUTHORIZATION;
use axum::middleware::Next;
use axum::response::{IntoResponse as _, Response};
use tankovault_domain::Feature;

/// The axum middleware. Mount **outside** the feature gate, so a walled deployment answers a
/// signed-out caller without first telling them which of its features are switched off.
pub(crate) async fn enforce(State(state): State<AppState>, req: Request, next: Next) -> Response {
    if !state.features.is_enabled(Feature::AccountsRequired) {
        return next.run(req).await;
    }

    // The matched pattern, not the raw URI: a path parameter must not be able to spell its way
    // out of the gate, and `/v1/auth/…` has none to spell with.
    let path = req
        .extensions()
        .get::<MatchedPath>()
        .map_or_else(|| req.uri().path(), MatchedPath::as_str);
    let admitted = is_sign_in_surface(path) || carries_an_account(&state, req.headers());

    if !admitted {
        metrics::counter!(tankovault_service::metrics::names::ACCOUNT_REQUIRED).increment(1);
        return ApiError::AccountRequired.into_response();
    }
    next.run(req).await
}

/// Routes that stay open to an anonymous caller while the deployment is private.
///
/// Exactly the surface needed to *obtain* an account, on the same reasoning as
/// `crate::state::exempt_from_mandatory_mfa`: a gate with no way through it is not a private
/// deployment but a bricked one, recoverable only by an operator switching the flag back off.
///
/// - `/v1/auth` — signing in, registering, resetting a password, confirming an address, and both
///   legs of a passkey or second-factor sign-in.
/// - `/v1/legal` — registering *is* the act of accepting the Terms, so a visitor has to be able
///   to read them before they have an account.
///
/// `/v1/me/capabilities` is deliberately **not** here. It is the probe the web app gates its UI
/// on, and its refusal is how a signed-out client learns this deployment is private at all: a
/// `401 account_required` there is the signal to show the sign-in screen, where a plain
/// `401 unauthorized` means only that this particular session has ended. Exempting it would
/// answer the same for both and leave the client unable to tell them apart.
fn is_sign_in_surface(path: &str) -> bool {
    path.starts_with("/v1/auth") || path.starts_with("/v1/legal")
}

/// Whether the request presents an access token this deployment issued.
///
/// Signature-verified, so a forged or expired token is not an account; see the module docs for
/// what this deliberately does not check.
fn carries_an_account(state: &AppState, headers: &HeaderMap) -> bool {
    headers
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .and_then(|token| tankovault_auth::verify_access_token(&state.jwt_secret, token).ok())
        .is_some_and(|claims| claims.user_id().is_some())
}

#[cfg(test)]
mod tests {
    use super::is_sign_in_surface;

    #[test]
    fn the_whole_credential_surface_stays_open() {
        for path in [
            "/v1/auth/login",
            "/v1/auth/register",
            "/v1/auth/refresh",
            "/v1/auth/password/reset",
            "/v1/auth/verify-email",
            "/v1/auth/passkey/login/start",
            "/v1/auth/mfa/verify",
            "/v1/legal",
            "/v1/legal/{slug}",
        ] {
            assert!(
                is_sign_in_surface(path),
                "`{path}` is how a visitor gets an account; walling it makes the flag a lockout"
            );
        }
    }

    /// The inverse leg. A predicate that answered `true` too readily would pass every test
    /// above while gating nothing, and the signed-in surface is where the reader's own data is.
    #[test]
    fn everything_else_is_behind_the_wall() {
        for path in [
            "/v1/series",
            "/v1/series/{id}",
            "/v1/tags",
            "/v1/providers",
            "/v1/me/watchlist",
            "/v1/admin/users",
            "/scalar",
        ] {
            assert!(!is_sign_in_surface(path), "`{path}` must not be exempt");
        }
    }

    /// The capability probe carries the answer the signed-out client needs, so it has to be
    /// refused rather than exempted — see [`is_sign_in_surface`].
    #[test]
    fn the_capability_probe_is_not_exempt() {
        assert!(!is_sign_in_surface("/v1/me/capabilities"));
    }
}
