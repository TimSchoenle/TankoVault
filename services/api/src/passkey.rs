//! The `WebAuthn` relying party: how it is configured, and the two things every passkey
//! handler needs from it.
//!
//! The handlers themselves live next to the flows they belong to — sign-in in
//! `crate::auth::passkey`, credential management in `crate::me::passkeys` — because those
//! are the modules a reader looking for "how does one log in" or "how do I revoke a key"
//! opens. What is here is the part both share and neither should re-decide: constructing the
//! relying party from configuration, storing and consuming ceremony state, and turning a
//! [`WebauthnError`] into an HTTP answer that discloses nothing.
//!
//! (`crate::me::passkeys` is written as a path rather than an intra-doc link because `me` is a
//! private module: rustdoc has no item to resolve, and `broken_intra_doc_links = "deny"` in the
//! workspace lint table turns that into a failed `cargo doc`. `auth` is private too, but
//! `auth::passkey` inside it is `pub`, which is enough for the link to resolve.)
//!
//! # Why the relying party is optional
//!
//! `WebAuthn` binds a credential to an **origin**, and the browser enforces that binding: a
//! credential registered at `https://tanko.example.com` cannot be used anywhere else, and a
//! relying-party id that does not match the page's origin makes the browser refuse the
//! ceremony outright. So the origin cannot be guessed from a request — a `Host` header is
//! attacker-controlled, and trusting it would let anyone register credentials under a domain
//! of their choosing. It has to be configured, and a deployment that has not configured it has
//! no relying party at all.
//!
//! That is why [`RelyingParty::from_config`] returns `Option` and the boot path logs loudly
//! rather than failing: passkeys are one feature of an application that must still start
//! without them. Every handler then answers `503` with a message naming the setting, which is
//! the one failure mode an operator can act on.

use std::sync::Arc;

use time::OffsetDateTime;
use uuid::Uuid;
use webauthn_rs::prelude::{Url, Webauthn, WebauthnBuilder, WebauthnError};

use crate::error::{ApiError, ApiResult};
use crate::state::AppState;
use tankovault_db::repo::users::webauthn::CeremonyKind;

/// How long the browser is told it may keep the authenticator prompt open.
///
/// Five minutes is long enough to find a phone, unlock it and approve, and short enough that
/// an abandoned prompt does not sit there as a live challenge.
const CEREMONY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);

/// How long the *server* keeps the matching state.
///
/// Deliberately longer than [`CEREMONY_TIMEOUT`]. The two deadlines are enforced by different
/// clocks — the browser's and Postgres's — and if the server's were the shorter one, a response
/// the browser was still willing to produce would arrive to find its challenge already gone.
/// The user would see the authenticator succeed and the sign-in fail, which is the least
/// diagnosable outcome available. The extra minute is slack for clock skew and the round trip,
/// not extra life for the challenge: the browser stops asking at five minutes regardless.
const CEREMONY_GRACE: time::Duration = time::Duration::seconds(360);

/// A configured relying party, plus the values it was built from so a failure can name them.
pub struct RelyingParty {
    webauthn: Webauthn,
    /// The relying-party id — the registrable domain credentials are bound to. Kept for
    /// diagnostics: a mismatch between this and the page's origin is the single most common
    /// way a passkey deployment fails, and the browser's error says nothing useful about it.
    rp_id: String,
    /// The exact origin the SPA is served from.
    origin: String,
}

/// Hand-written because [`Webauthn`] is not `Debug`, and because the useful debug output is
/// the pair of values a misconfiguration turns on anyway — the relying party's internals are
/// a fixed policy, while `rp_id` and `origin` are what an operator got wrong.
impl std::fmt::Debug for RelyingParty {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RelyingParty")
            .field("rp_id", &self.rp_id)
            .field("origin", &self.origin)
            .finish_non_exhaustive()
    }
}

