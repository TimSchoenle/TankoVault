//! Keeping the desktop client current from the GitHub releases this repository publishes.
//!
//! Desktop only, and deliberately **not** under [`crate::platform`]: that module is a contract with
//! two implementations that views are held to equally, and this has one implementation and no
//! browser counterpart — a web SPA is updated by reloading it. Only [`install`] reaches for
//! `cfg(windows)`/`cfg(unix)`, and only for the hand-off itself.
//!
//! Three things are settings of *this installation*, so they live in `settings.json` beside the
//! server address rather than on the account: the [`Policy`], the hold-back window, and what has
//! been staged or dismissed. A reader with two machines may reasonably want one of them current
//! and the other pinned.
//!
//! ## What actually runs, and when
//!
//! [`run`] waits out first paint, then checks at most every six hours. A check asks the connected
//! server which repository to read and how far it may go ([`channel`]), asks GitHub for that
//! repository's recent releases, picks the newest one that is newer than this build, older than
//! the hold-back window **and** within the server's supported range ([`discover::eligible`]), and
//! then — depending on the policy — announces it or downloads it. Nothing is executed at that point: [`install::apply_staged`] runs the
//! installer from `main` at the *next* start, which is why an unattended update never interrupts
//! a reading session with an elevation prompt.
//!
//! That start is also where the reader finally sees any of it: [`adopt_applied`] notices that the
//! version has changed and opens the "what's new" panel over the app with the release notes of
//! the version now running (`components::whats_new`). It is keyed to the version rather than to
//! the hand-off, so an installer the reader ran themselves opens it too.
//!
//! ## What is trusted
//!
//! Nothing that did not arrive inside a manifest signed by a key compiled into this binary. That
//! is the whole of [`discover`]'s doc comment and the reason this module can exist at all: the
//! alternative is downloading an executable over TLS and running it, and TLS says who *served* the
//! bytes, not who made them.
//!
//! Two consequences worth stating because they read as omissions:
//!
//! * **The HTTP client that talks to GitHub is its own**, built below rather than taken from
//!   [`crate::api`]. That one carries the process-wide cookie jar holding the refresh credential
//!   ([`crate::api::session_store`]); using it would send the reader's session cookie to
//!   github.com on every check. This client has no jar, no `Authorization` header and no
//!   credential of any kind — the release list and the assets are public. [`channel`] is the one
//!   part of the updater that *does* use [`crate::api`], and correctly: it asks the reader's own
//!   deployment about itself.
//! * **A check contacts `api.github.com`**, which is the only request this app makes to anywhere
//!   but the reader's own server. The settings sheet says so, and `Policy::Off` stops it.
//!
//! Every error in this module and its two children is a **catalogue key**, not a sentence — the
//! same contract [`crate::platform::save_text_file`] has, so the settings sheet resolves them
//! through the reader's own translator. They all live under `settings.update.error.`, which the
//! `# Errors` sections below abbreviate to `…`.

mod channel;
mod discover;
mod install;

use crate::i18n::Translator;
use dioxus::prelude::*;
use std::time::Duration;

pub(crate) use discover::ReleaseNotes;
pub(crate) use install::{apply_staged, flavour, run_as_relauncher};

/// Which release policy the reader chose.
const POLICY_KEY: &str = "tv-update-policy";
/// How old a release has to be before it is offered, in days.
const MIN_AGE_KEY: &str = "tv-update-min-age-days";
/// When the last check ran, as epoch milliseconds.
const LAST_CHECK_KEY: &str = "tv-update-last-check";
/// A version the reader declined, so `prompt` stops asking about it.
const DISMISSED_KEY: &str = "tv-update-dismissed";
/// The version currently staged on disk. Read by [`install::apply_staged`] at startup.
const STAGED_KEY: &str = "tv-update-staged";
/// The version an installer was handed off for, written immediately before the hand-off and
/// read once by the run that follows it. See [`adopt_applied`].
const APPLIED_KEY: &str = "tv-update-applied";
/// The version the last start was running.
///
/// Compared against this build rather than against [`APPLIED_KEY`], so an installer the reader
/// double-clicked themselves is noticed as well as one this app handed off to. See
/// [`version_change`].
const SEEN_VERSION_KEY: &str = "tv-update-seen-version";

