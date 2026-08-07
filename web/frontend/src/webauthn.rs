//! The two `navigator.credentials` calls, wrapped so a screen never touches `JsValue`.
//!
//! The private key never leaves the authenticator, so the browser must broker it through
//! `navigator.credentials.create()` / `.get()` — this is the typed boundary around that, the
//! counterpart to [`crate::platform`].
//!
//! Byte-level conversion (challenge, `user.id`, `excludeCredentials[].id`) is intentionally not
//! reimplemented here: `webauthn-rs-proto`'s `wasm` feature already does it, at the same pinned
//! version the API verifies with. Every call below is a typed `web-sys` binding — the served CSP
//! carries no `'unsafe-eval'`, and none is needed.
//!
//! **The desktop build does not go through a webview, and must not try to.** `WebAuthn` in a
//! browser requires `rp.id` to be a registrable suffix of the *document's* origin, and a wry
//! webview serves this app from its own custom protocol — so a challenge naming the server's
//! relying-party id is refused with `SecurityError` however the call is reached. That rule is
//! the *browser's*, not `WebAuthn`'s: it is how a page is stopped from asserting an origin it
//! does not occupy.
//!
//! Windows exposes the same ceremony natively through `webauthn.dll`, which is the API the
//! browsers themselves call, and it takes the `clientDataJSON` — origin included — from the
//! caller. So the desktop build talks to Windows Hello directly and claims the origin the reader
//! connected to. **The origin binding is therefore this app's assertion rather than a browser's
//! guarantee**, which is the accepted model for a native client and is worth knowing: Windows has
//! no app-to-relying-party association (no equivalent of Android's Digital Asset Links), so any
//! native process on the machine can ask for the same `rp.id`. The server cannot tell one from
//! the other, and a passkey therefore proves possession of the credential, not that the request
//! came from this app.
//!
//! Linux has no OS passkey provider, so [`is_available`] answers `false` there and the controls
//! hide themselves — which is what that function is for. Hardware security keys over CTAP/HID
//! would be a separate feature with a separate dependency set.

#[cfg(feature = "web")]
use wasm_bindgen::JsCast as _;
#[cfg(feature = "web")]
use wasm_bindgen::JsValue;
#[cfg(feature = "web")]
use wasm_bindgen_futures::JsFuture;
use webauthn_rs_proto::{
    CreationChallengeResponse, PublicKeyCredential, RegisterPublicKeyCredential,
    RequestChallengeResponse,
};

/// Why a ceremony did not produce a credential.
///
/// One outcome per thing the screen should say, because lumping them together produces the worst
/// message in the set. A user who pressed Escape is not looking at an error and must not be shown
/// one; a user with no `WebAuthn` at all needs to be told that rather than to try again.
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
    /// The prompt closed without producing a credential, and the platform did not say why.
    ///
    /// Windows only, and it exists because [`Cancelled`](Self::Cancelled) cannot be reached
    /// there: `webauthn-authenticator-rs`'s Win10 backend maps every `HRESULT` — the user
    /// pressing Cancel included — onto one opaque error, so a ceremony that was declined and one
    /// that faulted are the same value by the time they arrive here.
    ///
    /// A fourth outcome rather than folding it into either neighbour, because both foldings lie:
    /// as `Cancelled` it would silently swallow a misconfigured relying party, and as
    /// [`Failed`](Self::Failed) it would tell a reader who pressed Cancel that something broke.
    /// The wording names both possibilities instead. Fixing it properly is an upstream patch
    /// mapping `ERROR_CANCELLED`/`NTE_USER_CANCELLED`.
    #[cfg_attr(
        all(feature = "web", not(test)),
        expect(
            dead_code,
            reason = "browsers distinguish cancellation, so only the Windows backend needs this"
        )
    )]
    Incomplete,
    /// Anything else: a malformed challenge, an origin mismatch, an authenticator fault. The
    /// platform's own message, which is the only thing that will help.
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
            Self::Incomplete => "passkey.error.incomplete",
            Self::Failed(_) => "passkey.error.failed",
        }
    }
}

/// Whether this browser and this page can run a ceremony at all.
///
/// Used to hide the passkey controls rather than to gate the call — a control that fails when
/// pressed is worse than one that is not offered. The call is still guarded, because a stale
/// render is not a security boundary.
#[cfg(feature = "web")]
pub(crate) fn is_available() -> bool {
    web_sys::window().is_some_and(|w| !JsValue::from(w.navigator().credentials()).is_undefined())
}