impl RelyingParty {
    /// Build the relying party from configuration, or `None` when this deployment has not
    /// configured one.
    ///
    /// `origin` is the public origin of the web app (`https://tanko.example.com`). `rp_id`
    /// defaults to that origin's host, which is what a single-origin deployment always wants;
    /// it is configurable separately only for the case where the app is served from a
    /// subdomain and credentials should be usable across the parent domain.
    ///
    /// `rp_name` is the label the authenticator shows ("Save a passkey for …") and is resolved
    /// by the caller, not defaulted here: it is the deployment's own name, which this module
    /// has no business knowing.
    ///
    /// # Errors
    /// When `origin` is not a URL, has no host, or `rp_id` does not cover it — reported at
    /// boot rather than left to a browser's opaque `SecurityError`.
    pub fn from_config(
        origin: Option<&str>,
        rp_id: Option<&str>,
        rp_name: &str,
    ) -> anyhow::Result<Option<Self>> {
        let Some(origin) = origin.map(str::trim).filter(|s| !s.is_empty()) else {
            return Ok(None);
        };

        let url = Url::parse(origin).map_err(|e| {
            anyhow::anyhow!("webauthn_origin {origin:?} is not a valid URL: {e}. Expected the public origin of the web app, e.g. https://tanko.example.com")
        })?;
        let host = url.host_str().ok_or_else(|| {
            anyhow::anyhow!(
                "webauthn_origin {origin:?} has no host; a relying party cannot be derived from it"
            )
        })?;

        let rp_id = rp_id
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(host)
            .to_owned();

        let webauthn = WebauthnBuilder::new(&rp_id, &url)
            .map_err(|e| {
                anyhow::anyhow!(
                    "webauthn_rp_id {rp_id:?} does not cover webauthn_origin {origin:?}: {e}. \
                     The relying-party id must be the origin's host or a parent domain of it."
                )
            })?
            .rp_name(rp_name)
            .timeout(CEREMONY_TIMEOUT)
            .build()
            .map_err(|e| anyhow::anyhow!("could not build the WebAuthn relying party: {e}"))?;

        Ok(Some(Self {
            webauthn,
            rp_id,
            origin: url.to_string(),
        }))
    }

    /// The relying-party id credentials are bound to.
    #[must_use]
    pub fn rp_id(&self) -> &str {
        &self.rp_id
    }

    /// The origin the relying party was configured for.
    #[must_use]
    pub fn origin(&self) -> &str {
        &self.origin
    }
}

/// The relying party for this request, or the `503` that says why there is none.
///
/// Every passkey handler starts with this line. The error names the setting rather than saying
/// "unavailable", because the operator reading it in a log is the only person who can fix it
/// and the browser-side symptom of a missing origin gives them nothing to search for.
pub(crate) fn relying_party(state: &AppState) -> ApiResult<&Webauthn> {
    state
        .webauthn
        .as_deref()
        .map(|rp| &rp.webauthn)
        .ok_or_else(|| {
            tracing::warn!(
                "a passkey ceremony was requested but no WebAuthn relying party is configured; \
             set TANKOVAULT_AUTH__WEBAUTHN_ORIGIN to the public origin of the web app"
            );
            ApiError::Unavailable
        })
}

/// Persist an in-flight ceremony and return the handle the client echoes back.
///
/// The id is v4 rather than the v7 used for rows elsewhere: this is not a record, it is a
/// short-lived handle travelling through a client, and v7's embedded timestamp would disclose
/// when the ceremony started for no benefit. It is not a bearer credential either — holding it
/// gets you nothing without an authenticator that can sign the challenge inside — but it costs
/// nothing to keep it opaque.
///
/// # Errors
/// [`ApiError::Internal`] if serialising the ceremony state or the insert fails.
pub(crate) async fn begin_ceremony<S: serde::Serialize>(
    state: &AppState,
    user_id: Option<tankovault_domain::UserId>,
    kind: CeremonyKind,
    ceremony_state: &S,
) -> ApiResult<Uuid> {
    let serialised = serde_json::to_value(ceremony_state).map_err(|e| {
        tracing::error!(error = %e, "could not serialise webauthn ceremony state");
        ApiError::Internal
    })?;
    let id = Uuid::new_v4();
    tankovault_db::repo::users::webauthn::insert_ceremony(
        &state.pool,
        id,
        user_id,
        kind,
        &serialised,
        OffsetDateTime::now_utc() + CEREMONY_GRACE,
    )
    .await?;
    Ok(id)
}

/// Consume a ceremony and deserialise its state.
///
/// Consuming is a single `DELETE ... RETURNING` in the repository, so a challenge cannot be
/// used twice however this function returns.
///
/// # Errors
/// [`ApiError::Unauthorized`] for any lookup or deserialisation failure — collapsed into one
/// answer so a client cannot enumerate ceremony state by the failure mode.
pub(crate) async fn take_ceremony<S: serde::de::DeserializeOwned>(
    state: &AppState,
    id: Uuid,
    kind: CeremonyKind,
) -> ApiResult<(Option<tankovault_domain::UserId>, S)> {
    let ceremony = tankovault_db::repo::users::webauthn::take_ceremony(&state.pool, id, kind)
        .await?
        .ok_or(ApiError::Unauthorized)?;
    let parsed = serde_json::from_value(ceremony.state).map_err(|e| {
        tracing::warn!(
            error = %e,
            "a stored webauthn ceremony no longer deserialises; the client must start a new one"
        );
        ApiError::Unauthorized
    })?;
    Ok((ceremony.user_id, parsed))
}

