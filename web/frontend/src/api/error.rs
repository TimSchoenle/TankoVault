//! Turning API operation failures into text a reader can act on.
//!
//! The generated client's `progenitor_client::Error` mixes transport, decode and error-response
//! faults into one enum; this maps it to a short sentence and, occasionally, the raw status code.

use crate::i18n::Translator;
use crate::wire::types::ProblemDetails;
use progenitor_client::Error as ApiOpError;

/// The HTTP status of a failed operation, when the failure was an error *response* rather
/// than a transport or decoding fault.
pub(crate) fn error_status<E>(err: &ApiOpError<E>) -> Option<u16> {
    match err {
        ApiOpError::ErrorResponse(response) => Some(response.status().as_u16()),
        _ => None,
    }
}

/// The server's own explanation of a refusal, when it sent one.
///
/// Only for operator surfaces whose 400s carry a rule rather than a validation slip: the
/// recommendation console's tunables refuse a write with the *reason* — a privacy threshold, a
/// range, or the cross-field rule that at least one score weight stays non-zero — and replacing
/// that with "the request was rejected" would leave an operator retrying a value the server will
/// never take. Reader-facing screens keep [`friendly_error`], which never shows server prose.
pub(crate) fn problem_detail(err: &ApiOpError<ProblemDetails>) -> Option<String> {
    match err {
        ApiOpError::ErrorResponse(response) => {
            let detail = response.detail.trim();
            (!detail.is_empty()).then(|| detail.to_owned())
        }
        _ => None,
    }
}

/// Which of the three refusals a guarded route answered with.
///
/// `403` is not one answer on the guarded surfaces but three — "confirm it is you", "enrol a
/// second factor first" and "you do not hold that grant" — and only the problem type separates
/// them. Branching on the bare status is what made the operator console report a step-up demand
/// as insufficient privileges, and would make it re-prompt forever for the other two: neither is
/// something a step-up prompt can resolve, so a gate that opened for them would ask a question
/// whose correct answer changes nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Refusal {
    /// `step_up_required` — present a second factor, then retry.
    StepUp,
    /// `mfa_enrolment_required` — the account holds no second factor at all.
    MfaEnrolment,
    /// Anything else, including a genuinely missing grant.
    Other,
}

impl Refusal {
    /// Read the refusal off the server's problem body.
    ///
    /// The tokens are matched as strings because `web/frontend` is a separate workspace and
    /// shares no types with the API; `services/api`'s `the_console_branches_on_these_problem_types`
    /// pins them from the other side.
    pub(crate) fn of(err: &ApiOpError<ProblemDetails>) -> Self {
        let ApiOpError::ErrorResponse(response) = err else {
            return Self::Other;
        };
        match response.title.as_str() {
            "step_up_required" => Self::StepUp,
            "mfa_enrolment_required" => Self::MfaEnrolment,
            _ => Self::Other,
        }
    }

    /// The best a bodyless operation can do: read the status alone.
    ///
    /// For the handful of routes whose only documented failure is the elevation demand — the
    /// account export and deletion, which publish no error schema — `403` has exactly one
    /// meaning, so this is precise there and a guess nowhere else.
    pub(crate) fn from_status(status: Option<u16>) -> Self {
        if status == Some(403) {
            Self::StepUp
        } else {
            Self::Other
        }
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

/// The wording for a call the API guards with a permission, an enrolment and an elevation.
///
/// Differs from [`friendly_error`] in one place, and it is the place that matters: an account
/// with no second factor is told to enrol one rather than that its privileges are insufficient —
/// which is a sentence nobody can act on, for the one refusal that has an obvious remedy. A
/// step-up demand never reaches here; the caller's [`crate::components::StepUpGate`] takes it
/// first and opens the prompt.
pub(crate) fn guarded_error(i18n: Translator, err: ApiOpError<ProblemDetails>) -> String {
    match Refusal::of(&err) {
        Refusal::MfaEnrolment => i18n.t("error.mfaEnrolmentRequired"),
        Refusal::StepUp | Refusal::Other => friendly_error(i18n, err),
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

    fn refusal_of(title: &str) -> Refusal {
        let body = ProblemDetails {
            detail: String::new(),
            status: 403,
            title: title.to_owned(),
            type_: serde_json::Value::String(format!("about:blank#{title}")),
        };
        Refusal::of(&ApiOpError::ErrorResponse(
            progenitor_client::ResponseValue::new(
                body,
                reqwest::StatusCode::FORBIDDEN,
                reqwest::header::HeaderMap::new(),
            ),
        ))
    }

    /// The bug: every operator action reported "you don't have permission to do that" for a
    /// refusal that was the server asking for a second factor. The console read the `403` and
    /// nothing else, so the one answer with a remedy was indistinguishable from the one without.
    #[test]
    fn the_three_forbidden_answers_are_told_apart() {
        assert_eq!(refusal_of("step_up_required"), Refusal::StepUp);
        assert_eq!(refusal_of("mfa_enrolment_required"), Refusal::MfaEnrolment);
        assert_eq!(refusal_of("forbidden"), Refusal::Other);
    }

    /// A transport or decode fault carries no problem body, and must not open a prompt: there is
    /// nothing on the other side that a second factor would satisfy.
    #[test]
    fn a_failure_with_no_problem_body_is_not_a_step_up_demand() {
        assert_eq!(
            Refusal::of(&ApiOpError::InvalidRequest("no body".to_owned())),
            Refusal::Other
        );
        assert_eq!(Refusal::from_status(None), Refusal::Other);
        assert_eq!(Refusal::from_status(Some(404)), Refusal::Other);
        assert_eq!(Refusal::from_status(Some(403)), Refusal::StepUp);
    }
}
