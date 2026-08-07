//! What this bundle is, for the footer's meta line.

/// The crate version.
///
/// Stamped into `Cargo.toml` by the release workflow immediately before it builds, and never
/// committed — release-please's `extra-files` cannot bump this manifest without also bumping
/// `web/frontend/Cargo.lock`, and CI's `supply-chain` job checks exactly that pair with
/// `cargo metadata --locked`. So a build from a working copy reads the placeholder below, and
/// only a released artefact carries the tag.
pub(crate) const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The short commit this bundle was built from, when the build supplied one.
///
/// `option_env!`, not a `build.rs`: the value has to come from the build *environment* (CI, or
/// the Docker build arg) because a `build.rs` running `git` would bake the developer's working
/// tree into a release image built from a clean checkout, and would produce nothing useful in
/// the release build anyway — where there is no `.git` to ask.
///
/// `None` simply omits the segment. A footer that says `unknown` next to a version number reads
/// as a fault; a footer that says nothing reads as a build that did not record it.
pub(crate) fn commit() -> Option<&'static str> {
    option_env!("TANKOVAULT_COMMIT")
        .map(str::trim)
        .filter(|value| !value.is_empty())
}
