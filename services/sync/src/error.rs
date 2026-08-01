//! Failures this service's HTTP contract is defined in terms of.
//! [`SyncError`] carries status/kind explicitly so rewording a message can never silently change
//! what's served; everything untyped is `anyhow::Error` and maps to a 502.

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
    /// The status this variant maps to. Exhaustive by construction: a new variant with no
    /// status decided is a compile error.
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
        // Downcast rather than match on text: `anyhow` preserves the concrete type through
        // `?`-added context, so a `SyncError` raised anywhere below still arrives typed.
        let (status, kind, detail) = match self.0.downcast_ref::<SyncError>() {
            Some(err) => (err.status(), err.kind(), err.to_string()),
            // Untyped failures are treated as upstream provider errors, hence bad gateway.
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

    /// Pins a bug where substring-matching messages misrouted a success message containing
    /// the same phrase as the error to a `409`.
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
