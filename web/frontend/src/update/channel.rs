//! What the server this client is connected to says about updating it.
//!
//! Two things come from `GET /v1/client` rather than from this binary. **Which repository** the
//! releases are read from, so a fork's readers are offered the fork's installers. And **which
//! client versions the deployment supports**, which is the reason the endpoint exists: client and
//! server speak one API, so a client that runs ahead of its server talks to a deployment that has
//! never heard of half of what it sends. The range is a ceiling, and [`super::look`] never offers
//! a release above it.
//!
//! **The server is not thereby trusted with what the reader runs.** It names a repository; it
//! does not name a key. Every release is still verified against a signing key compiled into this
//! binary ([`super::discover`]), so the worst a hostile server can do is name a repository whose
//! releases this client then refuses. What it *is* trusted with is the URL that request goes to,
//! which is why [`is_repo_path`] refuses anything but `owner/name` — the value is interpolated
//! into a `api.github.com` path, and a `/`, `?` or `..` in it would address something else.
//!
//! This is the one part of the updater that talks to the reader's own server, and it does so
//! through [`crate::api`] — the credentialled client, correctly: it is their deployment. The
//! GitHub half keeps its own client with no credential of any kind; see [`super`].
//!
//! The answer is cached in `settings.json` beside the other update settings, keyed by the origin
//! it came from. A server that is down at check time leaves the last answer standing rather than
//! silently restoring "no ceiling"; a client repointed at another server ignores the previous
//! one's.

use serde::{Deserialize, Serialize};

/// The cached channel, and the origin it was read from.
const CHANNEL_KEY: &str = "tv-update-channel";

/// The repository this build falls back to when no server has named one.
///
/// A constant, not a preference: it decides where an executable this app will *run* comes from
/// when nothing else has said, and a settings file is writable by anything running as the reader.
/// A fork changes this line and the keys in [`super::discover`].
const FALLBACK_REPO: &str = "TimSchoenle/TankoVault";

/// The channel in force for this check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Channel {
    /// `owner/name` on GitHub.
    pub(crate) repo: String,
    /// Which client versions the server supports.
    pub(crate) supported: Range,
}

/// The client versions a deployment supports. An absent bound is no bound.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Range {
    min: Option<semver::Version>,
    max: Option<semver::Version>,
}

impl Range {
    /// Whether `version` is one this deployment supports.
    pub(crate) fn contains(&self, version: &semver::Version) -> bool {
        self.min.as_ref().is_none_or(|min| version >= min)
            && self.max.as_ref().is_none_or(|max| version <= max)
    }

    /// Whether `version` is refused for being **newer** than this deployment supports.
    ///
    /// Told apart from the floor because only this end has a sentence to say: "your server
    /// supports up to X" is an answer, and there is no equivalent for a release below the floor
    /// — one of those is a version the reader could not use either, so it is simply not offered.
    pub(crate) fn exceeds_ceiling(&self, version: &semver::Version) -> bool {
        self.max.as_ref().is_some_and(|max| version > max)
    }

    /// The ceiling, for the sentence that explains why a release was not offered. `None` when
    /// there is none — in which case nothing can be refused for being too new.
    pub(crate) fn ceiling(&self) -> Option<String> {
        self.max.as_ref().map(ToString::to_string)
    }
}

impl Default for Channel {
    fn default() -> Self {
        Self {
            repo: FALLBACK_REPO.to_owned(),
            supported: Range::default(),
        }
    }
}

/// The document as it is stored and as the server sends it.
///
/// Strings rather than parsed versions, because this is also the on-disk shape and the file is
/// writable by anything running as the reader — so it is parsed on the way out either way.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Stored {
    /// The server this was read from. A client repointed elsewhere must not honour it.
    origin: String,
    repo: Option<String>,
    min: Option<String>,
    max: Option<String>,
}

/// Ask the connected server for its channel and cache the answer.
///
/// Silent on every failure, and deliberately: the previous answer stands, and a first run with no
/// answer at all falls back to [`FALLBACK_REPO`] with no ceiling — which is what this client did
/// before the endpoint existed. Refusing to update because a server did not answer would turn one
/// unreachable deployment into a client that can never take a security fix.
pub(crate) async fn refresh(api: crate::api::Api) {
    let origin = crate::platform::origin();
    if origin.is_empty() {
        return;
    }
    let Ok(view) = api.client().client_channel().send().await else {
        return;
    };
    let view = view.into_inner();
    let stored = Stored {
        origin,
        repo: view.release_repo.clone(),
        min: view.min_version.clone(),
        max: Some(view.max_version.clone()),
    };
    // Written only once it has been read back the way a later check will read it. A document
    // this client would discard is worse than the one already on disk: it reads as a configured
    // ceiling and behaves as none.
    if parse(&stored).is_none() {
        return;
    }
    if let Ok(text) = serde_json::to_string(&stored) {
        crate::platform::store_set(CHANNEL_KEY, &text);
    }
}

/// The channel in force, from the cache, or the compiled-in fallback.
pub(crate) fn current() -> Channel {
    cached().unwrap_or_default()
}

/// The cached channel, if there is a usable one for the server currently configured.
fn cached() -> Option<Channel> {
    let stored: Stored = serde_json::from_str(&crate::platform::store_get(CHANNEL_KEY)?).ok()?;
    (stored.origin == crate::platform::origin())
        .then(|| parse(&stored))
        .flatten()
}

