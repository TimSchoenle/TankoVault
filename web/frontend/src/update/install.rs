//! Owns staging a verified installer, and handing it to the system.
//!
//! **How this installation was installed decides whether it can be updated at all**, which is why
//! [`Flavour`] exists instead of a single "download and run" path. A copy under `/usr/bin` came
//! from the `.deb` and is owned by the package manager; a copy run out of an extracted archive is
//! owned by whoever extracted it. Writing to either would be this app deciding it knows better
//! than the tool that put the files there, so both are announced and never touched.
//!
//! **The installer runs at the next start, not at exit.** [`apply_staged`] is called from `main`
//! before there is a window, which means no coupling to the event loop and no elevation prompt
//! appearing in the middle of a reading session. It also re-verifies everything from scratch
//! (the manifest signature, the file's length and its digest) instead of trusting that what the
//! previous run wrote is what it is about to execute. The staging directory is an ordinary
//! directory in the reader's profile, writable by every process running as them, so "we checked
//! this before we wrote it" is not a statement about what is there now.
//!
//! Errors are catalogue keys under `settings.update.error.`, abbreviated to `…` below; see
//! [`super`].

use super::discover::{self, Manifest, Target};
use futures_util::StreamExt as _;
use sha2::Digest as _;
use std::convert::Infallible;
use std::fs;
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The directory staged updates live in, beside `settings.json`.
const STAGING_DIR: &str = "updates";
/// The copy of this binary that sees a Windows update through. See [`launch`].
#[cfg(windows)]
const RELAUNCHER_FILE: &str = "apply.exe";
/// The argument that tells a started copy it is the relauncher and not the app.
///
/// Two dashes and a name nobody would type: this is checked before anything else in `main`, so
/// it must not collide with a flag the reader could plausibly pass.
const RELAUNCH_FLAG: &str = "--apply-staged-update";
/// The manifest and its signature, stored beside the installer so a later start can re-verify.
const MANIFEST_FILE: &str = "desktop-manifest.json";
const SIGNATURE_FILE: &str = "desktop-manifest.json.minisig";
/// Suffix a download carries until its digest has been checked.
const PARTIAL_SUFFIX: &str = ".part";

/// How this copy of the app was installed, and therefore what an update means for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Flavour {
    /// Windows, installed by the `.msi`. Updated by a major upgrade over the same product.
    Msi,
    /// Windows, installed by the NSIS `.exe`. Updated by running the new one silently.
    Nsis,
    /// Linux, running as an `AppImage`. Updated by replacing the file and re-executing it.
    AppImage,
    /// Installed by a package manager (the `.deb`). Its files are not ours to replace.
    Package,
    /// Runs from an extracted archive. There is no install to upgrade.
    Portable,
}

impl Flavour {
    /// The manifest target kind for this flavour, or `None` when there is nothing to install.
    pub(crate) fn kind(self) -> Option<&'static str> {
        match self {
            Self::Msi => Some("msi"),
            Self::Nsis => Some("nsis"),
            Self::AppImage => Some("appimage"),
            Self::Package | Self::Portable => None,
        }
    }

    /// Whether this app can install an update over itself.
    pub(crate) fn can_install(self) -> bool {
        self.kind().is_some()
    }

    /// The catalogue key explaining why it cannot, for the flavours where it cannot.
    pub(crate) fn unmanaged_reason(self) -> Option<&'static str> {
        match self {
            Self::Package => Some("settings.update.unmanaged.package"),
            Self::Portable => Some("settings.update.unmanaged.portable"),
            Self::Msi | Self::Nsis | Self::AppImage => None,
        }
    }
}

/// How this copy was installed.
#[cfg(windows)]
pub(crate) fn flavour() -> Flavour {
    let Ok(exe) = std::env::current_exe() else {
        return Flavour::Portable;
    };
    flavour_of(&exe)
}

/// How the copy at `exe` was installed.
///
/// Separate from [`flavour`] for the relauncher, which is a copy of this binary running from the
/// staging directory: asking about *itself* there answers `Portable`, correctly and uselessly.
/// The install it is updating is the one it was given.
#[cfg(windows)]
fn flavour_of(exe: &Path) -> Flavour {
    let dir = |name: &str| std::env::var_os(name).map(PathBuf::from);
    let program_files: Vec<PathBuf> = ["ProgramFiles", "ProgramFiles(x86)"]
        .iter()
        .filter_map(|name| dir(name))
        .collect();
    classify_windows(exe, dir("LOCALAPPDATA").as_deref(), &program_files)
}

/// How this copy was installed.
#[cfg(unix)]
pub(crate) fn flavour() -> Flavour {
    let appimage = std::env::var_os("APPIMAGE").map(PathBuf::from);
    // Asked by opening for append, not inferred from the mode bits: an `AppImage` on a
    // read-only mount, or one owned by root under `/opt`, has to fall back to announcing the
    // release instead of downloading one it could never write.
    let writable = appimage
        .as_deref()
        .is_some_and(|path| fs::OpenOptions::new().append(true).open(path).is_ok());
    let exe = std::env::current_exe().unwrap_or_default();
    classify_unix(appimage.as_deref(), writable, &exe)
}

