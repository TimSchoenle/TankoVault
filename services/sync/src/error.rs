//! The failures this service's HTTP contract is defined in terms of.
//!
//! Previously the status code was chosen by **substring-matching the error message**:
//!
//! ```ignore
//! let status = if message.contains("unknown sync provider") { NOT_FOUND }
//!              else if message.contains("account linked")   { CONFLICT }
//!              else                                         { BAD_GATEWAY };
//! ```
//!
//! Two things were wrong with that. Rewording a log line silently changed an HTTP status
//! contract, with no compile error and no test that could catch it. And the `"account linked"`
//! needle matched the *negated* message it was derived from — `"no anilist account linked for
//! user"` — so any future message containing the phrase, `"account linked successfully"` for
//! instance, would have been served as a `409`.
//!
//! [`SyncError`] names the failures that carry HTTP meaning. Everything else stays
//! `anyhow::Error`: a provider 500, a sealed-token decode failure and a database outage are
//! all "something went wrong upstream of the caller" and genuinely share one status. Typing
//! only the contractual cases keeps the change proportionate — this is an error *contract*,
//! not an exhaustive taxonomy of everything that can go wrong.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use tankovault_service::problem::Problem;

/// A failure with a defined place in this service's HTTP contract.
#[derive(Debug, thiserror::Error)]
pub(crate) enum SyncError {
    /// No provider is registered under that slug. `404`.
    #[error("unknown sync provider: {0}")]
    UnknownProvider(String),
    /// The user has not linked an account at this provider. `409` — the request is
    /// well-formed and the caller may fix it by linking, which is what conflict means here.
    #[error("no {0} account linked for user")]
    NotLinked(String),
}

impl SyncError {
    /// The status this variant maps to. Exhaustive by construction: adding a variant without
    /// deciding its status is a compile error, which is the property the substring match
    /// could never have.
    fn status(&self) -> StatusCode {
        match self {
            Self::UnknownProvider(_) => StatusCode::NOT_FOUND,
            Self::NotLinked(_) => StatusCode::CONFLICT,
        }
    }

    /// A stable machine-readable discriminator for the problem body.
    fn kind(&self) -> &'static str {
        match self {
            Self::UnknownProvider(_) => "unknown_provider",
            Self::NotLinked(_) => "not_linked",
        }
    }
}

/// The service's handler error: a typed contract failure, or anything else.
pub(crate) struct AppError(anyhow::Error);

impl<E: Into<anyhow::Error>> From<E> for AppError {
    fn from(err: E) -> Self {
        Self(err.into())
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        // Downcast rather than match on the text. `anyhow` preserves the concrete type, so a
        // `SyncError` raised anywhere down the call stack still arrives typed here — including
        // through the `?` chains that add context along the way.
        let (status, kind, detail) = match self.0.downcast_ref::<SyncError>() {
            Some(err) => (err.status(), err.kind(), err.to_string()),
            // Most failures here genuinely originate at a third-party provider, so the caller
            // is looking at a bad *gateway*, not a bad request.
            None => (
                StatusCode::BAD_GATEWAY,
                "upstream_failure",
                self.0.to_string(),
            ),
        };

        tracing::warn!(error = %self.0, %status, "sync request failed");
        Problem::new(status, kind, detail).into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_failures_keep_their_status_regardless_of_wording() {
        assert_eq!(
            SyncError::UnknownProvider("nope".to_owned()).status(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            SyncError::NotLinked("AniList".to_owned()).status(),
            StatusCode::CONFLICT
        );
    }

    /// The regression the substring match invited: a *successful* message containing the same
    /// phrase used to be served as a `409`.
    #[test]
    fn an_unrelated_message_mentioning_the_old_needles_is_not_misrouted() {
        for message in [
            "account linked successfully",
            "unknown sync provider was resolved after a retry",
        ] {
            let err = AppError(anyhow::anyhow!("{message}"));
            let status = match err.0.downcast_ref::<SyncError>() {
                Some(e) => e.status(),
                None => StatusCode::BAD_GATEWAY,
            };
            assert_eq!(status, StatusCode::BAD_GATEWAY, "misrouted: {message}");
        }
    }

    #[test]
    fn a_typed_failure_survives_anyhow_context() {
        let err: anyhow::Error = SyncError::NotLinked("AniList".to_owned()).into();
        let wrapped = err.context("while pushing progress");
        assert!(
            matches!(
                wrapped.downcast_ref::<SyncError>(),
                Some(SyncError::NotLinked(_))
            ),
            "adding context must not erase the contract"
        );
    }
}
