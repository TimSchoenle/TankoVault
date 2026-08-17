//! One running instance per reader, and the handoff that makes a second launch behave like one.
//!
//! **Why this is a session bug and not a nicety.** Two copies of this app cannot coexist. They
//! share one OS credential-store entry and, through it, one server-side refresh-token *family* —
//! a rotating single-token lineage the API polices with reuse detection. Each copy rotates on its
//! own fourteen-minute schedule, and every rotation revokes the token the other one is still
//! holding. The first rotation after the second launch usually lands inside the API's one-minute
//! grace window and is forgiven; the next one does not, and a token presented long after a
//! rotation the holder never took delivery of is indistinguishable from a stolen one. The API
//! answers `401` and revokes the whole family, and `Session::clear` then deletes the stored
//! credential — so *both* windows land on the sign-in screen and the next start has nothing left
//! to restore. That is the "randomly signed out after leaving it closed for a few hours" report,
//! and closing it is what this module is for.
//!
//! **The lock is an OS one, never a pid file.** A pid file left behind by a crash locks the
//! reader out of their own app until they find and delete it. Both primitives below are released
//! by the kernel when the process ends, however it ends — and the Unix side treats a socket that
//! outlived its owner as the stale entry it is rather than as a live instance.
//!
//! **A refused launch still has to do something.** An app that appears to ignore its own icon
//! reads as broken, so the duplicate leaves an activation request behind before it exits and the
//! running instance brings its window forward — see [`request_activation`] and
//! [`crate::components::TrayHost`], which owns the one loop already holding a window handle.

use std::path::{Path, PathBuf};

/// The lock file. Never read or written — its *handle* is the lock, and the bytes are incidental.
const LOCK_FILE_NAME: &str = "instance.lock";

/// The activation request. Its existence is the whole message, so it is created empty and
/// consumed by removal.
const ACTIVATION_FILE_NAME: &str = "instance.activate";

/// Proof that this process is the one instance. **Hold it for the lifetime of the app**: dropping
/// it releases the lock, and a later launch would then start a second copy beside this one.
pub(crate) struct InstanceLock(
    #[expect(
        dead_code,
        reason = "held for its Drop: the kernel releases the handle, and with it the lock, when \
                  this closes"
    )]
    Option<imp::Lock>,
);

/// What the platform said about the lock, before it is turned into a start-or-hand-off decision.
enum Outcome {
    /// This process took the lock.
    Held(imp::Lock),
    /// Another live instance holds it.
    Contended,
    /// The lock could not be evaluated at all — no config directory, or an unwritable one.
    Unavailable,
}

pub(crate) fn acquire_instance_lock() -> Option<InstanceLock> {
    match lock_path().map_or(Outcome::Unavailable, |path| imp::acquire(&path)) {
        Outcome::Held(lock) => Some(InstanceLock(Some(lock))),
        Outcome::Unavailable => {
            // Not fatal, but it is the state in which the guarantee above stops holding — so a
            // "signed out again" report from a machine whose log says this is a different bug
            // from one whose log does not.
            tracing::warn!(
                path = ?lock_path(),
                "the single-instance lock could not be evaluated; starting without it"
            );
            Some(InstanceLock(None))
        }
        Outcome::Contended => None,
    }
}

/// Ask the instance that holds the lock to bring its window forward. Best effort: a request that
/// cannot be written leaves the reader with a launch that did nothing visible, which is the same
/// place they were before this existed.
pub(crate) fn request_activation() {
    if let Some(path) = activation_path() {
        write_request(&path);
    }
}

/// Take a pending activation request, if there is one.
///
/// True at most once per request: the file *is* the request and removing it is what consumes it,
/// so two polls cannot both act on one launch.
pub(crate) fn take_activation_request() -> bool {
    activation_path().is_some_and(|path| std::fs::remove_file(path).is_ok())
}

/// Create the request, making the directory first — a second launch can be the first thing that
/// ever needs it.
fn write_request(path: &Path) {
    if path
        .parent()
        .is_some_and(|dir| std::fs::create_dir_all(dir).is_ok())
    {
        let _ = std::fs::File::create(path);
    }
}

fn lock_path() -> Option<PathBuf> {
    prepared_config_dir().map(|dir| dir.join(LOCK_FILE_NAME))
}

fn activation_path() -> Option<PathBuf> {
    super::desktop::config_dir().map(|dir| dir.join(ACTIVATION_FILE_NAME))
}