/// Which Windows installer produced a copy running from `exe`.
///
/// The two installers land in different places and that is the whole signal: the `.msi` installs
/// per machine under `%ProgramFiles%`, and the NSIS installer defaults to the current user, under
/// `%LOCALAPPDATA%`. **Handing the wrong one to an existing install does not upgrade it: it adds
/// a second install** with its own entry in Apps & Features, so anything not recognisably one or
/// the other is left alone.
///
/// It therefore depends on the NSIS install mode, which `Dioxus.toml` states explicitly
/// (`[bundle.windows.nsis] install_mode`) instead of leaving to the bundler's default, so this
/// reads a decision, and changing it there is visibly a change to this rule.
// Compiled on every host on purpose. This rule decides which installer is handed to a Windows
// install, and handing over the wrong one produces a second install instead of an upgrade, so
// it is the per-OS rule that most needs the CI gate, which runs on Linux, to test it.
#[cfg_attr(
    all(not(windows), not(test)),
    expect(dead_code, reason = "compiled everywhere so its tests run in CI")
)]
fn classify_windows(
    exe: &Path,
    local_app_data: Option<&Path>,
    program_files: &[PathBuf],
) -> Flavour {
    if local_app_data.is_some_and(|dir| is_under(exe, dir)) {
        return Flavour::Nsis;
    }
    if program_files.iter().any(|dir| is_under(exe, dir)) {
        return Flavour::Msi;
    }
    Flavour::Portable
}

/// Whether `path` sits inside `dir`, by Windows' rules for what counts as the same path.
///
/// **Not `Path::starts_with`**, for two separate reasons, and each of them is a real
/// misclassification, not tidying:
///
/// * `Path::starts_with` compares *components*, and what a component is depends on the host. A
///   backslash is not a separator off Windows, so the whole of `C:\Program Files\…` is one opaque
///   component there, which made every Windows fixture classify as `Portable` on the Linux runner
///   that gates this file.
/// * Windows paths are case-insensitive, and `Path`'s component comparison is not. An executable
///   reported as `C:\PROGRAM FILES\Tankovault\…` (which is what a short-name or an
///   all-caps environment variable yields) is under `%ProgramFiles%`, and comparing it verbatim
///   says it is not.
///
/// Separators are folded too, because Windows accepts `/` in a path and `current_exe` is not the
/// only thing that produces one.
///
/// A path that is not valid UTF-8 is compared lossily. The worst outcome is `Portable`, which is
/// the answer that touches nothing.
fn is_under(path: &Path, dir: &Path) -> bool {
    let normalise = |value: &Path| {
        value
            .to_string_lossy()
            .to_ascii_lowercase()
            .replace('\\', "/")
            .trim_end_matches('/')
            .to_owned()
    };
    let dir = normalise(dir);
    // An empty directory would otherwise match everything: an unset `%LOCALAPPDATA%` arrives here
    // as `Some("")` if it is ever read without a filter, and "every path is under it" is the one
    // answer that installs the wrong package.
    if dir.is_empty() {
        return false;
    }
    // The trailing separator is what stops `C:/Program Files (x86)` matching under
    // `C:/Program Files`.
    normalise(path).starts_with(&format!("{dir}/"))
}

/// Which Linux packaging produced a copy running from `exe`.
///
/// `APPIMAGE` is set by the `AppImage` runtime to the path of the image itself, which is the one
/// case where the whole application is a single file this app may replace. `/usr` and `/opt` are
/// the `.deb`'s territory.
#[cfg_attr(
    all(not(unix), not(test)),
    expect(dead_code, reason = "see `classify_windows` above")
)]
fn classify_unix(appimage: Option<&Path>, writable: bool, exe: &Path) -> Flavour {
    if appimage.is_some() {
        return if writable {
            Flavour::AppImage
        } else {
            Flavour::Portable
        };
    }
    if exe.starts_with("/usr") || exe.starts_with("/opt") {
        Flavour::Package
    } else {
        Flavour::Portable
    }
}

/// Where staged updates go, or `None` when the platform exposes no config directory: the same
/// answer, and for the same reason, as a settings write that cannot land.
fn staging_dir() -> Option<PathBuf> {
    Some(
        crate::platform::settings_path()?
            .parent()?
            .join(STAGING_DIR),
    )
}

/// Forgets any staged update.
///
/// Called whenever a staged build turns out not to be usable, so a bad download is not retried
/// from disk on every start. Failures are ignored: the next stage clears the directory again.
pub(crate) fn clear() {
    if let Some(dir) = staging_dir() {
        let _ = fs::remove_dir_all(&dir);
    }
    crate::platform::store_remove(super::STAGED_KEY);
}

/// Downloads `target` for `candidate`, checks it against the manifest, and stages it for the next
/// start. `progress` is called with a percentage as the bytes arrive.
///
/// The manifest and its signature are written alongside, because [`apply_staged`] verifies them
/// again instead of trusting this directory to be untouched between runs.
///
/// # Errors
/// `…staging` for anything the filesystem refuses, `…network` for a failed download,
/// `…checksum` when the bytes do not match the manifest.
pub(crate) async fn stage(
    client: &reqwest::Client,
    url: &str,
    target: &Target,
    manifest_bytes: &[u8],
    signature: &str,
    mut progress: impl FnMut(u8),
) -> Result<(), &'static str> {
    let dir = staging_dir().ok_or("settings.update.error.staging")?;
    // A previous attempt's partial download, or a different version entirely, is not something to
    // resume from.
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).map_err(|_| "settings.update.error.staging")?;

    let partial = dir.join(format!("{}{PARTIAL_SUFFIX}", target.file));
    let digest = download_to(client, url, &partial, target.size, &mut progress).await?;

    if digest != target.sha256.to_ascii_lowercase() {
        let _ = fs::remove_dir_all(&dir);
        return Err("settings.update.error.checksum");
    }
    fs::rename(&partial, dir.join(&target.file)).map_err(|_| "settings.update.error.staging")?;
    write(&dir.join(MANIFEST_FILE), manifest_bytes)?;
    write(&dir.join(SIGNATURE_FILE), signature.as_bytes())?;
    Ok(())
}