/// Whether Windows exposes its `WebAuthn` API, and whether a server has been chosen to name as
/// the relying party.
///
/// Both halves matter: the ceremony asserts an origin, and before the first-run connection screen
/// has been answered there is no origin to assert.
#[cfg(all(feature = "desktop", windows))]
pub(crate) fn is_available() -> bool {
    webauthn_authenticator_rs::win10::Win10::api_version() > 0
        && crate::platform::server_origin().is_some()
}

/// Always `false`: no OS passkey provider outside Windows. See the module contract.
#[cfg(all(feature = "desktop", not(windows)))]
pub(crate) const fn is_available() -> bool {
    false
}

/// Register a new credential: `navigator.credentials.create()`.
///
/// # Errors
/// [`CeremonyError`], never a panic. A ceremony fails routinely — the user walks away, the
/// authenticator is already registered, the page is on plain HTTP — and a panic in WASM is an
/// `unreachable` trap that takes the whole app down (`panic = "abort"`), so every path here
/// returns.
#[cfg(feature = "web")]
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
#[cfg(feature = "web")]
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

/// Windows Hello, through the platform's own `webauthn.dll`.
///
/// The same two ceremonies, reached through `webauthn-authenticator-rs`'s `Win10` backend rather
/// than through a browser. It is pinned to the `webauthn-rs-proto` this crate and the API already
/// share, so the challenge the API minted goes to the authenticator and the result comes back
/// with no conversion anyone here had to write.
#[cfg(all(feature = "desktop", windows))]
mod hello {
    use super::{
        CeremonyError, CreationChallengeResponse, PublicKeyCredential, RegisterPublicKeyCredential,
        RequestChallengeResponse,
    };
    use webauthn_authenticator_rs::prelude::{Url, WebauthnCError};
    use webauthn_authenticator_rs::{win10::Win10, AuthenticatorBackend as _};

    /// Used when the relying party sets no timeout. Matches the API's own ceremony timeout, so a
    /// prompt does not outlive the challenge behind it.
    const DEFAULT_TIMEOUT_MS: u32 = 300_000;

    pub(crate) async fn create(
        challenge: CreationChallengeResponse,
    ) -> Result<RegisterPublicKeyCredential, CeremonyError> {
        let origin = ceremony_origin()?;
        let options = challenge.public_key;
        let timeout = options.timeout.unwrap_or(DEFAULT_TIMEOUT_MS);
        off_thread(move || Win10::default().perform_register(origin, options, timeout)).await
    }

    pub(crate) async fn get(
        challenge: RequestChallengeResponse,
    ) -> Result<PublicKeyCredential, CeremonyError> {
        let origin = ceremony_origin()?;
        // No `for_modal_prompt` here: `mediation` is a browser presentation hint and the native
        // API has no field for it, so the conditional-UI trap the web side works around cannot
        // arise. Windows always prompts.
        let options = challenge.public_key;
        let timeout = options.timeout.unwrap_or(DEFAULT_TIMEOUT_MS);
        off_thread(move || Win10::default().perform_auth(origin, options, timeout)).await
    }

    /// The origin this client claims in `clientDataJSON`, which the server checks against the
    /// relying party it was configured with.
    ///
    /// It is the server the reader connected to, because that is the origin this app is acting
    /// for — and in a working deployment it is also the one the web SPA is served from, since
    /// both are the address the reader reaches the API at.
    fn ceremony_origin() -> Result<Url, CeremonyError> {
        let origin = crate::platform::server_origin().ok_or(CeremonyError::Unsupported)?;
        Url::parse(&origin).map_err(|e| {
            CeremonyError::Failed(format!(
                "configured server {origin:?} is not a valid URL: {e}"
            ))
        })
    }

    /// Run a ceremony on its own thread and await the answer.
    ///
    /// `webauthn.dll` shows a modal system dialog and does not return until the reader has
    /// finished with it. On the UI thread that is a frozen window for as long as they take —
    /// including the full timeout if they walk away.
    ///
    /// A plain `std::thread` plus a channel rather than `spawn_blocking`, because that would
    /// assume an ambient Tokio runtime on whichever executor Dioxus polls this future on.
    async fn off_thread<T, F>(ceremony: F) -> Result<T, CeremonyError>
    where
        T: Send + 'static,
        F: FnOnce() -> Result<T, WebauthnCError> + Send + 'static,
    {
        let (tx, rx) = tokio::sync::oneshot::channel();
        std::thread::spawn(move || {
            // The receiver is gone if the screen was left mid-ceremony; nothing to report to.
            let _ = tx.send(ceremony());
        });
        match rx.await {
            Ok(result) => result.map_err(classify),
            Err(_) => Err(CeremonyError::Failed(
                "the passkey ceremony ended without an answer".to_owned(),
            )),
        }
    }

