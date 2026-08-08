//! What this bundle is: its version, and where it comes from.

/// The project's own page.
///
/// Both builds use it — the footer's source link and the desktop sheet's About tab — so it is
/// one constant rather than a literal per call site.
///
/// It deliberately does **not** feed `update::discover`, which names the same repository in its
/// own constant. That one decides where an executable this app will *run* comes from, and the
/// two must be changeable independently: a fork that repoints its links has not thereby earned
/// the right to ship the update channel's binaries.
pub(crate) const PROJECT_URL: &str = "https://github.com/TimSchoenle/TankoVault";

/// Where a reader downloads the native client. The `latest` alias rather than a version, because
/// nothing on the web side knows which release is current — and must not ask, since the served
/// Content-Security-Policy does not reach github.com and widening it for a download link would
/// be the wrong trade entirely.
pub(crate) const RELEASES_URL: &str = "https://github.com/TimSchoenle/TankoVault/releases/latest";

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
