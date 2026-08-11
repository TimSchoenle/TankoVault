//! What this deployment expects of the native client that connects to it.
//!
//! **Unauthenticated on purpose**, like `/v1/branding`: the desktop updater runs from the moment
//! the app starts, including on the sign-in screen and while a session is being re-adopted, and
//! a client that had to authenticate before it learned which releases its server supports would
//! simply never learn it.
//!
//! Two things are published, and the second is the point of the endpoint. The **repository** is
//! where the client reads its releases from, so a fork's readers are offered the fork's
//! installers rather than this project's. The **version range** is the compatibility contract:
//! a client honours it as a ceiling, and therefore never installs a build newer than the server
//! it is pointed at can talk to.
//!
//! Naming a repository here does **not** make this deployment a trust anchor for what a reader
//! runs. The client verifies every release against a signing key compiled into it and refuses
//! anything else, so the worst a hostile server can name is a repository whose releases it will
//! then reject — see `web/frontend/src/update/discover.rs`.

use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::http::header::CACHE_CONTROL;
use axum::response::{IntoResponse as _, Response};
use serde::Serialize;
use tankovault_config::ClientConfig;
use utoipa::ToSchema;

use crate::openapi::CLIENT_TAG;
use crate::state::AppState;

/// How long a client may reuse the channel without revalidating.
///
/// The same five minutes `/v1/branding` uses. It changes only when the operator restarts, and
/// the updater asks for it far less often than that.
const CACHE_POLICY: &str = "public, max-age=300";

/// The resolved channel, held in [`AppState`] and served verbatim.
///
/// Resolution happens once at boot — unlike the branding's copyright year, nothing in it depends
/// on the request.
#[derive(Debug, Clone)]
pub struct ClientChannel(Arc<ClientView>);

impl ClientChannel {
    /// Resolve the configured channel, filling the ceiling in from `service_version` when the
    /// operator set none.
    ///
    /// # Errors
    /// A sentence naming the setting and what it holds, for a boot that should not continue: a
    /// client that cannot read the ceiling falls back to having none, so an unusable value here
    /// would silently restore the behaviour the range exists to remove.
    pub fn new(config: &ClientConfig, service_version: &str) -> Result<Self, String> {
        config.validate()?;
        Ok(Self(Arc::new(ClientView {
            release_repo: config.release_repo().map(str::to_owned),
            min_version: config.min_version.clone(),
            max_version: config
                .max_version
                .clone()
                .unwrap_or_else(|| service_version.to_owned()),
        })))
    }
}

/// The client channel this deployment names.
#[derive(Debug, Serialize, ToSchema)]
pub struct ClientView {
    /// The GitHub repository the native client reads its releases from, as `owner/name`.
    /// Absent leaves the client on whichever repository it was built with.
    pub release_repo: Option<String>,
    /// The oldest client version this deployment supports. Absent means no floor.
    pub min_version: Option<String>,
    /// The newest client version this deployment supports. A client does not install past it.
    pub max_version: String,
}

/// Read the client update channel
///
/// Which repository the native client updates from, and the version range this deployment
/// supports. Unauthenticated: the updater runs before there is a session.
#[utoipa::path(
    get,
    path = "/v1/client",
    tag = CLIENT_TAG,
    responses(
        (status = 200, description = "This deployment's client channel", body = ClientView),
    )
)]
pub async fn client_channel(State(state): State<AppState>) -> Response {
    (
        [(CACHE_CONTROL, CACHE_POLICY)],
        Json(Arc::clone(&state.client_channel.0)),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unset_ceiling_resolves_to_the_running_service() {
        let channel = ClientChannel::new(&ClientConfig::default(), "2.4.1").expect("stock config");
        assert_eq!(channel.0.max_version, "2.4.1");
        assert_eq!(
            channel.0.release_repo.as_deref(),
            Some("TimSchoenle/TankoVault")
        );
        assert_eq!(channel.0.min_version, None);
    }

    #[test]
    fn a_configured_ceiling_wins_over_the_running_service() {
        let channel = ClientChannel::new(
            &ClientConfig {
                max_version: Some("2.0.0".to_owned()),
                ..ClientConfig::default()
            },
            "2.4.1",
        )
        .expect("a plain version");
        assert_eq!(channel.0.max_version, "2.0.0");
    }

    /// A value no client would honour fails the boot rather than reaching the wire — the client
    /// discards a malformed bound, and a discarded ceiling is no ceiling.
    #[test]
    fn an_unusable_setting_refuses_to_resolve() {
        let error = ClientChannel::new(
            &ClientConfig {
                release_repo: "https://github.com/TimSchoenle/TankoVault".to_owned(),
                ..ClientConfig::default()
            },
            "2.4.1",
        )
        .expect_err("a URL is not `owner/name`");
        assert!(error.contains("client.release_repo"), "{error}");
    }
}
