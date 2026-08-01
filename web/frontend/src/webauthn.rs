//! The two `navigator.credentials` calls, wrapped so a screen never touches `JsValue`.
//!
//! A passkey ceremony cannot be an HTTP call. The private key never leaves the authenticator,
//! so the browser has to broker it, and the only door is `navigator.credentials.create()` /
//! `.get()`. That makes this module the counterpart of [`crate::browser`]: a typed boundary
//! around a web API, so the rest of the app stays in Rust types and no screen has to reason
//! about a rejected promise.
//!
//! # What is *not* here
//!
//! Any JSON-to-`ArrayBuffer` translation. `PublicKeyCredentialCreationOptions` is a nested
//! structure whose `challenge`, `user.id` and `excludeCredentials[].id` fields are raw bytes
//! that arrive over the wire as base64url and must be `Uint8Array`s before the browser will
//! accept them — and the response has to travel back the other way. `webauthn-rs-proto`'s
//! `wasm` feature already implements all four conversions, and it is the *same crate the API
//! verifies with*, at the same pinned version. Re-deriving them here would be a second copy of
//! a specification, maintained against nothing.
//!
//! # No `eval`, and none needed
//!
//! Every call below is a typed `web-sys` binding. The served CSP carries no `'unsafe-eval'`
//! (see `web/frontend/clippy.toml`), and nothing here wants it: `js_sys::JSON` is the browser's
//! own parser, not a code path.

use wasm_bindgen::JsCast as _;
use wasm_bindgen::JsValue;
use wasm_bindgen_futures::JsFuture;
use webauthn_rs_proto::{
    CreationChallengeResponse, PublicKeyCredential, RegisterPublicKeyCredential,
    RequestChallengeResponse,
};

/// Why a ceremony did not produce a credential.
///
/// Three outcomes, because the screen says something different for each and lumping them
/// together produces the worst message in the set. A user who pressed Escape is not looking at
/// an error and must not be shown one; a user on a browser without `WebAuthn` needs to be told
/// that rather than to try again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CeremonyError {
    /// This browser exposes no `navigator.credentials`, or the page is not in a secure context.
    ///
    /// `navigator.credentials` is gated on a secure context, so plain-HTTP development hits
    /// this too — which is worth wording as "not available here" rather than "not supported",
    /// since the browser is usually perfectly capable and the *page* is the problem.
    Unsupported,
    /// The user dismissed the prompt, or it timed out.
    ///
    /// `NotAllowedError` is the browser's single answer to "the user said no", "the user did
    /// nothing", and "no credential matched" — it is deliberately indistinguishable so a page
    /// cannot probe which authenticators a visitor holds. So this is not necessarily a refusal,
    /// and the wording must not accuse anyone of one.
    Cancelled,
    /// Anything else: a malformed challenge, an origin mismatch, an authenticator fault. The
    /// browser's own message, which is the only thing that will help.
    Failed(String),
}

impl CeremonyError {
    /// The message-catalogue key this outcome should be worded with.
    ///
    /// Split out from the callers for the reason `crate::views::auth::sign_in_failure` is: the
    /// mapping is the part worth testing, and testing it must not require a browser.
    pub(crate) const fn key(&self) -> &'static str {
        match self {
            Self::Unsupported => "passkey.error.unsupported",
            Self::Cancelled => "passkey.error.cancelled",
            Self::Failed(_) => "passkey.error.failed",
        }
    }
}

/// Whether this browser and this page can run a ceremony at all.
///
/// Used to hide the passkey controls rather than to gate the call — a control that fails when
/// pressed is worse than one that is not offered. The call is still guarded, because a stale
/// render is not a security boundary.
pub(crate) fn is_available() -> bool {
    web_sys::window().is_some_and(|w| !JsValue::from(w.navigator().credentials()).is_undefined())
}

/// Register a new credential: `navigator.credentials.create()`.
///
/// # Errors
/// [`CeremonyError`], never a panic. A ceremony fails routinely — the user walks away, the
/// authenticator is already registered, the page is on plain HTTP — and a panic in WASM is an
/// `unreachable` trap that takes the whole app down (`panic = "abort"`), so every path here
/// returns.
pub(crate) async fn create(
    challenge: CreationChallengeResponse,
) -> Result<RegisterPublicKeyCredential, CeremonyError> {
    let credentials = container()?;
    let options = web_sys::CredentialCreationOptions::from(challenge);
    let promise = credentials
        .create_with_options(&options)
        .map_err(|e| classify(&e))?;
    let value = JsFuture::from(promise).await.map_err(|e| classify(&e))?;

    // `create()` resolves to `null` when the browser declines without throwing. Rare, but the
    // unchecked cast below would otherwise hand a null to the conversion and trap.
    let credential = value
        .dyn_into::<web_sys::PublicKeyCredential>()
        .map_err(|_| CeremonyError::Cancelled)?;
    Ok(RegisterPublicKeyCredential::from(credential))
}

