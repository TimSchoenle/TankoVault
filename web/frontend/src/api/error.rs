//! Turning API operation failures into text a reader can act on.
//!
//! The generated client's `progenitor_client::Error` mixes transport, decode and error-response
//! faults into one enum; this maps it to a short sentence and, occasionally, the raw status code.

use crate::i18n::Translator;
use progenitor_client::Error as ApiOpError;

/// The HTTP status of a failed operation, when the failure was an error *response* rather
/// than a transport or decoding fault.
pub(crate) fn error_status<E>(err: &ApiOpError<E>) -> Option<u16> {
    match err {
        ApiOpError::ErrorResponse(response) => Some(response.status().as_u16()),
        _ => None,
    }
}

/// A short, user-facing sentence for a failed operation (§17.3: name what failed).
///
/// Never leaks `Debug` output into the UI — transport/decode faults are bucketed into plain
/// language instead. Resolved eagerly, in the language active when the call failed.
pub(crate) fn friendly_error<E>(i18n: Translator, err: ApiOpError<E>) -> String {
    match err {
        ApiOpError::ErrorResponse(response) => status_message(i18n, response.status().as_u16()),
        ApiOpError::UnexpectedResponse(response) => {
            status_message(i18n, response.status().as_u16())
        }
        ApiOpError::CommunicationError(_) | ApiOpError::InvalidUpgrade(_) => {
            i18n.t("error.unreachable")
        }
        ApiOpError::ResponseBodyError(_) => i18n.t("error.truncated"),
        ApiOpError::InvalidResponsePayload(_, _) => i18n.t("error.unreadable"),
        // Both are programming faults on our side rather than anything the reader did.
        ApiOpError::InvalidRequest(_) | ApiOpError::Custom(_) => i18n.t("error.unbuildable"),
    }
}

/// Map an HTTP status onto the sentence shown to the reader.
fn status_message(i18n: Translator, status: u16) -> String {
    match status_key(status) {
        Some(key) => i18n.t(key),
        None => i18n.args("error.status.other", &[("code", &status.to_string())]),
    }
}

/// The catalogue key wording an HTTP status, or `None` when there is nothing better to say
/// than the bare code.
///
/// Split from [`status_message`] so this stays testable on the host target — the message
/// lookup needs a Dioxus runtime, this doesn't.
fn status_key(status: u16) -> Option<&'static str> {
    Some(match status {
        400 => "error.status.badRequest",
        401 => "error.status.unauthorized",
        403 => "error.status.forbidden",
        404 => "error.status.notFound",
        409 => "error.status.conflict",
        413 => "error.status.tooLarge",
        429 => "error.status.rateLimited",
        500..=599 => "error.status.server",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_known_statuses_to_their_own_message() {
        assert_eq!(status_key(401), Some("error.status.unauthorized"));
        assert_eq!(status_key(503), Some("error.status.server"));
    }

    #[test]
    fn falls_back_to_the_bare_code_for_unmapped_statuses() {
        assert_eq!(status_key(418), None);
    }
}