/// Streams `url` into `path`, returning the lower-case hex SHA-256 of what was written.
///
/// Hashed as it streams instead of by re-reading the file: the installer is on the order of a
/// hundred megabytes, and the digest has to cover the bytes that reached the disk anyway.
async fn download_to(
    client: &reqwest::Client,
    url: &str,
    path: &Path,
    expected_size: u64,
    progress: &mut impl FnMut(u8),
) -> Result<String, &'static str> {
    let response = client
        .get(url)
        .send()
        .await
        .map_err(|_| "settings.update.error.network")?;
    if !response.status().is_success() {
        return Err("settings.update.error.network");
    }

    let mut file = fs::File::create(path).map_err(|_| "settings.update.error.staging")?;
    let mut hasher = sha2::Sha256::new();
    let mut written: u64 = 0;
    let mut reported = u8::MAX;
    let mut stream = response.bytes_stream();

    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| "settings.update.error.network")?;
        // A body longer than the manifest says is refused here instead of after a hundred
        // megabytes of it have been written: the length is signed, so exceeding it is already a
        // reason to stop.
        written = written.saturating_add(chunk.len() as u64);
        if written > expected_size {
            return Err("settings.update.error.checksum");
        }
        hasher.update(&chunk);
        file.write_all(&chunk)
            .map_err(|_| "settings.update.error.staging")?;

        let percent = percentage(written, expected_size);
        if percent != reported {
            reported = percent;
            progress(percent);
        }
    }
    file.sync_all()
        .map_err(|_| "settings.update.error.staging")?;
    if written != expected_size {
        return Err("settings.update.error.checksum");
    }
    Ok(hex::encode(hasher.finalize()))
}

/// `written` as a percentage of `total`, saturating at 100 and answering 0 for an unknown total.
fn percentage(written: u64, total: u64) -> u8 {
    if total == 0 {
        return 0;
    }
    let percent = written.saturating_mul(100) / total;
    u8::try_from(percent.min(100)).unwrap_or(100)
}

/// Writes `bytes` to `path`, flushed to the device.
fn write(path: &Path, bytes: &[u8]) -> Result<(), &'static str> {
    let mut file = fs::File::create(path).map_err(|_| "settings.update.error.staging")?;
    file.write_all(bytes)
        .map_err(|_| "settings.update.error.staging")?;
    file.sync_all().map_err(|_| "settings.update.error.staging")
}

/// A staged update that has passed every check and may be executed.
struct Plan {
    flavour: Flavour,
    installer: PathBuf,
    /// What the signed manifest says this installer produces. Recorded before the hand-off so
    /// the run that follows can confirm it: see [`super::adopt_applied`].
    version: String,
}

/// Applies a staged update, if there is a usable one.
///
/// Called from `main` before the window exists. On success it does not return: the process is
/// replaced by, or exits in favour of, the installer. Anything unusable clears the staging
/// directory and lets the app start normally: a staged build that cannot be applied must not be
/// retried on every launch for ever.
///
/// **This is the whole of what an unattended update looks like from the outside**, which is why
/// it says so out loud. The reader opened the app; the next minute is an installer with no
/// window of ours behind it. A notification is raised before the hand-off and the version is
/// recorded for the run that comes back, so the two ends of that minute are accounted for.
pub(crate) fn apply_staged() {
    if crate::platform::store_get(super::STAGED_KEY).is_none() {
        return;
    }
    let plan = match plan(flavour()) {
        Ok(plan) => plan,
        Err(error) => {
            tracing::warn!(?error, "a staged update is unusable and has been discarded");
            clear();
            return;
        }
    };
    tracing::info!(
        version = plan.version,
        "applying a staged update; handing off to the installer"
    );
    // Written first: after the hand-off there is no process left here to write anything, and an
    // install that fails is caught by the version check at the other end instead of by this one.
    crate::platform::store_set(super::APPLIED_KEY, &plan.version);
    crate::platform::notify_now(
        &crate::i18n::translate_offline("settings.update.notify.applyingTitle", &[]),
        &crate::i18n::translate_offline(
            "settings.update.notify.applying",
            &[("version", &plan.version)],
        ),
    );
    // Only reached if the hand-off itself failed; the success path does not return.
    let Err(error) = launch(&plan);
    tracing::error!(
        version = plan.version,
        error,
        "the installer hand-off failed; starting the installed version instead"
    );
    crate::platform::store_remove(super::APPLIED_KEY);
    clear();
}

/// What a command line asks this binary to be.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Invocation {
    /// The app. Every ordinary start, including the reader's own.
    App,
    /// The relauncher, for the install at this path.
    Relaunch(PathBuf),
    /// The relauncher's flag with no path after it.
    ///
    /// A separate answer from [`Invocation::App`] deliberately: this process is a copy of the
    /// app sitting in the staging directory, and falling through to launch a window would put a
    /// *second* copy on screen; running from a temporary directory, updating nothing, and
    /// looking to the reader exactly like the one they installed.
    RelaunchWithoutTarget,
}

/// Reads the command line. Compiled everywhere so its test runs on the CI runner, for the reason
/// [`classify_windows`] is.
#[cfg_attr(
    all(not(windows), not(test)),
    expect(dead_code, reason = "compiled everywhere so its tests run in CI")
)]
fn invocation<I: Iterator<Item = std::ffi::OsString>>(mut args: I) -> Invocation {
    if args.next().is_none_or(|flag| flag != RELAUNCH_FLAG) {
        return Invocation::App;
    }
    args.next()
        .map_or(Invocation::RelaunchWithoutTarget, |target| {
            Invocation::Relaunch(PathBuf::from(target))
        })
}

