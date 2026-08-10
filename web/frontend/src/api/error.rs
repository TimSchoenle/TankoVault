//! Turning API operation failures into text a reader can act on.
//!
//! The generated client's `progenitor_client::Error` mixes transport, decode and error-response
//! faults into one enum; this maps it to a short sentence and, occasionally, the raw status code.

use crate::i18n::Translator;
use crate::state::account_wall::Admission;
use crate::wire::types::{ProblemDetails, ProblemKind};
use progenitor_client::Error as ApiOpError;

/// The HTTP status of a failed operation, when the failure was an error *response* rather
/// than a transport or decoding fault.
pub(crate) fn error_status<E>(err: &ApiOpError<E>) -> Option<u16> {
    match err {
        ApiOpError::ErrorResponse(response) => Some(response.status().as_u16()),
        _ => None,
    }
}

/// How long the server told the caller to wait, in milliseconds, when it answered `429`.
///
/// Only for the console's few *looping* operations — the catalogue purge repeats one endpoint
/// until it reports the job done — where the right response to a rate limit is to wait and carry
/// on, not to abandon a half-finished destructive operation and report a failure. Everything else
/// surfaces the refusal and lets the operator decide.
///
/// A `429` without a usable header still yields a duration: the limiter is a token bucket, so
/// there is always a time at which the next call succeeds, and guessing one badly is far better
/// than treating a temporary refusal as the end of the run.
pub(crate) fn retry_after_ms<E>(err: &ApiOpError<E>) -> Option<u32> {
    /// What to wait when the server refused without saying for how long.
    const FALLBACK_MS: u32 = 5_000;
    /// The ceiling on an honoured `Retry-After`. A header naming an hour would strand a purge
    /// behind a progress line that never moves; stopping and reporting is the better answer.
    const MAX_MS: u32 = 60_000;

    let ApiOpError::ErrorResponse(response) = err else {
        return None;
    };
    if response.status().as_u16() != 429 {
        return None;
    }
    let seconds = response
        .headers()
        .get("retry-after")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse::<u32>().ok());
    Some(
        seconds
            .map_or(FALLBACK_MS, |s| s.saturating_mul(1_000))
            .clamp(1_000, MAX_MS),
    )
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