/// The default hold-back window: a release is not offered until it is this many days old.
///
/// Not zero. A release with a fault bad enough to be pulled is usually pulled within a day or two,
/// and an updater's job is to keep readers current, not to put them at the front of the queue.
const DEFAULT_MIN_AGE_DAYS: u32 = 3;
/// Ceiling on the window, and the top of the settings sheet's slider. A month behind is already
/// the extreme end of "let someone else find the faults"; beyond it the control would be a way to
/// switch updates off while the policy still claimed otherwise.
pub(crate) const MAX_MIN_AGE_DAYS: u32 = 30;

/// How long after mount the first check waits. Long enough that it never competes with first paint
/// or the boot-time silent refresh.
const STARTUP_DELAY_MS: u32 = 20_000;
/// Between checks, and the floor on how often one may run — a reader who restarts the app ten
/// times in an evening causes one check, not ten.
const CHECK_INTERVAL_MS: u32 = 6 * 60 * 60 * 1_000;
/// How soon a *failed* check is tried again.
///
/// A refused request is usually a machine that woke up before its network did, and waiting out
/// the full interval for that turns a thirty-second outage into six hours of not looking.
const RETRY_INTERVAL_MS: u32 = 30 * 60 * 1_000;
/// How many consecutive failures are retried at [`RETRY_INTERVAL_MS`] before the loop returns to
/// its ordinary cadence. A laptop that is simply offline must not ask twelve times a day for as
/// long as it stays that way.
const MAX_QUICK_RETRIES: u32 = 4;

/// Connect timeout, and the ceiling on the two small metadata requests. The installer download
/// deliberately has neither: it is on the order of a hundred megabytes.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

/// The version a build the release workflow did not stamp carries.
///
/// `web/frontend/Cargo.toml` is bumped in the workflow's working copy and never committed (see
/// [`crate::build_info::VERSION`]), so this is what a local `dx bundle` reports. The updater
/// refuses to run at this version: every published release is newer, so it would otherwise
/// replace a developer's own build with one from the internet.
const DEVELOPMENT_VERSION: &str = "0.1.0";

/// What the reader asked to happen when a new release appears.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Policy {
    /// Download and verify quietly, install at the next start.
    Auto,
    /// Say a release is available and wait to be told.
    Prompt,
    /// Never contact GitHub.
    Off,
}

impl Policy {
    /// The stored token. Stable — it is written to the settings file.
    pub(crate) fn token(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Prompt => "prompt",
            Self::Off => "off",
        }
    }

    pub(crate) fn from_token(token: &str) -> Option<Self> {
        match token {
            "auto" => Some(Self::Auto),
            "prompt" => Some(Self::Prompt),
            "off" => Some(Self::Off),
            _ => None,
        }
    }

    /// The catalogue key of this option's label.
    pub(crate) fn label_key(self) -> &'static str {
        match self {
            Self::Auto => "settings.update.policyOption.auto",
            Self::Prompt => "settings.update.policyOption.prompt",
            Self::Off => "settings.update.policyOption.off",
        }
    }

    /// Every option, in the order the settings sheet offers them.
    pub(crate) fn all() -> [Self; 3] {
        [Self::Auto, Self::Prompt, Self::Off]
    }
}

/// The reader's policy. Unrecognised or absent reads as [`Policy::Prompt`] — the shipped default,
/// which asks before it does anything.
pub(crate) fn policy() -> Policy {
    crate::platform::store_get(POLICY_KEY)
        .and_then(|token| Policy::from_token(&token))
        .unwrap_or(Policy::Prompt)
}

pub(crate) fn set_policy(policy: Policy) {
    crate::platform::store_set(POLICY_KEY, policy.token());
}

/// The hold-back window in days. A value that will not parse falls back to the default rather than
/// to zero: a corrupt settings file must not turn "hold releases back" into "take them at once".
pub(crate) fn min_age_days() -> u32 {
    parse_min_age(crate::platform::store_get(MIN_AGE_KEY).as_deref())
}

