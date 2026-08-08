//! Step-up ("sudo") elevation: the proof a sensitive route demands on top of a session.
//!
//! # What this replaces, and why
//!
//! Sensitive routes used to take a `current_password`. Against the threat they exist for —
//! someone who *has* the password, or has stolen a live access token — that proved nothing: the
//! attacker types the same password again and changes the email, enrols their own passkey, and
//! the takeover survives the real owner's password reset. The elevation demanded here is a
//! *second factor*, which by construction the password thief does not hold.
//!
//! # The shape
//!
//! [`Elevated`] is an extractor, not a helper a handler remembers to call. A route that needs
//! elevation writes `Elevated(user): Elevated` instead of `user: AuthUser`, so the requirement
//! is in the signature and cannot be dropped in a refactor — which is the failure mode of every
//! convention-based version of this.
//!
//! The grant itself is a row (`step_up_grants`), resolved per request from an opaque token the
//! client sends in `X-Step-Up`. Not a JWT claim: `AccessClaims` carries no authorization state
//! on purpose, and an elevation must be revocable before it expires — a password change, a
//! sign-out or the removal of the factor that earned it all end it immediately. Not the refresh
//! cookie either: the desktop build has no cookie jar to key on.
//!
//! Issuing a grant lives in `crate::me::mfa`, next to the factors it verifies.

use axum::extract::FromRequestParts;
use axum::http::HeaderMap;
use axum::http::request::Parts;
use secrecy::SecretString;
use tankovault_db::repo::users::mfa::StepUpMethod;

use crate::error::ApiError;
use crate::state::{AppState, AuthUser};

/// The header a client presents its elevation in.
pub const STEP_UP_HEADER: &str = "x-step-up";

/// An authenticated principal that has *also* presented a second factor within the step-up
/// window.
///
/// A newtype over [`AuthUser`] rather than a field beside it: `Elevated(user)` destructures to
/// exactly the value every other handler takes, so converting a route costs one pattern in the
/// signature and nothing in the body.
///
/// Which factor was presented is deliberately not carried. It is recorded where it is known —
/// the audit record written when the grant is issued — and a handler that could read it here
/// would be a handler tempted to branch on it, re-deciding per route a policy that belongs in
/// one place.
pub struct Elevated(pub AuthUser);

impl FromRequestParts<AppState> for Elevated {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        // The session first: an expired token must answer `401` and drive a token refresh, not
        // `403` and drive a re-authentication prompt that cannot succeed without one.
        let user =
            <AuthUser as FromRequestParts<AppState>>::from_request_parts(parts, state).await?;
        if user.elevated {
            Ok(Self(user))
        } else {
            Err(ApiError::StepUpRequired)
        }
    }
}

/// Resolve the `X-Step-Up` header into the `elevated` bit `AuthUser` carries.
///
/// Called once per authenticated request, from the `AuthUser` extractor. The lookup is skipped
/// entirely when the header is absent, which is every request but the handful that follow a
/// re-authentication prompt — so the common path costs a header probe and no query.
///
/// # Errors
/// [`ApiError::Internal`] on a database failure. A missing, unknown, expired or revoked grant
/// is `Ok(false)`, not an error: this function answers "is this request elevated", and the
/// handler that cares decides what to do about "no".
pub(crate) async fn resolve(
    state: &AppState,
    user_id: tankovault_domain::UserId,
    mfa_enrolled: bool,
    headers: &HeaderMap,
) -> Result<bool, ApiError> {
    let Some(token) = headers
        .get(STEP_UP_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(SecretString::from)
    else {
        return Ok(false);
    };

    let Some(method) = tankovault_db::repo::users::mfa::find_step_up(
        &state.pool,
        user_id,
        &tankovault_auth::hash_handle(&token),
    )
    .await?
    else {
        return Ok(false);
    };

    // A grant earned by password — the fallback offered to an account with no factor — stops
    // counting the moment a factor exists. Without this, enrolling a second factor would leave
    // the weaker proof usable beside it, and every elevation would be worth exactly what the
    // password is worth: nothing, against someone who has it.
    Ok(!(method == StepUpMethod::Password && mfa_enrolled))
}

/// Demand an elevation only from an account that has a factor to elevate with.
///
/// The enrolment routes need this instead of [`Elevated`]: a user adding their *first* factor
/// cannot present one, and requiring it would make the feature unreachable. The moment a factor
/// exists the requirement applies in full, which is what stops a stolen session from quietly
/// adding its own second factor beside the owner's — or replacing it.
///
/// # Errors
/// [`ApiError::StepUpRequired`] when a factor exists and no valid grant was presented.
pub(crate) fn require_elevation_if_enrolled(user: &AuthUser) -> Result<(), ApiError> {
    if user.mfa_enrolled && !user.elevated {
        return Err(ApiError::StepUpRequired);
    }
    Ok(())
}
