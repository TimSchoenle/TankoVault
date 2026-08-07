//! The adult-content gate: the one place that decides whether a request may see gated series.
//!
//! Two independent conditions, and both must hold:
//!
//! 1. the deployment has [`Feature::CatalogueAdultContent`] switched on — off by default, and
//!    an operator turning it on shows nobody anything by itself;
//! 2. the requesting account has opted in *and* attested its age.
//!
//! There is no third way in. An anonymous request cannot satisfy (2) at all, which is the
//! design: the opt-in is a property of an account, so a caller without one has nowhere to
//! record a decision and nothing to consult. Every read surface resolves the answer through
//! [`AdultVisibility`] rather than assembling it locally — the gate is only as good as its
//! least careful call site, and there are a dozen of them.

use crate::state::{AppState, AuthUser};
use axum::extract::{FromRequestParts, OptionalFromRequestParts};
use axum::http::request::Parts;
use tankovault_domain::Feature;

/// Whether this request may see adult-gated series.
///
/// Extracted, not computed in the handler, so "did we remember to check?" is answered by the
/// function signature instead of by reading the body.
#[derive(Debug, Clone, Copy)]
pub struct AdultVisibility(pub bool);

impl AdultVisibility {
    /// The value the repository layer's `include_adult` parameters take.
    #[must_use]
    pub const fn include_adult(self) -> bool {
        self.0
    }
}

impl FromRequestParts<AppState> for AdultVisibility {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        // Checked before the token is even looked at. The kill switch has to hold without
        // consulting anything an account can influence, and this way flipping it off costs a
        // disabled deployment no database work per catalogue request.
        if !state.features.is_enabled(Feature::CatalogueAdultContent) {
            return Ok(Self(false));
        }

        // Optional, and deliberately so: these routes serve anonymous callers, and a missing or
        // expired token has to read as "anonymous", never as a rejection. Turning an unreadable
        // token into a 401 here would break public browsing for anyone holding a stale session.
        let user = <AuthUser as OptionalFromRequestParts<AppState>>::from_request_parts(
            parts, state,
        )
        .await
        .unwrap_or(None);

        Ok(Self(user.is_some_and(|u| u.adult_opt_in)))
    }
}