/// The stored hold-back value as a usable number of days.
///
/// Split from the read so the fallback is testable without a settings file. Absent, blank or
/// unparseable all give the default rather than zero — a value that will not parse must not turn
/// "hold releases back" into "take them the moment they appear".
fn parse_min_age(stored: Option<&str>) -> u32 {
    stored
        .and_then(|value| value.trim().parse::<u32>().ok())
        .map_or(DEFAULT_MIN_AGE_DAYS, |days| days.min(MAX_MIN_AGE_DAYS))
}

pub(crate) fn set_min_age_days(days: u32) {
    crate::platform::store_set(MIN_AGE_KEY, &days.min(MAX_MIN_AGE_DAYS).to_string());
}

/// A slider position as a whole number of days, clamped to the range.
///
/// `position as u32` would do it in one token and is exactly what clippy refuses: `as` on a float
/// truncates towards zero, saturates at the bounds and turns a negative into a very large number,
/// all silently. The slider is integer-stepped, so taking the largest day count the position has
/// reached is the same answer with none of that.
///
/// Only `NaN` and a position at or below zero mean no delay. An out-of-range position clamps to
/// the top of the range — `!is_finite()` here made an infinity mean "take every release at once",
/// which inverts the control rather than saturating it.
pub(crate) fn days_from_slider(position: f64) -> u32 {
    if position.is_nan() || position <= 0.0 {
        return 0;
    }
    (0..=MAX_MIN_AGE_DAYS)
        .rev()
        .find(|days| f64::from(*days) <= position)
        .unwrap_or(0)
}

/// Whether this build carries a signing key to verify a release against. `false` switches the
/// whole feature off, including the network call — see `discover`'s `TRUSTED_KEYS`.
pub(crate) fn is_configured() -> bool {
    discover::is_configured()
}

/// This build's version, or `None` for a build the release workflow did not stamp.
pub(crate) fn running_version() -> Option<semver::Version> {
    if crate::build_info::VERSION == DEVELOPMENT_VERSION {
        return None;
    }
    semver::Version::parse(crate::build_info::VERSION).ok()
}

/// What the updater is currently doing, or last did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Status {
    /// No check has run yet in this session.
    Idle,
    Checking,
    UpToDate,
    /// A newer release exists. `installable` is false when this app must not apply it — a release
    /// with no signed manifest, or a copy owned by a package manager.
    Available {
        version: String,
        page: String,
        installable: bool,
    },
    Downloading {
        percent: u8,
    },
    /// Verified and on disk; it installs at the next start.
    Staged {
        version: String,
    },
    /// A newer release exists that the connected server does not support.
    ///
    /// Not a failure and not `UpToDate`: the client is as current as this deployment allows, and
    /// what has to move is the server. `supported` names the ceiling it published.
    Unsupported {
        version: String,
        supported: String,
    },
    /// The installer ran and this is the build it produced.
    ///
    /// Set once, by [`adopt_applied`], at the start that follows the hand-off. It is the only
    /// confirmation the reader ever gets that an unattended update did what it said: everything
    /// else about that path happens with no window on screen.
    Applied {
        version: String,
    },
    /// A catalogue key naming what refused.
    Failed(&'static str),
}

/// What the reader can be shown about a release, and whether it is still on its way.
///
/// Separate from [`Status`] because the two answer different questions: the status is what the
/// updater is *doing*, and this is what the release it found *is*. They also move at different
/// times — a check that finds nothing clears the notes without the panel changing what it shows.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) enum Notes {
    /// Nothing to show — not asked for, or the release list could not be reached.
    #[default]
    None,
    Fetching,
    Ready(ReleaseNotes),
}

/// The updater's state, provided once at the app root and read by the settings sheet, the title
/// bar and the "what's new" panel.
#[derive(Clone, Copy, PartialEq)]
pub(crate) struct UpdateState {
    status: Signal<Status>,
    notes: Signal<Notes>,
    /// Whether the "what's new" panel is on screen.
    panel: Signal<bool>,
}