/// Assert an existing credential: `navigator.credentials.get()`, as a modal prompt.
///
/// # Errors
/// [`CeremonyError`]; see [`create`].
pub(crate) async fn get(
    challenge: RequestChallengeResponse,
) -> Result<PublicKeyCredential, CeremonyError> {
    let credentials = container()?;
    let options = web_sys::CredentialRequestOptions::from(for_modal_prompt(challenge));
    let promise = credentials
        .get_with_options(&options)
        .map_err(|e| classify(&e))?;
    let value = JsFuture::from(promise).await.map_err(|e| classify(&e))?;

    let credential = value
        .dyn_into::<web_sys::PublicKeyCredential>()
        .map_err(|_| CeremonyError::Cancelled)?;
    Ok(PublicKeyCredential::from(credential))
}

/// Drop the `mediation` hint the API's challenge carries, so the ceremony opens a dialog.
///
/// # The bug this exists to prevent
///
/// `webauthn-rs`'s `start_discoverable_authentication()` unconditionally stamps
/// `mediation: Some(Conditional)` onto the challenge, and `webauthn-rs-proto`'s `wasm`
/// conversion serialises the *whole* envelope into the `CredentialRequestOptions` it hands to
/// the browser — so the field survives all the way to `navigator.credentials.get()`.
///
/// `mediation: "conditional"` is **Conditional UI**: the browser must not prompt. It parks the
/// promise and waits for the reader to pick a passkey out of an autofill dropdown on an
/// `<input autocomplete="… webauthn">`. There is no such input on the sign-in card, so the
/// promise never settled — pressing "Sign in with a passkey" showed no dialog, produced no
/// error, and left the button disabled with the [`crate::hooks::Busy`] guard still claimed.
/// "Nothing happens" was the whole symptom, and it looks identical to a dead `onclick`.
///
/// Mediation is a client-side presentation choice, not part of what the server verifies: the
/// stored `DiscoverableAuthentication` state, the challenge and the origin binding are all
/// untouched by this. So the decision belongs on this side of the wire, and stripping it here
/// covers every screen rather than the one that happens to remember. A future conditional-UI
/// affordance is a *second* entry point that opts back in — not a reason to leave the default
/// on a button that has no autofill surface to attach to.
fn for_modal_prompt(mut challenge: RequestChallengeResponse) -> RequestChallengeResponse {
    challenge.mediation = None;
    challenge
}

/// Parse the opaque challenge envelope the API sent into the typed options the browser wants.
///
/// The generated client hands these over as a `serde_json::Map`, because `openapi.json`
/// deliberately does not model the `WebAuthn` structures — see the module docs. This is where the
/// JSON becomes a type again, using the crate the server serialised it with.
///
/// # Errors
/// [`CeremonyError::Failed`] when the envelope is not the shape this crate version expects,
/// which means the API and this bundle were built against different `webauthn-rs-proto`
/// versions. Worth surfacing rather than swallowing: it is a deployment fault, and every
/// ceremony will fail until it is fixed.
pub(crate) fn parse_challenge<T: serde::de::DeserializeOwned>(
    options: serde_json::Map<String, serde_json::Value>,
) -> Result<T, CeremonyError> {
    serde_json::from_value(serde_json::Value::Object(options))
        .map_err(|e| CeremonyError::Failed(format!("unreadable challenge: {e}")))
}

/// Serialise a ceremony result back into the envelope the API expects.
///
/// # Errors
/// [`CeremonyError::Failed`] if the credential will not serialise into a JSON object. It always
/// does; the branch exists so a change upstream surfaces here rather than as a `400` from the
/// API with no local explanation.
pub(crate) fn to_envelope<T: serde::Serialize>(
    credential: &T,
) -> Result<serde_json::Map<String, serde_json::Value>, CeremonyError> {
    match serde_json::to_value(credential) {
        Ok(serde_json::Value::Object(map)) => Ok(map),
        Ok(_) => Err(CeremonyError::Failed(
            "credential did not serialise to an object".to_owned(),
        )),
        Err(e) => Err(CeremonyError::Failed(format!(
            "could not serialise the credential: {e}"
        ))),
    }
}

/// `navigator.credentials`, or [`CeremonyError::Unsupported`].
fn container() -> Result<web_sys::CredentialsContainer, CeremonyError> {
    let window = web_sys::window().ok_or(CeremonyError::Unsupported)?;
    let credentials = window.navigator().credentials();
    if JsValue::from(credentials.clone()).is_undefined() {
        return Err(CeremonyError::Unsupported);
    }
    Ok(credentials)
}

