//! Turning API operation failures into text a reader can act on.
//!
//! The generated client surfaces `progenitor_client::Error`, which mixes transport faults,
//! decode faults and documented error *responses* into one enum. Views only ever want two
//! things from it: a short human sentence, and — occasionally — the raw status code so they
//! can branch (a login `403` means "confirm your email", not "you lack permission").

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
/// Never leaks `Debug` output into the UI: transport and decoding faults are bucketed into
/// plain language, because the underlying `reqwest`/`serde` text is meaningless to a reader
/// and, in the decode case, can be long enough to break the error box's layout.
pub(crate) fn friendly_error<E>(err: ApiOpError<E>) -> String {
    match err {
        ApiOpError::ErrorResponse(response) => status_message(response.status().as_u16()),
        ApiOpError::UnexpectedResponse(response) => status_message(response.status().as_u16()),
        ApiOpError::CommunicationError(_) | ApiOpError::InvalidUpgrade(_) => {
            "Couldn't reach the server. Check your connection and retry.".to_owned()
        }
        ApiOpError::ResponseBodyError(_) => {
            "The server's reply was cut short. Please retry.".to_owned()
        }
        ApiOpError::InvalidResponsePayload(_, _) => {
            "The server sent something this app couldn't read.".to_owned()
        }
        // Both are programming faults on our side rather than anything the reader did.
        ApiOpError::InvalidRequest(_) | ApiOpError::Custom(_) => {
            "That request couldn't be built. This is a bug — please report it.".to_owned()
        }
    }
}

/// Map an HTTP status onto the sentence shown to the reader.
fn status_message(status: u16) -> String {
    match status {
        400 => "That request wasn't valid.".to_owned(),
        401 => "You need to sign in to do that.".to_owned(),
        403 => "You don't have permission to do that.".to_owned(),
        404 => "Not found.".to_owned(),
        409 => "That conflicts with the current state.".to_owned(),
        413 => "That's too large to send.".to_owned(),
        429 => "Too many requests — give it a moment and retry.".to_owned(),
        500..=599 => "The server had a problem. Please retry.".to_owned(),
        other => format!("Request failed ({other})."),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_known_statuses_to_plain_language() {
        assert_eq!(status_message(401), "You need to sign in to do that.");
        assert_eq!(
            status_message(503),
            "The server had a problem. Please retry."
        );
    }

    #[test]
    fn falls_back_to_the_bare_code_for_unmapped_statuses() {
        assert_eq!(status_message(418), "Request failed (418).");
    }
}