impl UpdateState {
    pub(crate) fn new() -> Self {
        Self {
            status: Signal::new(Status::Idle),
            notes: Signal::new(Notes::None),
            panel: Signal::new(false),
        }
    }

    pub(crate) fn status(self) -> Status {
        self.status.read().clone()
    }

    fn set(mut self, status: Status) {
        self.status.set(status);
    }

    /// What the release currently in view says about itself.
    pub(crate) fn notes(self) -> Notes {
        self.notes.read().clone()
    }

    fn set_notes(mut self, notes: Notes) {
        self.notes.set(notes);
    }

    pub(crate) fn panel_open(self) -> bool {
        *self.panel.read()
    }

    /// Put the "what's new" panel on screen. Called at a start that changed version, and by the
    /// settings sheet for a release on offer. Closing goes through [`close_panel`], which has a
    /// receipt to retire as well.
    pub(crate) fn open_panel(mut self) {
        self.panel.set(true);
    }

    fn set_panel(mut self, open: bool) {
        self.panel.set(open);
    }

    /// Whether the title bar should draw its dot: there is something the reader has not seen and
    /// has not already declined.
    pub(crate) fn wants_attention(self) -> bool {
        match self.status() {
            Status::Available { ref version, .. } => !is_dismissed(version),
            // The one that is not a request: it is the receipt for an install the reader never
            // saw, and the sheet is where it says which version arrived.
            Status::Staged { .. } | Status::Applied { .. } => true,
            Status::Idle
            | Status::Checking
            | Status::UpToDate
            | Status::Downloading { .. }
            // Nothing the reader can do about it from here, so it does not ask for their
            // attention; the settings sheet says so when they next open it.
            | Status::Unsupported { .. }
            | Status::Failed(_) => false,
        }
    }
}

/// Report an update this installation has taken, at the first start that is running it.
///
/// Two things happen here, and they are deliberately independent.
///
/// **The receipt.** The hand-off in [`install::apply_staged`] records the version and then
/// replaces this process with an installer, so the run that comes back is a *different build*
/// with no memory of any of it. Without [`APPLIED_KEY`] the whole automatic path is invisible:
/// the reader starts the app, it disappears, something reopens a minute later, and nothing ever
/// says why. It is read once and cleared, so a receipt cannot be claimed by a later start.
///
/// **The version change.** [`SEEN_VERSION_KEY`] is compared against this build, which catches an
/// installer the reader ran themselves as well as one this app handed off to — it is the same
/// event either way, and only one of the two leaves a receipt. That is what opens the "what's
/// new" panel. The notification stays tied to the receipt: a reader who has just double-clicked
/// an installer does not need to be told that an installer ran.
pub(crate) fn adopt_applied(state: UpdateState, i18n: Translator) {
    let handed_off = crate::platform::store_get(APPLIED_KEY);
    if handed_off.is_some() {
        crate::platform::store_remove(APPLIED_KEY);
    }
    // A build the release workflow did not stamp has no version to compare. Recording the
    // placeholder would make the next real build look like an update from 0.1.0, and every
    // developer start after it look like a downgrade.
    let Some(current) = running_version() else {
        return;
    };
    let version = current.to_string();
    // Anything but the version this build reports means the installer did not produce what it
    // said it would — a failed install, or a downgrade someone arranged by hand. Saying "updated
    // to 2.4.0" while running 2.3.0 is worse than saying nothing.
    let confirmed = handed_off.as_deref() == Some(version.as_str());
    if confirmed {
        crate::platform::notify(
            &i18n.t("settings.update.notify.appliedTitle"),
            &i18n.args("settings.update.notify.applied", &[("version", &version)]),
        );
    }
    // Its own statement, not an operand: the version has to be recorded whatever is decided
    // here, or the *next* start reads the one before this and greets the reader twice.
    let changed = note_running_version(&current).is_some();
    // Either is an update. A confirmed receipt with nothing recorded is the reader who updated
    // *into* the first build that keeps this record, and telling them by notification while the
    // app itself says nothing is the gap all of this exists to close.
    if !confirmed && !changed {
        return;
    }
    state.set(Status::Applied { version });
    state.open_panel();
    state.set_notes(Notes::Fetching);
    spawn(async move {
        let notes = running_notes(&current).await;
        state.set_notes(notes.map_or(Notes::None, Notes::Ready));
    });
}