/// Turn a thrown `DOMException` into one of the three outcomes.
///
/// The name is read rather than the message, because the message is localised by the browser
/// and differs between them; `NotAllowedError` is stable and specified. `AbortError` is the
/// same event arriving through `AbortController` — a navigation away, or a second ceremony
/// superseding this one — and belongs with it: nothing went wrong and nobody should be told
/// anything did.
fn classify(error: &JsValue) -> CeremonyError {
    let Some(exception) = error.dyn_ref::<web_sys::DomException>() else {
        return CeremonyError::Failed(describe(error));
    };
    match exception.name().as_str() {
        "NotAllowedError" | "AbortError" => CeremonyError::Cancelled,
        // Every other `DOMException` is a real fault worth surfacing: `SecurityError` for an
        // origin the relying-party id does not cover, `InvalidStateError` for an authenticator
        // that already holds a credential for this account, `NotSupportedError` for a page
        // outside a secure context.
        _ => CeremonyError::Failed(format!("{}: {}", exception.name(), exception.message())),
    }
}

/// A human-readable rendering of a non-`DOMException` throw.
fn describe(error: &JsValue) -> String {
    error
        .as_string()
        .or_else(|| {
            js_sys::Reflect::get(error, &JsValue::from_str("message"))
                .ok()?
                .as_string()
        })
        .unwrap_or_else(|| "unknown error".to_owned())
}

#[cfg(test)]
mod tests {
    use super::CeremonyError;
    use webauthn_rs_proto::RequestChallengeResponse;

    /// A verbatim `/v1/auth/passkey/login/start` response, captured from the running API.
    ///
    /// Hard-coded rather than built from the proto types because the point of the test below is
    /// that the field arrives *from the server*, in the envelope, whether or not this crate
    /// thinks about it. Constructing the value locally would test our own literal.
    const SERVER_CHALLENGE: &str = r#"{
        "mediation": "conditional",
        "publicKey": {
            "allowCredentials": [],
            "challenge": "iwZUf9YFixFHQNSDNsvwUc5UIx2rd7OC7uOYEaQxY1k",
            "extensions": { "uvm": true },
            "rpId": "localhost",
            "timeout": 300000,
            "userVerification": "required"
        }
    }"#;

    /// The challenge handed to `navigator.credentials.get()` must carry no `mediation`.
    ///
    /// The bug: `webauthn-rs` forces `mediation: conditional` on every discoverable-auth
    /// challenge, and the `wasm` conversion passes it through to the browser. Conditional
    /// mediation forbids a prompt — the call waits, silently and forever, for an autofill pick
    /// on a `webauthn` input the sign-in card does not have. So the "Sign in with a passkey"
    /// button opened nothing, reported nothing, and stayed disabled behind a `Busy` guard that
    /// was never released. See [`super::for_modal_prompt`].
    ///
    /// This asserts on the parsed challenge rather than the `CredentialRequestOptions` because
    /// the latter only exists under `wasm32`; what regresses is this field surviving, and that
    /// is visible here.
    #[test]
    fn a_ceremony_started_from_a_button_does_not_ask_for_conditional_mediation() {
        let challenge: RequestChallengeResponse =
            super::parse_challenge(serde_json::from_str(SERVER_CHALLENGE).expect("valid JSON"))
                .expect("the captured envelope parses");
        assert!(
            challenge.mediation.is_some(),
            "the captured envelope must still carry the field, or this test proves nothing"
        );
        assert!(
            super::for_modal_prompt(challenge).mediation.is_none(),
            "conditional mediation reached the browser: the ceremony will never prompt"
        );
    }

    /// Each outcome must reach a *different* message, and the cancelled one must not be worded
    /// as a failure.
    ///
    /// The failure this guards is a product bug, not a crash: `NotAllowedError` is what a
    /// browser returns when the user simply pressed Escape, and it is also what it returns when
    /// no credential matched — deliberately indistinguishable, so a page cannot enumerate a
    /// visitor's authenticators. Collapsing it into the generic error bucket therefore shows
    /// "something went wrong" to a user who chose to cancel, on a screen where the natural next
    /// thought is that passkeys are broken here.
    #[test]
    fn each_outcome_is_worded_separately_and_cancellation_is_not_an_error() {
        let keys = [
            CeremonyError::Unsupported.key(),
            CeremonyError::Cancelled.key(),
            CeremonyError::Failed(String::new()).key(),
        ];
        let unique: std::collections::BTreeSet<_> = keys.iter().collect();
        assert_eq!(
            unique.len(),
            keys.len(),
            "two outcomes share a message: {keys:?}"
        );
        assert_ne!(
            CeremonyError::Cancelled.key(),
            CeremonyError::Failed(String::new()).key()
        );
    }
}