/// Whether this process was started to see an update through, not to be the app.
///
/// `true` means it has already done that job and `main` must return without launching anything.
/// See [`launch`] for why a second process exists at all.
#[cfg(windows)]
pub(crate) fn run_as_relauncher() -> bool {
    match invocation(std::env::args_os().skip(1)) {
        Invocation::App => false,
        Invocation::Relaunch(target) => {
            tracing::info!(?target, "running as the update relauncher");
            relaunch_after_install(&target);
            true
        }
        Invocation::RelaunchWithoutTarget => {
            tracing::warn!("started as the relauncher with no target; exiting without a window");
            true
        }
    }
}

/// Waits in bounded intervals before checking `ready`, so the process being replaced gets time to
/// exit before the first probe. The injected sleeper keeps the timing deterministic in tests.
#[cfg_attr(
    all(not(windows), not(test)),
    expect(
        dead_code,
        reason = "compiled everywhere so its timing is tested on the Linux runner"
    )
)]
fn wait_until<T>(
    budget: Duration,
    poll: Duration,
    mut ready: impl FnMut() -> Option<T>,
    mut sleep: impl FnMut(Duration),
) -> Option<T> {
    if poll.is_zero() {
        return None;
    }

    let mut remaining = budget;
    while !remaining.is_zero() {
        let interval = poll.min(remaining);
        sleep(interval);
        remaining -= interval;
        if remaining.is_zero() {
            return None;
        }
        if let Some(value) = ready() {
            return Some(value);
        }
    }
    None
}

/// Builds the process command from the installed executable's directory, not the staging copy's.
#[cfg_attr(
    all(not(windows), not(test)),
    expect(
        dead_code,
        reason = "compiled everywhere so its working directory is tested on Linux"
    )
)]
fn restart_command(target: &Path) -> std::process::Command {
    let mut command = std::process::Command::new(target);
    if let Some(directory) = target
        .parent()
        .filter(|directory| !directory.as_os_str().is_empty())
    {
        command.current_dir(directory);
    }
    command
}

#[cfg_attr(
    all(not(windows), not(test)),
    expect(dead_code, reason = "used by the Windows-only update relauncher")
)]
const INSTALL_WAIT_BUDGET: Duration = Duration::from_secs(30);
const RESTART_WAIT_BUDGET: Duration = Duration::from_secs(30);
/// Long enough for an installer that is genuinely working (an elevation prompt the reader has
/// not answered yet, a slow disk), short enough that one which never answers is not permanent.
/// See [`run_installer`] for what the bound protects.
#[cfg(windows)]
const INSTALLER_WAIT_BUDGET: Duration = Duration::from_mins(10);
const WAIT_POLL_INTERVAL: Duration = Duration::from_millis(100);

/// Blocks this thread; the relauncher's only way to wait.
#[cfg(windows)]
fn pause(duration: Duration) {
    #[expect(
        clippy::disallowed_methods,
        reason = "the Windows relauncher has no event loop to wait on"
    )]
    std::thread::sleep(duration);
}

/// Never the relauncher: the `AppImage` path replaces its own image and `exec`s it, so the
/// process that comes back *is* the new version and there is nothing to wait for.
#[cfg(unix)]
pub(crate) fn run_as_relauncher() -> bool {
    false
}

/// Runs the staged installer, waits for it, and starts the app again.
///
/// Runs as a copy of the app in the staging directory, so nothing it does holds the installed
/// executable open. Everything is re-verified here as well as in the process that spawned this
/// one, because this is the process that actually executes an installer and the directory it
/// runs from is writable by anything running as the reader.
///
/// `target` (the executable to start afterwards) comes from the command line, and is the one
/// thing here that does not. It is not a widening: passing an argument to this binary means
/// being able to start a process as the reader, which is already enough to start any other.
#[cfg(windows)]
fn relaunch_after_install(target: &Path) {
    relaunch_sequence(
        target,
        crate::platform::try_acquire_instance_lock,
        pause,
        || match plan(flavour_of(target)) {
            Ok(plan) => run_installer(&plan),
            Err(error) => tracing::warn!(?error, "staged update could not be installed"),
        },
        clear,
        || restart(target),
    );
}

/// The relauncher's ordering, with every effect injected so the order itself can be tested.
///
/// The order is the fix and each step depends on the one before it: the installer cannot copy
/// over an executable the app still holds open, the staged copy has to be gone before anything
/// starts again so the new process finds nothing to hand off a second time, and the lock has to
/// be released before the restart or the app it starts sees itself as a second instance.
#[cfg_attr(
    all(not(windows), not(test)),
    expect(
        dead_code,
        reason = "compiled everywhere so the ordering is tested on the Linux runner"
    )
)]
fn relaunch_sequence<L>(
    target: &Path,
    acquire: impl FnMut() -> Option<L>,
    sleep: impl FnMut(Duration),
    install: impl FnOnce(),
    clear: impl FnOnce(),
    restart: impl FnOnce(),
) {
    let instance_lock = wait_until(INSTALL_WAIT_BUDGET, WAIT_POLL_INTERVAL, acquire, sleep);
    if instance_lock.is_none() {
        tracing::warn!(
            ?target,
            "timed out waiting for the running app to exit; proceeding with update"
        );
    }
    install();
    // Unconditional, including after an install that failed: `restart` starts the app a few
    // lines below, and an app that still finds a staged copy hands off to a relauncher again.
    // Keeping it to allow a retry would be that loop, with a notification each time round; the
    // update is re-discovered by the next check instead.
    clear();
    drop(instance_lock);
    // Started whether or not the install worked. The app is either the new version or the old
    // one, and both are better than the reader being left with nothing after a double-click.
    restart();
}