    /// Map the backend's error onto what the screen should say.
    ///
    /// `Internal` is deliberately **not** folded into either neighbour — see
    /// [`CeremonyError::Incomplete`]. The Win10 backend maps every `HRESULT` onto it, so a
    /// declined prompt and a genuine fault are the same value here.
    fn classify(error: WebauthnCError) -> CeremonyError {
        match error {
            WebauthnCError::Cancelled => CeremonyError::Cancelled,
            WebauthnCError::Internal => CeremonyError::Incomplete,
            WebauthnCError::NotSupported | WebauthnCError::PlatformAuthenticator => {
                CeremonyError::Unsupported
            }
            other => CeremonyError::Failed(other.to_string()),
        }
    }
}

#[cfg(all(feature = "desktop", windows))]
pub(crate) use hello::{create, get};

/// The non-Windows desktop counterparts, which refuse rather than pretend.
///
/// They exist so the screens are one code path on every build: the controls are already hidden on
/// [`is_available`], and this is the guard behind them for a stale render.
#[cfg(all(feature = "desktop", not(windows)))]
mod unavailable {
    use super::{
        CeremonyError, CreationChallengeResponse, PublicKeyCredential, RegisterPublicKeyCredential,
        RequestChallengeResponse,
    };

    /// Mirrors the platform signature, which has to await the authenticator.
    pub(crate) async fn create(
        _challenge: CreationChallengeResponse,
    ) -> Result<RegisterPublicKeyCredential, CeremonyError> {
        Err(CeremonyError::Unsupported)
    }

    /// See [`create`].
    pub(crate) async fn get(
        _challenge: RequestChallengeResponse,
    ) -> Result<PublicKeyCredential, CeremonyError> {
        Err(CeremonyError::Unsupported)
    }
}

#[cfg(all(feature = "desktop", not(windows)))]
pub(crate) use unavailable::{create, get};

/// Drop the `mediation` hint the API's challenge carries, so the ceremony opens a dialog.
///
/// `webauthn-rs` stamps `mediation: Some(Conditional)` onto every discoverable-auth challenge,
/// and it survives verbatim into `CredentialRequestOptions`. Conditional UI never prompts — it
/// waits for an autofill pick on a `webauthn` input the sign-in card doesn't have, so the promise
/// never settles and "Sign in with a passkey" silently does nothing. See the test below.
///
/// Mediation is a client-side presentation choice, not something the server verifies, so
/// stripping it here is safe and covers every caller.
#[cfg(feature = "web")]
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
#[cfg(feature = "web")]
fn container() -> Result<web_sys::CredentialsContainer, CeremonyError> {
    let window = web_sys::window().ok_or(CeremonyError::Unsupported)?;
    let credentials = window.navigator().credentials();
    if JsValue::from(credentials.clone()).is_undefined() {
        return Err(CeremonyError::Unsupported);
    }
    Ok(credentials)
}

/// Turn a thrown `DOMException` into one of the outcomes.
///
/// The name is read rather than the message, because the message is localised by the browser
/// and differs between them; `NotAllowedError` is stable and specified. `AbortError` is the
/// same event arriving through `AbortController` — a navigation away, or a second ceremony
/// superseding this one — and belongs with it: nothing went wrong and nobody should be told
/// anything did.
#[cfg(feature = "web")]
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
#[cfg(feature = "web")]
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
    #[cfg(feature = "web")]
    use webauthn_rs_proto::RequestChallengeResponse;

    /// A verbatim `/v1/auth/passkey/login/start` response, captured from the running API.
    ///
    /// Hard-coded rather than built from the proto types because the point of the test below is
    /// that the field arrives *from the server*, in the envelope, whether or not this crate
    /// thinks about it. Constructing the value locally would test our own literal.
    #[cfg(feature = "web")]
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
    ///
    /// Web-only because the ceremony it guards is: the desktop build cannot reach an
    /// authenticator at all (see the module contract), so `for_modal_prompt` does not exist there.
    #[cfg(feature = "web")]
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
    ///
    /// `Incomplete` is in the set for the same reason from the other direction: Windows cannot
    /// tell a declined prompt from a fault, so it gets wording that names both rather than
    /// borrowing either neighbour's — and that only means something while its message stays its
    /// own.
    #[test]
    fn each_outcome_is_worded_separately_and_cancellation_is_not_an_error() {
        let keys = [
            CeremonyError::Unsupported.key(),
            CeremonyError::Cancelled.key(),
            CeremonyError::Incomplete.key(),
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