/// Record `current` as the version this installation is on, and report the one it replaced.
fn note_running_version(current: &semver::Version) -> Option<semver::Version> {
    let previous = version_change(
        crate::platform::store_get(SEEN_VERSION_KEY).as_deref(),
        current,
    );
    crate::platform::store_set(SEEN_VERSION_KEY, &current.to_string());
    previous
}

/// The version this start replaced, given what the last start recorded.
///
/// `None` unless the stored value parses **and** is genuinely older. A first run has recorded
/// nothing and has replaced nothing, so treating it as an update would meet every reader at
/// their first launch with the changelog of a version they have never not been on; and a
/// downgrade — a reader reinstalling an older build on purpose — is not an update to announce.
fn version_change(stored: Option<&str>, current: &semver::Version) -> Option<semver::Version> {
    let previous = semver::Version::parse(stored?.trim()).ok()?;
    (previous < *current).then_some(previous)
}

/// What the release this build came from says about itself.
///
/// [`Policy::Off`] is honoured here as everywhere else: a reader who chose it asked for no
/// request to github.com, and the panel still names the version it is about — it simply has no
/// notes under the heading.
async fn running_notes(version: &semver::Version) -> Option<ReleaseNotes> {
    if policy() == Policy::Off {
        return None;
    }
    let client = client().ok()?;
    let releases = discover::releases(&client, &channel::current().repo)
        .await
        .ok()?;
    discover::notes_for(&releases, version)
}

/// Close the "what's new" panel, and retire the receipt it was showing.
///
/// The panel is the one surface an unattended update is certain to have been seen through, so
/// dismissing it is what clears [`Status::Applied`] and with it the title bar's dot.
pub(crate) fn close_panel(state: UpdateState) {
    state.set_panel(false);
    acknowledge_applied(state);
}

/// Whether the reader has declined `version`.
///
/// Compared by version rather than remembered as a flag, so declining 2.1.0 says nothing about
/// 2.2.0 — a dismissal is about one release, not about updates.
fn is_dismissed(version: &str) -> bool {
    crate::platform::store_get(DISMISSED_KEY).is_some_and(|declined| declined == version)
}

/// Retire the "updated to …" receipt once the reader has had the settings sheet open.
///
/// [`Status::Applied`] draws the title bar's dot, and a dot that never clears stops meaning
/// anything. Closing the sheet is the one moment it is safe to assume the line was there to be
/// read — clearing it when the sheet *opens* would race the reader to it.
pub(crate) fn acknowledge_applied(state: UpdateState) {
    if matches!(state.status(), Status::Applied { .. }) {
        state.set(Status::UpToDate);
    }
}

/// Stop asking about the release currently on offer.
pub(crate) fn dismiss(state: UpdateState) {
    if let Status::Available { version, .. } = state.status() {
        crate::platform::store_set(DISMISSED_KEY, &version);
    }
}

/// The periodic check. Runs for the life of the app; the caller's `use_future` drops it.
pub(crate) async fn run(state: UpdateState, api: crate::api::Api, i18n: Translator) {
    // Nothing below can produce a sensible answer for a build with no release version or no key to
    // verify one against, and both are permanent for the life of the process.
    if running_version().is_none() || !is_configured() {
        return;
    }
    crate::platform::sleep_ms(STARTUP_DELAY_MS).await;
    let mut failures = 0_u32;
    loop {
        if policy() != Policy::Off && due() {
            check(state, api, i18n).await;
            failures = if matches!(state.status(), Status::Failed(_)) {
                failures.saturating_add(1)
            } else {
                0
            };
        }
        crate::platform::sleep_ms(wait_after(failures)).await;
    }
}