/// Runs the staged installer and waits for it, within a budget.
///
/// The budget is the point: this process holds the instance lock while the installer runs, so a
/// plain `wait` on an installer that never exits would hold that lock for the life of the
/// machine's session, and every later launch would exit silently against a lock nobody is ever
/// going to release. Giving up on the wait does not kill the installer; it only stops this
/// process from standing in front of the app for ever.
#[cfg(windows)]
fn run_installer(plan: &Plan) {
    let Some(mut command) = installer_command(plan) else {
        tracing::warn!(flavour = ?plan.flavour, "no installer command for staged update");
        return;
    };
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            tracing::warn!(?error, "could not start update installer");
            return;
        }
    };
    let exited = wait_until(
        INSTALLER_WAIT_BUDGET,
        WAIT_POLL_INTERVAL,
        || child.try_wait().transpose(),
        pause,
    );
    match exited {
        Some(Ok(status)) if status.success() => {
            tracing::info!(?status, "update installer exited successfully");
        }
        Some(Ok(status)) => tracing::warn!(?status, "update installer exited unsuccessfully"),
        Some(Err(error)) => tracing::warn!(?error, "could not wait for update installer"),
        None => tracing::error!(
            budget = ?INSTALLER_WAIT_BUDGET,
            "the update installer has not exited within its budget; leaving it running and \
             releasing the instance lock so the app can be started again"
        ),
    }
}

/// Starts `target` again, retrying within [`RESTART_WAIT_BUDGET`]. `true` if a process started.
#[cfg_attr(
    all(not(windows), not(test)),
    expect(
        dead_code,
        reason = "compiled everywhere so restart retries are tested on Linux"
    )
)]
fn restart_with(
    target: &Path,
    mut spawn: impl FnMut(&Path) -> bool,
    sleep: impl FnMut(Duration),
) -> bool {
    wait_until(
        RESTART_WAIT_BUDGET,
        WAIT_POLL_INTERVAL,
        || spawn(target).then_some(()),
        sleep,
    )
    .is_some()
}

/// Starts the installed app again, telling the reader if it could not be started at all.
#[cfg(windows)]
fn restart(target: &Path) {
    let mut last_error = None;
    let started = restart_with(
        target,
        |path| match restart_command(path).spawn() {
            Ok(_) => true,
            Err(error) => {
                last_error = Some(error);
                false
            }
        },
        pause,
    );
    if !started {
        tracing::error!(?target, ?last_error, "could not restart app after update");
        crate::platform::notify_now(
            &crate::i18n::translate_offline("settings.update.notify.restartFailedTitle", &[]),
            &crate::i18n::translate_offline("settings.update.notify.restartFailed", &[]),
        );
    }
}

/// Re-verifies everything about the staged update and decides what to run.
///
/// `flavour` is passed, not read, because the relauncher's own path says nothing about
/// the install it is updating. See `flavour_of`.
///
/// # Errors
/// A catalogue key naming the check that refused it; the caller's response to all of them is the
/// same, so they are not told apart beyond being recorded.
fn plan(flavour: Flavour) -> Result<Plan, &'static str> {
    let dir = staging_dir().ok_or("settings.update.error.staging")?;
    let manifest_bytes =
        fs::read(dir.join(MANIFEST_FILE)).map_err(|_| "settings.update.error.staging")?;
    let signature = fs::read_to_string(dir.join(SIGNATURE_FILE))
        .map_err(|_| "settings.update.error.staging")?;

    // The signature first, before a single field of the document is read. Everything below
    // (which file to execute, how long it should be, what it should hash to) comes out of these
    // bytes, and this directory is writable by anything running as the reader.
    discover::verify_trusted(&manifest_bytes, &signature)?;
    let manifest: Manifest = discover::parse(&manifest_bytes)?;

    let staged =
        semver::Version::parse(&manifest.version).map_err(|_| "settings.update.error.manifest")?;
    let current = super::running_version().ok_or("settings.update.error.manifest")?;
    // Already applied: the installer ran, this is the new build, and the directory is just
    // leftovers.
    if staged <= current {
        return Err("settings.update.error.stale");
    }

    let kind = flavour.kind().ok_or("settings.update.error.unmanaged")?;
    let target = manifest
        .targets
        .get(&discover::target_key(kind))
        .ok_or("settings.update.error.noTarget")?;

    let installer = dir.join(&target.file);
    verify_file(&installer, target)?;
    Ok(Plan {
        flavour,
        installer,
        version: manifest.version,
    })
}

/// Checks a staged file against the length and digest the signed manifest gives for it.
///
/// # Errors
/// `…checksum` for a wrong length or digest, `…staging` if it cannot be read at all.
fn verify_file(path: &Path, target: &Target) -> Result<(), &'static str> {
    let metadata = fs::metadata(path).map_err(|_| "settings.update.error.staging")?;
    if metadata.len() != target.size {
        return Err("settings.update.error.checksum");
    }
    let mut file = fs::File::open(path).map_err(|_| "settings.update.error.staging")?;
    let mut hasher = sha2::Sha256::new();
    // Read by hand instead of `io::copy`: `digest` 0.11 dropped the `io::Write` impl on hashers.
    // `Interrupted` is ignored for the same reason `io::copy` retries it: a signal arriving
    // mid-read is not a failed verification. Heap-allocated because a buffer this size is over
    // the crate's stack-array ceiling, and it is read once per launch over a ~100 MB installer.
    let mut buffer = vec![0_u8; 64 * 1024];
    loop {
        match file.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => hasher.update(&buffer[..read]),
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => return Err("settings.update.error.staging"),
        }
    }
    if hex::encode(hasher.finalize()) == target.sha256.to_ascii_lowercase() {
        Ok(())
    } else {
        Err("settings.update.error.checksum")
    }
}