/// The config directory, created if it is missing. `None` when the platform exposes none or it
/// cannot be made — both of which [`acquire_instance_lock`] reads as `Unavailable`.
fn prepared_config_dir() -> Option<PathBuf> {
    let dir = super::desktop::config_dir()?;
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

/// Windows: the lock is the file handle's own sharing mode, which the kernel drops when the
/// process ends.
#[cfg(windows)]
mod imp {
    use super::Outcome;
    use std::fs::OpenOptions;
    use std::os::windows::fs::OpenOptionsExt as _;
    use std::path::Path;

    /// `FILE_SHARE_READ`, and deliberately not `0`. With no sharing at all, any process that so
    /// much as opens the file — a backup agent, a search indexer, an anti-malware scanner — makes
    /// this open fail, and the app would refuse to start over a scan it has nothing to do with.
    /// Sharing reads denies the one thing that has to be denied, a second writer, and nothing else.
    const FILE_SHARE_READ: u32 = 0x0000_0001;

    /// `ERROR_SHARING_VIOLATION` and `ERROR_LOCK_VIOLATION` — the two ways Windows says "someone
    /// else has this open on terms that exclude you". Matched by number because neither has a
    /// stable [`std::io::ErrorKind`], and reading every other error as contention would turn a
    /// full disk into a silent refusal to start.
    const SHARING_VIOLATIONS: [i32; 2] = [32, 33];

    pub(super) struct Lock(
        #[expect(
            dead_code,
            reason = "held for its Drop: closing the handle is what releases the sharing mode"
        )]
        std::fs::File,
    );

    pub(super) fn acquire(path: &Path) -> Outcome {
        match OpenOptions::new()
            .write(true)
            .create(true)
            .share_mode(FILE_SHARE_READ)
            .open(path)
        {
            Ok(file) => Outcome::Held(Lock(file)),
            Err(error)
                if error
                    .raw_os_error()
                    .is_some_and(|code| SHARING_VIOLATIONS.contains(&code)) =>
            {
                Outcome::Contended
            }
            Err(_) => Outcome::Unavailable,
        }
    }
}

/// Unix: the lock is a bound socket, because binding is the one filesystem operation whose
/// exclusivity survives a `SIGKILL` without leaving a lockout behind.
#[cfg(unix)]
mod imp {
    use super::Outcome;
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::Path;

    pub(super) struct Lock(
        #[expect(
            dead_code,
            reason = "held for its Drop: closing the listener is what releases the name"
        )]
        UnixListener,
    );

    pub(super) fn acquire(path: &Path) -> Outcome {
        match UnixListener::bind(path) {
            Ok(listener) => Outcome::Held(Lock(listener)),
            Err(error) if error.kind() == std::io::ErrorKind::AddrInUse => reclaim(path),
            Err(_) => Outcome::Unavailable,
        }
    }

    /// `AddrInUse` reports that the *path* is taken, not that anything is listening on it — a
    /// socket file outlives the process that made it, so a crash would otherwise lock the reader
    /// out for good. Connecting is the only way to tell the two apart: refused means the owner is
    /// gone, and the stale entry is removed and the bind retried.
    fn reclaim(path: &Path) -> Outcome {
        if UnixStream::connect(path).is_ok() {
            return Outcome::Contended;
        }
        if std::fs::remove_file(path).is_err() {
            return Outcome::Unavailable;
        }
        UnixListener::bind(path).map_or(Outcome::Unavailable, |listener| {
            Outcome::Held(Lock(listener))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{imp, write_request, Outcome, ACTIVATION_FILE_NAME, LOCK_FILE_NAME};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// A directory of this test's own, since the lock is a name in the filesystem and two tests
    /// sharing one would contend with each other rather than with what they are asserting.
    fn scratch() -> PathBuf {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "tankovault-instance-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).expect("a scratch directory");
        dir
    }

    fn held(outcome: &Outcome) -> bool {
        matches!(outcome, Outcome::Held(_))
    }

    /// The bug: the desktop client had no single-instance guard, so a second launch — an
    /// accidental double click, or the shortcut pressed again while the first copy sat in the
    /// tray — ran a second copy against the same OS credential entry and the same server-side
    /// refresh-token family. The two rotated over each other until one presented a token revoked
    /// outside the API's grace window, which is reuse as far as the API can tell: it revoked the
    /// family, and both copies were signed out with the stored credential deleted behind them.
    #[test]
    fn a_second_instance_is_refused_while_the_first_holds_the_lock() {
        let path = scratch().join(LOCK_FILE_NAME);
        let first = imp::acquire(&path);
        assert!(held(&first), "the first launch takes the lock");
        assert!(
            matches!(imp::acquire(&path), Outcome::Contended),
            "a second launch must be told the app is already running, not started beside it"
        );
        drop(first);
    }

    /// The other half of the same rule, and the reason the lock is an OS handle rather than a pid
    /// file: an instance that ends — cleanly, or by being killed — must leave the app startable.
    #[test]
    fn the_lock_is_free_again_once_the_holder_releases_it() {
        let path = scratch().join(LOCK_FILE_NAME);
        drop(imp::acquire(&path));
        assert!(
            held(&imp::acquire(&path)),
            "a released lock must not lock the reader out of their own app"
        );
    }

    /// `Unavailable` is not `Contended`, and conflating them would make an unwritable config
    /// directory look exactly like a running instance — the app would refuse to start and say
    /// nothing. A path under a *file* is the cheapest way to have the platform refuse.
    #[test]
    fn an_unusable_location_reports_unavailable_rather_than_contention() {
        let blocker = scratch().join("not-a-directory");
        std::fs::write(&blocker, b"").expect("a file to block the path");
        assert!(
            matches!(
                imp::acquire(&blocker.join(LOCK_FILE_NAME)),
                Outcome::Unavailable
            ),
            "only a live instance may be reported as contention"
        );
    }

    /// One launch is one raise. The request is consumed by removal so that a poll cadence faster
    /// than the reader can click cannot turn a single launch into a window that keeps stealing
    /// focus.
    #[test]
    fn an_activation_request_is_consumed_exactly_once() {
        let path = scratch().join(ACTIVATION_FILE_NAME);
        write_request(&path);
        assert!(
            std::fs::remove_file(&path).is_ok(),
            "the request is pending"
        );
        assert!(
            std::fs::remove_file(&path).is_err(),
            "a consumed request must not be delivered twice"
        );
    }
}