/// How long to wait before looking again, given how many checks in a row have failed.
///
/// Split from the loop so the escalation is testable without a clock. A failed check does not
/// write [`LAST_CHECK_KEY`], so [`due`] is still true when the shorter wait elapses and the retry
/// actually runs — the two have to agree for this to be more than a shorter sleep.
fn wait_after(failures: u32) -> u32 {
    if (1..=MAX_QUICK_RETRIES).contains(&failures) {
        RETRY_INTERVAL_MS
    } else {
        CHECK_INTERVAL_MS
    }
}

/// When the last check completed, as epoch milliseconds, or `None` before the first one.
///
/// Survives a restart, which is the point: [`Status::Idle`] is only ever about *this session*,
/// and a sheet that says "no check has run yet" about an installation that checked an hour ago
/// is indistinguishable from an updater that has quietly stopped working.
pub(crate) fn last_check_ms() -> Option<f64> {
    crate::platform::store_get(LAST_CHECK_KEY)
        .and_then(|value| value.parse::<f64>().ok())
        .filter(|ms| ms.is_finite() && *ms > 0.0)
}

/// Whether enough time has passed since the last check.
fn due() -> bool {
    let last = crate::platform::store_get(LAST_CHECK_KEY)
        .and_then(|value| value.parse::<f64>().ok())
        .unwrap_or(0.0);
    let now = crate::platform::now_ms();
    // A stored time in the future — a clock that was wound back — must not lock checks out for
    // ever, so anything that is not "recently" counts as due.
    !(last..last + f64::from(CHECK_INTERVAL_MS)).contains(&now)
}

/// Look for a release, and act on the policy.
///
/// Also the "check now" button's handler, which is why it does not consult [`due`] — that gate
/// belongs to the loop, not to a reader who asked.
pub(crate) async fn check(state: UpdateState, api: crate::api::Api, i18n: Translator) {
    let previous = state.status();
    state.set(Status::Checking);
    match look(state, api, policy() == Policy::Auto).await {
        Ok(status) => {
            announce(&previous, &status, i18n);
            state.set(status);
        }
        Err(key) => state.set(Status::Failed(key)),
    }
}

/// Download and stage the release currently on offer, whatever the policy says — the reader
/// pressed the button.
pub(crate) async fn install_now(state: UpdateState, api: crate::api::Api, i18n: Translator) {
    let previous = state.status();
    match look(state, api, true).await {
        Ok(status) => {
            announce(&previous, &status, i18n);
            state.set(status);
        }
        Err(key) => state.set(Status::Failed(key)),
    }
}

/// Resolve the release on offer, staging it when `stage` and the install allow.
///
/// The channel is refreshed first, so the ceiling a check honours is the one the server holds
/// *now* rather than the one it held when the app started — an operator who upgrades their
/// deployment on Tuesday should not have to restart every reader's client to unblock them.
///
/// The discovery is repeated rather than remembered between the check and the install. It is two
/// small requests, and the alternative is holding a manifest and its signature in a signal across
/// an arbitrary gap — during which the reader can change the hold-back window, and the answer
/// should change with it.
///
/// # Errors
/// A catalogue key from [`discover`] or [`install`].
async fn look(
    state: UpdateState,
    api: crate::api::Api,
    download: bool,
) -> Result<Status, &'static str> {
    let current = running_version().ok_or("settings.update.error.unconfigured")?;
    let client = client()?;

    channel::refresh(api).await;
    let channel = channel::current();

    let releases = discover::releases(&client, &channel.repo).await?;
    crate::platform::store_set(LAST_CHECK_KEY, &crate::platform::now_ms().to_string());

    let candidate = match discover::eligible(
        &releases,
        &current,
        min_age_days(),
        crate::platform::now_ms(),
        &channel.supported,
    ) {
        discover::Offer::Ready(candidate) => {
            offer_notes(state, Notes::Ready(candidate.notes()));
            candidate
        }
        discover::Offer::None => {
            offer_notes(state, Notes::None);
            return Ok(Status::UpToDate);
        }
        discover::Offer::Unsupported(version) => {
            offer_notes(state, Notes::None);
            return Ok(Status::Unsupported {
                version: version.to_string(),
                // A refusal can only come from the ceiling, so there is one to name.
                supported: channel.supported.ceiling().unwrap_or_default(),
            });
        }
    };

    let version = candidate.version.to_string();
    let flavour = install::flavour();
    // A release with no signed manifest, or an install whose files belong to something else, is
    // announced and never touched. Downloading a hundred megabytes that could not be applied is
    // the failure this branch exists to avoid.
    let installable = candidate.is_installable() && flavour.can_install();
    if !installable || !download {
        return Ok(Status::Available {
            version,
            page: candidate.page,
            installable,
        });
    }

    let (manifest, bytes, signature) = discover::manifest(&client, &candidate).await?;
    let kind = flavour.kind().ok_or("settings.update.error.unmanaged")?;
    let target = manifest
        .targets
        .get(&discover::target_key(kind))
        .ok_or("settings.update.error.noTarget")?;
    let url = candidate
        .asset_url(&target.file)
        .ok_or("settings.update.error.manifest")?;

    state.set(Status::Downloading { percent: 0 });
    install::stage(&client, url, target, &bytes, &signature, |percent| {
        state.set(Status::Downloading { percent });
    })
    .await?;
    // Written last, so a staged directory is only ever announced to the next start once every
    // check in `install::stage` has passed.
    crate::platform::store_set(STAGED_KEY, &version);
    Ok(Status::Staged { version })
}