/// How each Windows installer is run, or `None` for a flavour [`plan`] would already have
/// refused.
#[cfg(windows)]
fn installer_command(plan: &Plan) -> Option<std::process::Command> {
    match plan.flavour {
        // A major upgrade over the same product, not a second install: the `.msi`'s UpgradeCode is
        // derived from the bundle identifier and is stable across versions. `/qb` shows a bare
        // progress bar, because a silent per-machine install still raises an elevation prompt and
        // a prompt with nothing behind it looks like malware.
        Flavour::Msi => {
            let mut command = std::process::Command::new("msiexec");
            command.arg("/i").arg(&plan.installer).arg("/qb");
            Some(command)
        }
        Flavour::Nsis => {
            let mut command = std::process::Command::new(&plan.installer);
            command.arg("/S");
            Some(command)
        }
        Flavour::AppImage | Flavour::Package | Flavour::Portable => None,
    }
}

/// Hands the staged installer to the system. Does not return on success.
///
/// **Neither installer can replace an executable that is running**, so this process has to be
/// gone before either reaches its file-copy phase, which is exactly why it cannot also be the
/// thing that starts the app again afterwards. A copy of this binary is placed outside the
/// install directory and given that one job ([`relaunch_after_install`]): wait out the
/// installer, then start what it produced.
///
/// Without it an unattended update ends with the app simply not there: the reader double-clicks,
/// the window never appears, and something has silently installed itself. That is what the copy
/// buys, and it is worth a hundred megabytes of temporary disk to buy it.
///
/// A copy that cannot be made or started falls back to the hand-off without one: updating and
/// not reopening is worse than reopening, and better than never updating.
///
/// # Errors
/// `settings.update.error.handoff` when neither the relauncher nor the installer could be
/// started.
#[cfg(windows)]
fn launch(plan: &Plan) -> Result<Infallible, &'static str> {
    let target = std::env::current_exe().map_err(|_| "settings.update.error.handoff")?;
    if let Some(dir) = staging_dir() {
        let relauncher = dir.join(RELAUNCHER_FILE);
        let handed_over = fs::copy(&target, &relauncher).is_ok()
            && std::process::Command::new(&relauncher)
                .arg(RELAUNCH_FLAG)
                .arg(&target)
                .spawn()
                .is_ok();
        if handed_over {
            std::process::exit(0);
        }
    }
    installer_command(plan)
        .ok_or("settings.update.error.handoff")?
        .spawn()
        .map_err(|_| "settings.update.error.handoff")?;
    std::process::exit(0);
}

