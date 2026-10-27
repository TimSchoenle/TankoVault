//! What this deployment expects of the native client that connects to it.
//!
//! `api` publishes it at `GET /v1/client` for the desktop updater. It answers two questions an
//! installed client cannot answer for itself: which repository its releases come from, and which
//! client versions this deployment supports, so a reader is never moved onto a build their server
//! cannot talk to.
//!
//! Nothing here is a trust anchor. The client verifies every release against a signing key
//! compiled into it, whatever repository a server names (`web/frontend/src/update/discover.rs`).

use serde::Deserialize;
use terrace_config::schema::Describe;

/// The repository a stock deployment's client updates itself from.
const DEFAULT_RELEASE_REPO: &str = "TimSchoenle/TankoVault";

/// The client update channel this deployment names.
///
/// Every field defaults, so an absent `[client]` section is valid and publishes the upstream
/// channel with this build's own version as the ceiling.
#[derive(Debug, Clone, Deserialize, Describe)]
#[serde(default)]
pub struct ClientConfig {
    /// The GitHub repository, as `owner/name`, that the native client reads its releases from.
    ///
    /// Blank publishes none, and a client then keeps whichever repository it was built with.
    /// A fork that ships its own signed installers names itself here.
    pub release_repo: String,
    /// The oldest client version this deployment supports. Unset means no floor.
    pub min_version: Option<String>,
    /// The newest client version this deployment supports. Unset means the running service's own
    /// version, since client and server are released together at one version.
    pub max_version: Option<String>,
}

impl Default for ClientConfig {
    fn default() -> Self {
        Self {
            release_repo: DEFAULT_RELEASE_REPO.to_owned(),
            min_version: None,
            max_version: None,
        }
    }
}

impl ClientConfig {
    /// Returns the repository to publish, or `None` when the operator named none.
    #[must_use]
    pub fn release_repo(&self) -> Option<&str> {
        let repo = self.release_repo.trim();
        (!repo.is_empty()).then_some(repo)
    }

    /// Checks every value before the deployment starts serving it.
    ///
    /// A client that cannot read a version bound ignores it, so a typo has to fail the boot.
    ///
    /// # Errors
    /// A sentence naming the setting and what it holds, when the repository is not
    /// `owner/name` or a version is not `major.minor.patch`.
    pub fn validate(&self) -> Result<(), String> {
        if let Some(repo) = self.release_repo()
            && !is_repo_path(repo)
        {
            return Err(format!(
                "client.release_repo must be `owner/name`, not {repo:?}"
            ));
        }
        for (setting, value) in [
            ("client.min_version", self.min_version.as_deref()),
            ("client.max_version", self.max_version.as_deref()),
        ] {
            if let Some(value) = value
                && !is_plain_version(value)
            {
                return Err(format!(
                    "{setting} must be a plain `major.minor.patch` version, not {value:?}"
                ));
            }
        }
        Ok(())
    }
}

/// Returns whether `value` is a GitHub `owner/name` and nothing else.
///
/// The client interpolates this into `https://api.github.com/repos/{repo}/releases`, so a value
/// carrying `/`, `?`, `#` or `..` would address a different resource. The client checks it again
/// on arrival; checking it here stops an operator publishing a channel no client accepts.
fn is_repo_path(value: &str) -> bool {
    let mut parts = value.split('/');
    let (Some(owner), Some(name), None) = (parts.next(), parts.next(), parts.next()) else {
        return false;
    };
    [owner, name].iter().all(|part| {
        !part.is_empty()
            && *part != ".."
            && part
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    })
}

/// Returns whether `value` is a `major.minor.patch` of digits.
///
/// This is what the client accepts: plain released tags, without prerelease or build metadata,
/// because `release-please` cuts nothing else.
fn is_plain_version(value: &str) -> bool {
    let mut parts = value.split('.');
    let (Some(major), Some(minor), Some(patch), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return false;
    };
    [major, minor, patch]
        .iter()
        .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_absent_section_publishes_the_upstream_channel() {
        let config = ClientConfig::default();
        assert_eq!(config.release_repo(), Some(DEFAULT_RELEASE_REPO));
        assert!(config.validate().is_ok());
    }

    #[test]
    fn a_blank_repository_publishes_none() {
        let config = ClientConfig {
            release_repo: "   ".to_owned(),
            ..ClientConfig::default()
        };
        assert_eq!(config.release_repo(), None);
        assert!(config.validate().is_ok());
    }

    /// A repository that could address a different GitHub API resource fails the boot.
    #[test]
    fn a_repository_that_is_not_owner_slash_name_is_refused() {
        for repo in [
            "TimSchoenle",
            "TimSchoenle/TankoVault/extra",
            "TimSchoenle/TankoVault?per_page=1",
            "TimSchoenle/TankoVault#x",
            "../../TimSchoenle/TankoVault",
            "TimSchoenle/..",
            "/TankoVault",
            "TimSchoenle/",
            "https://github.com/TimSchoenle/TankoVault",
        ] {
            let config = ClientConfig {
                release_repo: repo.to_owned(),
                ..ClientConfig::default()
            };
            assert!(config.validate().is_err(), "{repo}");
        }
        assert!(is_repo_path("TimSchoenle/TankoVault"));
        assert!(is_repo_path("a-fork_1/tanko.vault"));
    }

    /// A bound the client would discard fails the boot, since it would read as a ceiling and act
    /// as none.
    #[test]
    fn a_version_bound_that_is_not_a_plain_release_is_refused() {
        for version in ["2.1", "v2.1.0", "2.1.0-rc.1", "2.1.0+build", "latest", ""] {
            let config = ClientConfig {
                max_version: Some(version.to_owned()),
                ..ClientConfig::default()
            };
            assert!(config.validate().is_err(), "{version}");
        }
        assert!(is_plain_version("2.1.0"));
        assert!(is_plain_version("0.0.0"));
        assert!(is_plain_version("10.20.30"));
    }
}