/// Hand the panel what this check found — unless it is already on screen.
///
/// The first check runs twenty seconds after a start, which is exactly when a reader is still
/// reading the notes of the release they have just been updated to. Overwriting them there would
/// swap the panel's contents for another release's under the reader's eyes.
fn offer_notes(state: UpdateState, notes: Notes) {
    if !state.panel_open() {
        state.set_notes(notes);
    }
}

/// Raise an OS notification when the outcome is new.
///
/// Compared against the previous status so a six-hourly check does not re-announce the same
/// release, and a version the reader declined is not announced at all.
///
/// Deliberately **not** gated on [`crate::platform::notifications_enabled`]: that switch is about
/// chapter releases, which arrive several times a day, and a reader who turned it off did not ask
/// to stop hearing that their client is out of date.
fn announce(previous: &Status, next: &Status, i18n: Translator) {
    if previous == next {
        return;
    }
    let message = match next {
        Status::Available { version, .. } if !is_dismissed(version) => Some((
            i18n.t("settings.update.notify.availableTitle"),
            i18n.args("settings.update.notify.available", &[("version", version)]),
        )),
        Status::Staged { version } => Some((
            i18n.t("settings.update.notify.stagedTitle"),
            i18n.args("settings.update.notify.staged", &[("version", version)]),
        )),
        Status::Available { .. }
        | Status::Idle
        | Status::Checking
        | Status::UpToDate
        | Status::Unsupported { .. }
        | Status::Downloading { .. }
        // Announced by `adopt_applied`, which is the only thing that can set it.
        | Status::Applied { .. }
        | Status::Failed(_) => None,
    };
    if let Some((summary, body)) = message {
        crate::platform::notify(&summary, &body);
    }
}