/// A stored document as a usable channel, or `None` if any part of it is not.
///
/// All-or-nothing rather than field-by-field: a document with one unreadable bound is a document
/// from something that is not the endpoint, and taking the half that parsed would mean honouring
/// a repository named by it while dropping the ceiling that came with it.
fn parse(stored: &Stored) -> Option<Channel> {
    let repo = match stored.repo.as_deref().map(str::trim) {
        // The server named none: it publishes no channel of its own, and this build's fallback
        // is the honest answer.
        None | Some("") => FALLBACK_REPO.to_owned(),
        Some(repo) if is_repo_path(repo) => repo.to_owned(),
        Some(_) => return None,
    };
    Some(Channel {
        repo,
        supported: Range {
            min: bound(stored.min.as_deref())?,
            max: bound(stored.max.as_deref())?,
        },
    })
}

/// One end of the range: `Some(None)` for absent, `None` for present and unusable.
///
/// Prerelease and build metadata are rejected rather than compared, so a bound accepts exactly
/// the set [`super::discover::version_of`] offers — a bound that cannot be compared against a
/// candidate is not a bound.
fn bound(value: Option<&str>) -> Option<Option<semver::Version>> {
    let Some(value) = value else {
        return Some(None);
    };
    let version = semver::Version::parse(value.trim()).ok()?;
    (version.pre.is_empty() && version.build.is_empty()).then_some(Some(version))
}

/// Whether `value` is a GitHub `owner/name` and nothing else.
///
/// The security-relevant check on this whole module: the value is interpolated into
/// `https://api.github.com/repos/{repo}/releases`, so a `/`, `?`, `#` or `..` in it would send
/// the request somewhere else entirely. The server that supplied it validates the same shape,
/// which is why an operator hears about a typo — this is what makes a server that did not,
/// or one that is not the operator's, unable to redirect the request.
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

#[cfg(test)]
impl Range {
    /// A range built by hand, for the tests in [`super::discover`]. The real one only ever
    /// arrives over the wire, through [`parse`].
    pub(crate) fn between(min: Option<&str>, max: Option<&str>) -> Self {
        let parse = |value: Option<&str>| value.map(|v| semver::Version::parse(v).expect("semver"));
        Self {
            min: parse(min),
            max: parse(max),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stored(repo: Option<&str>, min: Option<&str>, max: Option<&str>) -> Stored {
        Stored {
            origin: "https://tanko.example".to_owned(),
            repo: repo.map(str::to_owned),
            min: min.map(str::to_owned),
            max: max.map(str::to_owned),
        }
    }

    #[test]
    fn a_server_that_names_no_repository_leaves_this_build_on_its_own() {
        let channel = parse(&stored(None, None, Some("2.1.0"))).expect("a usable document");
        assert_eq!(channel.repo, FALLBACK_REPO);
        assert_eq!(channel.supported.ceiling().as_deref(), Some("2.1.0"));
    }

    /// The repository is interpolated into a GitHub API path, so a value that could address a
    /// different resource has to make the whole document unusable rather than be sanitised into
    /// something plausible.
    #[test]
    fn a_repository_that_is_not_owner_slash_name_is_refused() {
        for repo in [
            "TimSchoenle",
            "TimSchoenle/TankoVault/releases",
            "TimSchoenle/TankoVault?per_page=1",
            "../../evil/repo",
            "TimSchoenle/..",
            "evil/repo#x",
            "https://evil.example/repo",
            "/TankoVault",
        ] {
            assert!(parse(&stored(Some(repo), None, None)).is_none(), "{repo}");
        }
        assert!(parse(&stored(Some("a-fork_1/tanko.vault"), None, None)).is_some());
    }

    /// A bound this client cannot compare against a candidate is not a ceiling, and a document
    /// carrying one is discarded whole — keeping the previous answer, or this build's fallback,
    /// rather than honouring the repository while quietly dropping the limit that came with it.
    #[test]
    fn an_unusable_bound_discards_the_whole_document() {
        for value in ["2.1", "v2.1.0", "2.1.0-rc.1", "2.1.0+build", "latest", ""] {
            assert!(parse(&stored(None, None, Some(value))).is_none(), "{value}");
            assert!(parse(&stored(None, Some(value), None)).is_none(), "{value}");
        }
    }

    #[test]
    fn a_range_bounds_both_ends() {
        let channel =
            parse(&stored(None, Some("1.5.0"), Some("2.1.0"))).expect("a usable document");
        let range = &channel.supported;
        assert!(!range.contains(&semver::Version::new(1, 4, 9)));
        assert!(range.contains(&semver::Version::new(1, 5, 0)));
        assert!(range.contains(&semver::Version::new(2, 1, 0)));
        assert!(!range.contains(&semver::Version::new(2, 1, 1)));
    }

    /// An absent range is not an empty one. This is what a client sees before any server has
    /// answered, and treating it as "nothing is supported" would stop every update instead of
    /// leaving the client where it was.
    #[test]
    fn no_range_admits_every_version() {
        let range = Range::default();
        assert!(range.contains(&semver::Version::new(0, 0, 1)));
        assert!(range.contains(&semver::Version::new(99, 0, 0)));
        assert_eq!(range.ceiling(), None);
    }
}