/// What a failed call implies about anonymous access to this deployment.
///
/// `account_required` is the one refusal that is not about the request that drew it: it says the
/// server serves a signed-out caller nothing at all, which is what raises
/// [`crate::state::account_wall::AccountWall`]. `None` where the server did not answer — a
/// transport fault proves nothing about admission policy, and must not be read as either verdict.
pub(crate) fn admission(err: &ApiOpError<ProblemDetails>) -> Option<Admission> {
    match err {
        ApiOpError::ErrorResponse(response) => Some(match response.title {
            ProblemKind::AccountRequired => Admission::AccountRequired,
            _ => Admission::Public,
        }),
        // The server answered, just not with a problem body it could parse; it is answering
        // signed-out callers, which is the whole question here.
        ApiOpError::UnexpectedResponse(_) => Some(Admission::Public),
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
    /// Classify a published problem kind.
    ///
    /// Exhaustive on purpose: `openapi.json` is the only thing connecting this workspace to the
    /// API, so a token added there arrives here as a new generated variant and this match stops
    /// compiling until someone decides what the console should do with it. That is the whole
    /// point of publishing the vocabulary as an enum — the previous version matched string
    /// literals, and a renamed token would have compiled and silently classified as `Other`.
    pub(crate) fn from_kind(kind: ProblemKind) -> Self {
        match kind {
            ProblemKind::StepUpRequired => Self::StepUp,
            ProblemKind::MfaEnrolmentRequired => Self::MfaEnrolment,
            ProblemKind::NotFound
            | ProblemKind::Conflict
            | ProblemKind::Unauthorized
            | ProblemKind::AccountRequired
            | ProblemKind::Forbidden
            | ProblemKind::EmailNotVerified
            | ProblemKind::AccountSuspended
            | ProblemKind::FeatureDisabled
            | ProblemKind::RateLimited
            | ProblemKind::BadRequest
            | ProblemKind::Unavailable
            | ProblemKind::UpstreamUnavailable
            | ProblemKind::UpstreamTimeout
            | ProblemKind::Internal => Self::Other,
        }
    }

    /// Read the refusal off the server's problem body.
    pub(crate) fn of(err: &ApiOpError<ProblemDetails>) -> Self {
        match err {
            ApiOpError::ErrorResponse(response) => Self::from_kind(response.title),
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

    fn problem(title: ProblemKind, status: reqwest::StatusCode) -> ApiOpError<ProblemDetails> {
        let body = ProblemDetails {
            detail: String::new(),
            status: i32::from(status.as_u16()),
            title,
            type_: serde_json::Value::String(format!("about:blank#{title}")),
        };
        ApiOpError::ErrorResponse(progenitor_client::ResponseValue::new(
            body,
            status,
            reqwest::header::HeaderMap::new(),
        ))
    }

    fn refusal_of(title: ProblemKind) -> Refusal {
        Refusal::of(&problem(title, reqwest::StatusCode::FORBIDDEN))
    }

    /// The two `401`s mean opposite things to a signed-out client — "your session ended" and
    /// "this deployment serves you nothing" — and only the token tells them apart. Branching on
    /// the status instead would put the sign-in wall up on any expired session.
    #[test]
    fn only_the_account_required_token_reports_a_private_deployment() {
        let unauthorized = reqwest::StatusCode::UNAUTHORIZED;
        assert_eq!(
            admission(&problem(ProblemKind::AccountRequired, unauthorized)),
            Some(Admission::AccountRequired)
        );
        assert_eq!(
            admission(&problem(ProblemKind::Unauthorized, unauthorized)),
            Some(Admission::Public)
        );
    }

    /// A server that could not be reached has said nothing about who it admits, and reading
    /// that as either answer is how an offline client either walls itself or refuses to.
    #[test]
    fn a_transport_fault_is_not_an_admission_verdict() {
        assert_eq!(
            admission(&ApiOpError::InvalidRequest("no body".to_owned())),
            None
        );
    }

    /// Every problem kind the API publishes must be one this workspace classifies, under the
    /// token the API actually sends.
    ///
    /// Read out of the committed `openapi.json`, the artefact `crates/api-client` is generated
    /// from and the only thing that connects these two workspaces (`web/frontend` is outside the
    /// host workspace, so no compiler relates the API's enum to the generated one here). The
    /// parse is the assertion: a token published but absent from the generated vocabulary means
    /// the client is stale, and a real response carrying it would decode as
    /// `InvalidResponsePayload` — the server's message dropped, the reader shown "unreadable".
    ///
    /// The defect this closes: the console matched these tokens as string literals across the
    /// workspace boundary. Renaming one on the API side compiled on both sides and silently
    /// turned the step-up prompt back into "you don't have permission to do that" — the exact
    /// dead end the prompt exists to avoid.
    #[test]
    fn every_published_problem_kind_is_classified() {
        const SPEC: &str = include_str!("../../../../openapi.json");
        let spec: serde_json::Value = serde_json::from_str(SPEC).expect("openapi.json parses");

        let published: Vec<String> = spec["components"]["schemas"]["ProblemKind"]["enum"]
            .as_array()
            .expect("the document declares the ProblemKind vocabulary")
            .iter()
            .map(|v| v.as_str().expect("problem tokens are strings").to_owned())
            .collect();
        assert!(!published.is_empty());

        for token in &published {
            let kind: ProblemKind = token.parse().unwrap_or_else(|_| {
                panic!(
                    "the API publishes `{token}`, which the generated client does not carry; \
                     regenerate with `cargo run -p xtask -- openapi`"
                )
            });
            // Total by construction — `from_kind` is exhaustive — but this pins that the
            // published token, not just the variant, reaches a decision.
            let _ = Refusal::from_kind(kind);
        }

        // The three answers `403` has on a guarded route, each still published under the name
        // the console branches on.
        for (kind, expected) in [
            (ProblemKind::StepUpRequired, Refusal::StepUp),
            (ProblemKind::MfaEnrolmentRequired, Refusal::MfaEnrolment),
            (ProblemKind::Forbidden, Refusal::Other),
        ] {
            assert!(
                published.contains(&kind.to_string()),
                "`{kind}` is no longer published; the console branches on it"
            );
            assert_eq!(refusal_of(kind), expected);
        }
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