/// The updater's own HTTP client.
///
/// **Never [`crate::api`]'s.** That one attaches the process-wide cookie jar that holds the
/// refresh credential, and the access token as a bearer header — so reusing it would send the
/// reader's session to github.com on every check. This one carries no credential at all, which is
/// correct rather than merely sufficient: everything it fetches is public.
///
/// # Errors
/// `settings.update.error.network` if the client cannot be built.
fn client() -> Result<reqwest::Client, &'static str> {
    reqwest::Client::builder()
        // GitHub refuses a request without one, and naming the version is what makes a misbehaving
        // release identifiable from the other end.
        .user_agent(format!("TankoVault-desktop/{}", crate::build_info::VERSION))
        .connect_timeout(CONNECT_TIMEOUT)
        .build()
        .map_err(|_| "settings.update.error.network")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_policy_token_survives_a_round_trip() {
        for policy in Policy::all() {
            assert_eq!(Policy::from_token(policy.token()), Some(policy));
        }
        assert_eq!(Policy::from_token("sometimes"), None);
    }

    /// A build the release workflow did not stamp must never update itself: every published
    /// release is newer than the placeholder version, so a local `dx bundle` would download one
    /// and replace the developer's own build with it.
    #[test]
    fn the_placeholder_version_is_not_a_release() {
        assert_eq!(
            crate::build_info::VERSION == DEVELOPMENT_VERSION,
            running_version().is_none(),
            "a build at the placeholder version has no release version, and only that one does not"
        );
    }

    /// A corrupt or missing value falls back to the default window, never to zero.
    ///
    /// Zero is not a neutral fallback here — it means "take every release the moment it appears",
    /// which is the one behaviour the hold-back exists to prevent. A settings file that will not
    /// parse must not silently opt the reader into it.
    #[test]
    fn the_hold_back_window_never_falls_back_to_zero() {
        for stored in [
            None,
            Some(""),
            Some("   "),
            Some("soon"),
            Some("-1"),
            Some("1e3"),
        ] {
            assert_eq!(parse_min_age(stored), DEFAULT_MIN_AGE_DAYS, "{stored:?}");
            assert_ne!(parse_min_age(stored), 0, "{stored:?}");
        }
        assert_eq!(parse_min_age(Some("7")), 7);
        assert_eq!(parse_min_age(Some(" 7 ")), 7);
        // Clamped rather than honoured, so a hand-edited file cannot exceed what the slider offers.
        assert_eq!(parse_min_age(Some("4000")), MAX_MIN_AGE_DAYS);
    }

    /// A first run has replaced nothing, and a downgrade is not an update.
    ///
    /// The first of those is the one that matters: with no stored version, treating the absence
    /// as "you were on something older" would open the "what's new" panel at every reader's
    /// first launch, describing a version they have never not been on. The second keeps a reader
    /// who has deliberately reinstalled an older build from being congratulated for it.
    #[test]
    fn only_a_move_to_a_newer_version_counts_as_an_update() {
        let current = semver::Version::new(2, 1, 0);
        assert_eq!(
            version_change(Some("2.0.5"), &current),
            Some(semver::Version::new(2, 0, 5))
        );
        assert_eq!(
            version_change(Some(" 2.0.5 "), &current),
            Some(semver::Version::new(2, 0, 5))
        );
        for stored in [
            None,
            Some(""),
            Some("nightly"),
            Some("2.1.0"),
            Some("2.2.0"),
        ] {
            assert_eq!(version_change(stored, &current), None, "{stored:?}");
        }
    }

    /// A run of failures is retried quickly a few times and then left alone.
    ///
    /// Both halves are the point: the short retry is what stops a machine that woke before its
    /// network from not looking again for six hours, and the fall-back is what stops one that is
    /// simply offline from asking twelve times a day for as long as it stays that way.
    #[test]
    fn a_failed_check_is_retried_sooner_but_not_for_ever() {
        assert_eq!(wait_after(0), CHECK_INTERVAL_MS);
        for failures in 1..=MAX_QUICK_RETRIES {
            assert_eq!(wait_after(failures), RETRY_INTERVAL_MS, "{failures}");
        }
        assert_eq!(wait_after(MAX_QUICK_RETRIES + 1), CHECK_INTERVAL_MS);
        assert_eq!(wait_after(u32::MAX), CHECK_INTERVAL_MS);
    }

    #[test]
    fn a_slider_position_lands_on_a_whole_clamped_day_count() {
        assert_eq!(days_from_slider(0.0), 0);
        assert_eq!(days_from_slider(7.0), 7);
        assert_eq!(days_from_slider(7.6), 7);
        assert_eq!(days_from_slider(-3.0), 0);
        assert_eq!(days_from_slider(f64::NAN), 0);
        assert_eq!(days_from_slider(f64::INFINITY), MAX_MIN_AGE_DAYS);
        assert_eq!(days_from_slider(1e30), MAX_MIN_AGE_DAYS);
    }
}