/// Hands the staged installer to the system. Does not return on success.
///
/// # Errors
/// `settings.update.error.handoff` when the image could not be replaced or re-executed.
#[cfg(unix)]
fn launch(plan: &Plan) -> Result<Infallible, &'static str> {
    use std::os::unix::fs::PermissionsExt as _;
    use std::os::unix::process::CommandExt as _;

    if plan.flavour != Flavour::AppImage {
        return Err("settings.update.error.handoff");
    }
    let target = std::env::var_os("APPIMAGE")
        .map(PathBuf::from)
        .ok_or("settings.update.error.handoff")?;

    // Written beside the target and renamed over it, not copied onto it: a rename is
    // atomic, so an interrupted update leaves the old image intact and runnable instead of a
    // half-written file that is neither version. Replacing a running `AppImage` this way is
    // safe: its filesystem is mounted from a descriptor that was already open, so the old image
    // stays readable until this process ends.
    let name = target.file_name().ok_or("settings.update.error.handoff")?;
    let next = target.with_file_name(format!("{}.new", name.to_string_lossy()));
    fs::copy(&plan.installer, &next).map_err(|_| "settings.update.error.handoff")?;
    fs::set_permissions(&next, fs::Permissions::from_mode(0o755))
        .map_err(|_| "settings.update.error.handoff")?;
    fs::OpenOptions::new()
        .write(true)
        .open(&next)
        .and_then(|file| file.sync_all())
        .map_err(|_| "settings.update.error.handoff")?;
    fs::rename(&next, &target).map_err(|_| "settings.update.error.handoff")?;

    // `exec`, not spawn-and-exit: the new image takes over this process, so there is no
    // window where both versions are running and no orphan if the parent is killed first.
    let error = std::process::Command::new(&target)
        .args(std::env::args_os().skip(1))
        .exec();
    let _ = error;
    Err("settings.update.error.handoff")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn local_app_data() -> PathBuf {
        PathBuf::from(r"C:\Users\reader\AppData\Local")
    }

    fn program_files() -> Vec<PathBuf> {
        vec![
            PathBuf::from(r"C:\Program Files"),
            PathBuf::from(r"C:\Program Files (x86)"),
        ]
    }

    fn windows(exe: &str) -> Flavour {
        classify_windows(Path::new(exe), Some(&local_app_data()), &program_files())
    }

    /// The relauncher used to launch an installer immediately, while the app still owned its
    /// executable. It now gives that process an exit window, probes only within the budget, and
    /// stops as soon as the lock is free.
    #[test]
    fn the_relauncher_waits_for_exit_and_stops_at_its_budget() {
        let mut probes = 0;
        let mut sleeps = Vec::new();
        let result = wait_until(
            Duration::from_millis(25),
            Duration::from_millis(5),
            || {
                probes += 1;
                Some(())
            },
            |duration| sleeps.push(duration),
        );
        assert!(result.is_some());
        assert_eq!(probes, 1, "stop polling as soon as the old app exits");
        assert_eq!(
            sleeps,
            [Duration::from_millis(5)],
            "wait before the first probe"
        );

        let mut timed_out_probes = 0;
        let result = wait_until(
            Duration::from_millis(5),
            Duration::from_millis(5),
            || {
                timed_out_probes += 1;
                Some(())
            },
            |_| {},
        );
        assert!(
            result.is_none(),
            "do not probe after the full budget expires"
        );
        assert_eq!(timed_out_probes, 0);
    }

    /// The budget has to be spent and then stop. A probe that never succeeds is the only case
    /// that distinguishes "waited the whole budget" from "retried for ever": the relauncher runs
    /// with no window, so an unbounded retry here is an app that silently never comes back.
    #[test]
    fn a_probe_that_never_succeeds_spends_the_budget_exactly_once() {
        let mut probes = 0;
        let mut slept = Duration::ZERO;
        let result = wait_until(
            Duration::from_millis(25),
            Duration::from_millis(5),
            || {
                probes += 1;
                assert!(probes <= 5, "retry within the budget, not for ever");
                None::<()>
            },
            |duration| slept += duration,
        );
        assert!(result.is_none());
        assert_eq!(probes, 4, "probe after every wait but the one that ends it");
        assert_eq!(slept, Duration::from_millis(25), "spend the whole budget");
    }

    /// A major upgrade removes and recreates the executable, so the first spawn can fail even
    /// after the installer exits; discarding that one failure left the reader with no window.
    #[test]
    fn restart_retries_until_the_installed_executable_exists() {
        use std::cell::Cell;

        let target = PathBuf::from("installed/tankovault.exe");
        let exists = Cell::new(false);
        let mut sleeps = 0;
        let mut attempts = 0;
        let started = restart_with(
            &target,
            |_| {
                attempts += 1;
                exists.get()
            },
            |_| {
                sleeps += 1;
                if sleeps == 2 {
                    exists.set(true);
                }
            },
        );
        assert!(started);
        assert_eq!(
            attempts, 2,
            "retry the failed spawn after the file is recreated"
        );
    }

    /// The relauncher used to run the installer while the app still owned its executable, and
    /// started the app again without ever having held the lock. The order is the fix: wait for
    /// the lock, install under it, clear the staged copy, release the lock, then restart.
    #[test]
    fn the_relauncher_installs_under_the_lock_and_releases_it_before_restarting() {
        use std::cell::RefCell;
        use std::rc::Rc;

        struct Token(Rc<RefCell<Vec<&'static str>>>);
        impl Drop for Token {
            fn drop(&mut self) {
                self.0.borrow_mut().push("unlock");
            }
        }

        let log = Rc::new(RefCell::new(Vec::new()));
        let mut taken = false;
        relaunch_sequence(
            Path::new("installed/tankovault.exe"),
            || {
                if taken {
                    return None;
                }
                taken = true;
                log.borrow_mut().push("lock");
                Some(Token(Rc::clone(&log)))
            },
            |_| {},
            || log.borrow_mut().push("install"),
            || log.borrow_mut().push("clear"),
            || log.borrow_mut().push("restart"),
        );

        assert_eq!(
            *log.borrow(),
            ["lock", "install", "clear", "unlock", "restart"]
        );
    }

    /// The lock is almost never free on the first probe: the app that handed off to this process
    /// is still exiting. A single-attempt regression would pass the test above and install over
    /// an executable the old app still owns, which is the bug the whole sequence exists for.
    #[test]
    fn the_relauncher_installs_only_once_a_contended_lock_comes_free() {
        use std::cell::RefCell;
        use std::rc::Rc;

        struct Token(Rc<RefCell<Vec<&'static str>>>);
        impl Drop for Token {
            fn drop(&mut self) {
                self.0.borrow_mut().push("unlock");
            }
        }

        let log = Rc::new(RefCell::new(Vec::new()));
        let mut probes = 0;
        relaunch_sequence(
            Path::new("installed/tankovault.exe"),
            || {
                probes += 1;
                if probes < 3 {
                    return None;
                }
                log.borrow_mut().push("lock");
                Some(Token(Rc::clone(&log)))
            },
            |_| {},
            || log.borrow_mut().push("install"),
            || log.borrow_mut().push("clear"),
            || log.borrow_mut().push("restart"),
        );

        assert_eq!(probes, 3, "keep probing until the old app lets go");
        assert_eq!(
            *log.borrow(),
            ["lock", "install", "clear", "unlock", "restart"],
            "install only after the lock is held"
        );
    }

    /// The relauncher runs from the updates directory, but the restarted app must resolve its
    /// settings and relative resources from the directory the installer produced.
    #[test]
    fn restart_runs_from_the_installed_executable_directory() {
        let target = std::env::temp_dir()
            .join("tankovault-installed")
            .join("tankovault.exe");
        let command = restart_command(&target);
        assert_eq!(command.get_current_dir(), target.parent());
    }

    /// The two Windows installers are told apart by where they land, and handing the wrong one to
    /// an existing install produces a **second** install instead of an upgrade, so anything not
    /// recognisably one or the other has to be left alone.
    #[test]
    fn a_windows_install_is_classified_by_its_directory() {
        assert_eq!(
            windows(r"C:\Users\reader\AppData\Local\Tankovault\tankovault.exe"),
            Flavour::Nsis
        );
        assert_eq!(
            windows(r"C:\Program Files\Tankovault\tankovault.exe"),
            Flavour::Msi
        );
        assert_eq!(
            windows(r"C:\Program Files (x86)\Tankovault\tankovault.exe"),
            Flavour::Msi
        );
        assert_eq!(windows(r"D:\unpacked\tankovault.exe"), Flavour::Portable);
    }

    /// The rule holds on a Linux runner, which is the only place it is ever gated.
    ///
    /// The bug: it was written with `Path::starts_with`, whose idea of a component is the *host's*:
    /// off Windows a backslash is not a separator, so every path above was one opaque component and
    /// every case classified as `Portable`. The rule passed on a Windows workstation and failed in
    /// CI, which is exactly the wrong way round for a rule about Windows.
    #[test]
    fn the_windows_rule_does_not_depend_on_the_host_path_semantics() {
        assert!(is_under(
            Path::new(r"C:\Program Files\Tankovault\tankovault.exe"),
            Path::new(r"C:\Program Files"),
        ));
        // Forward slashes are legal in a Windows path, and mixed separators have to fold together.
        assert!(is_under(
            Path::new("C:/Program Files/Tankovault/tankovault.exe"),
            Path::new(r"C:\Program Files"),
        ));
        // A sibling whose name merely starts the same is not inside it.
        assert!(!is_under(
            Path::new(r"C:\Program Files (x86)\Tankovault\tankovault.exe"),
            Path::new(r"C:\Program Files"),
        ));
        // The directory itself is not "under" itself; an executable is always in a subdirectory.
        assert!(!is_under(
            Path::new(r"C:\Program Files"),
            Path::new(r"C:\Program Files"),
        ));
        // An unset environment variable must not match every path in existence.
        assert!(!is_under(Path::new(r"C:\anywhere\at\all"), Path::new("")));
    }

    /// The bug: Windows paths are case-insensitive and `Path`'s comparison is not, so an executable
    /// reported in a different case than the environment variable it sits under (an all-caps
    /// `%ProgramFiles%`, a short name) was classified as portable and never updated.
    #[test]
    fn a_windows_path_matches_regardless_of_case() {
        assert_eq!(
            windows(r"C:\PROGRAM FILES\Tankovault\tankovault.exe"),
            Flavour::Msi
        );
        assert_eq!(
            windows(r"c:\users\reader\appdata\local\Tankovault\tankovault.exe"),
            Flavour::Nsis
        );
    }

    /// A `.deb` install is the package manager's, and this app does not write to `/usr`.
    #[test]
    fn a_packaged_linux_install_is_never_updated_in_place() {
        assert_eq!(
            classify_unix(None, false, Path::new("/usr/bin/tankovault")),
            Flavour::Package
        );
        assert_eq!(
            classify_unix(None, false, Path::new("/opt/tankovault/tankovault")),
            Flavour::Package
        );
        assert!(!Flavour::Package.can_install());
        assert!(Flavour::Package.unmanaged_reason().is_some());
    }

    /// An `AppImage` this app cannot write to is announced, not downloaded: the swap would fail
    /// after a hundred megabytes had been fetched.
    #[test]
    fn an_unwritable_appimage_falls_back_to_announcing() {
        let image = Path::new("/home/reader/Apps/Tankovault.AppImage");
        assert_eq!(classify_unix(Some(image), true, image), Flavour::AppImage);
        assert_eq!(classify_unix(Some(image), false, image), Flavour::Portable);
    }

    #[test]
    fn only_the_installable_flavours_name_a_manifest_target() {
        assert_eq!(Flavour::Msi.kind(), Some("msi"));
        assert_eq!(Flavour::Nsis.kind(), Some("nsis"));
        assert_eq!(Flavour::AppImage.kind(), Some("appimage"));
        assert_eq!(Flavour::Portable.kind(), None);
        assert_eq!(Flavour::Package.kind(), None);
    }

    fn parse(args: &[&str]) -> Invocation {
        invocation(args.iter().map(std::ffi::OsString::from))
    }

    /// The relauncher is a **copy of the app** in the staging directory, so every command line
    /// that is not unambiguously the relauncher's has to be one that never launches a window
    /// from there.
    ///
    /// The trap this pins: reading the flag and then falling through when no path follows it.
    /// That start is not the app: it is a second copy of it, running out of a temporary
    /// directory, updating nothing and indistinguishable on screen from the installed one.
    #[test]
    fn only_a_complete_relaunch_command_line_is_the_relauncher() {
        assert_eq!(parse(&[]), Invocation::App);
        assert_eq!(parse(&["--help"]), Invocation::App);
        // The path is only read *after* the flag; a bare path is an ordinary start.
        assert_eq!(
            parse(&[r"C:\Program Files\Tankovault\tankovault.exe"]),
            Invocation::App
        );
        assert_eq!(
            parse(&[RELAUNCH_FLAG, r"C:\Program Files\Tankovault\tankovault.exe"]),
            Invocation::Relaunch(PathBuf::from(r"C:\Program Files\Tankovault\tankovault.exe"))
        );
        assert_eq!(parse(&[RELAUNCH_FLAG]), Invocation::RelaunchWithoutTarget);
    }

    #[test]
    fn progress_saturates_rather_than_overflowing() {
        assert_eq!(percentage(0, 100), 0);
        assert_eq!(percentage(50, 100), 50);
        assert_eq!(percentage(100, 100), 100);
        assert_eq!(percentage(200, 100), 100);
        assert_eq!(percentage(10, 0), 0);
        assert_eq!(percentage(u64::MAX, 1), 100);
    }
}