/// Turn a verification failure into an HTTP answer.
///
/// **Always `401`, whatever went wrong.** `WebauthnError` distinguishes a bad signature from a
/// mismatched origin from an absent user-verification flag, and each of those is a fact about
/// the relying party's configuration or the victim's authenticator that an attacker would
/// otherwise learn by probing. The variant is logged, at `debug`, where the operator can see it
/// and the client cannot.
///
/// This is the same rule [`crate::auth::login`] follows for "unknown account" versus "wrong
/// password", applied to a protocol with far more ways to say no.
pub(crate) fn verification_failed(error: &WebauthnError) -> ApiError {
    tracing::debug!(error = ?error, "webauthn verification failed");
    ApiError::Unauthorized
}

/// A ceremony could not be started. Distinct from [`verification_failed`]: the inputs to a
/// `start_*` call are all ours, so a failure is a bug or a misconfiguration here, never
/// something the client did.
pub(crate) fn ceremony_start_failed(error: &WebauthnError) -> ApiError {
    tracing::error!(error = ?error, "could not start a webauthn ceremony");
    ApiError::Internal
}

/// Shorthand for the `Arc` [`AppState`] holds.
pub type SharedRelyingParty = Arc<RelyingParty>;

#[cfg(test)]
mod tests {
    use super::{CEREMONY_GRACE, CEREMONY_TIMEOUT, RelyingParty};

    /// The server must outlive the browser, or a response arrives to find its challenge gone.
    ///
    /// The failure this pins is not hypothetical arithmetic: with the server deadline the
    /// shorter of the two, the authenticator succeeds, the browser hands back a perfectly valid
    /// assertion, and the API answers `401`. Nothing in the response, the audit log or the
    /// browser console explains it, and it only reproduces for users who take a while to reach
    /// their phone.
    #[test]
    fn the_server_keeps_the_challenge_longer_than_the_browser_asks_for_it() {
        let browser = time::Duration::seconds(
            i64::try_from(CEREMONY_TIMEOUT.as_secs()).expect("a five-minute timeout fits in i64"),
        );
        assert!(
            CEREMONY_GRACE > browser,
            "server grace {CEREMONY_GRACE} must exceed the browser timeout {browser}"
        );
    }

    /// An unconfigured deployment gets `None`, not an error: passkeys are one feature, and the
    /// service has to boot without them.
    #[test]
    fn an_absent_origin_disables_passkeys_rather_than_failing() {
        assert!(
            RelyingParty::from_config(None, None, "TankoVault")
                .expect("no origin is not an error")
                .is_none()
        );
        assert!(
            RelyingParty::from_config(Some("   "), None, "TankoVault")
                .expect("a blank origin is not an error")
                .is_none()
        );
    }

    /// The relying-party id defaults to the origin's host, which is what a single-origin
    /// deployment always wants and what nobody should have to write out twice.
    #[test]
    fn the_relying_party_id_defaults_to_the_origins_host() {
        let rp = RelyingParty::from_config(Some("https://tanko.example.com"), None, "TankoVault")
            .expect("a valid origin builds")
            .expect("and yields a relying party");
        assert_eq!(rp.rp_id(), "tanko.example.com");
    }

    /// A relying-party id that does not cover the origin is refused at boot.
    ///
    /// The browser's own reaction to this mistake is to refuse every ceremony with an opaque
    /// `SecurityError` and no indication of which of the two values is wrong, so catching it
    /// where both are in front of us is the difference between a one-line log and an afternoon.
    #[test]
    fn a_relying_party_id_that_does_not_cover_the_origin_is_refused() {
        let err = RelyingParty::from_config(
            Some("https://tanko.example.com"),
            Some("example.org"),
            "TankoVault",
        )
        .expect_err("a foreign rp_id must not build");
        assert!(
            err.to_string().contains("does not cover"),
            "the refusal must name the mismatch, got: {err}"
        );
    }

    /// A malformed origin is a configuration error, reported in the operator's terms.
    #[test]
    fn a_malformed_origin_is_reported_as_configuration() {
        let err = RelyingParty::from_config(Some("tanko.example.com"), None, "TankoVault")
            .expect_err("a bare host is not an origin");
        assert!(err.to_string().contains("valid URL"), "got: {err}");
    }
}
