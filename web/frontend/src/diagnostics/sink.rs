//! The rolling log file, and the one rule it may never break: writing a log line must not fail
//! the thing being logged.
//!
//! Every error here — no directory, no permission, a full disk, a rename another process is
//! holding open — is swallowed and the write reported as accepted. This writer is called from
//! inside the panic hook, and a logger that turned its own failure into a second panic would take
//! the crash report down with it.

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use tracing_subscriber::fmt::MakeWriter;

/// Roll the live file once it passes this: small enough that a reader can attach one to an issue,
/// large enough to hold a long reading session at `info`.
const MAX_BYTES: u64 = 2 * 1024 * 1024;

/// Rotated generations kept beside the live file. The oldest is discarded on each roll.
const GENERATIONS: usize = 3;

/// An append-only log file that rolls itself, cloned into the `tracing` layer that writes to it.
#[derive(Clone)]
pub(super) struct Rolling(Arc<Mutex<State>>);

impl Rolling {
    /// Open — or create — the log at `path`.
    ///
    /// A path that cannot be opened yields a sink that discards, and it does not try again: the
    /// only reasons this fails are structural (no profile directory, a policy-locked path), and
    /// retrying one per log line would cost every call site a syscall for a file that is never
    /// going to appear.
    pub(super) fn open(path: PathBuf) -> Self {
        let mut state = State {
            path,
            file: None,
            written: 0,
            ceiling: MAX_BYTES,
        };
        state.reopen();
        Self(Arc::new(Mutex::new(state)))
    }
}

impl<'a> MakeWriter<'a> for Rolling {
    type Writer = Handle<'a>;

    fn make_writer(&'a self) -> Self::Writer {
        // `into_inner`, never `unwrap`: a poisoned lock means another thread panicked, which is
        // exactly when this log is worth having.
        Handle(self.0.lock().unwrap_or_else(PoisonError::into_inner))
    }
}

/// One locked write, held for the duration of a single event — which is what keeps two threads'
/// lines from interleaving mid-line.
pub(super) struct Handle<'a>(MutexGuard<'a, State>);

impl Write for Handle<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.append(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        if let Some(file) = self.0.file.as_mut() {
            let _ = file.flush();
        }
        Ok(())
    }
}

struct State {
    path: PathBuf,
    /// `None` once the file has proved unopenable; see [`Rolling::open`].
    file: Option<File>,
    written: u64,
    /// The size at which the next roll is attempted.
    ///
    /// Not simply [`MAX_BYTES`]: a roll fails when another process — the update relauncher, which
    /// is this same binary — holds the file open, because Windows refuses to rename across that.
    /// Raising the ceiling on a failed roll is what stops every subsequent write retrying the
    /// same rename.
    ceiling: u64,
}

impl State {
    fn append(&mut self, buf: &[u8]) {
        if self.written >= self.ceiling {
            self.roll();
        }
        let Some(file) = self.file.as_mut() else {
            return;
        };
        if file.write_all(buf).is_ok() {
            self.written = self
                .written
                .saturating_add(u64::try_from(buf.len()).unwrap_or(u64::MAX));
        }
    }

    fn roll(&mut self) {
        // Closed before the renames, because Windows refuses to rename a file this process still
        // holds open.
        self.file = None;
        for generation in (1..GENERATIONS).rev() {
            let _ = std::fs::rename(self.generation(generation), self.generation(generation + 1));
        }
        let _ = std::fs::rename(&self.path, self.generation(1));
        self.reopen();
        self.ceiling = self.written.saturating_add(MAX_BYTES);
    }

    /// Reopen the live file in append mode, adopting whatever is already in it.
    fn reopen(&mut self) {
        if let Some(dir) = self.path.parent() {
            if std::fs::create_dir_all(dir).is_err() {
                return;
            }
        }
        let Ok(file) = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
        else {
            return;
        };
        self.written = file.metadata().map_or(0, |meta| meta.len());
        self.file = Some(file);
    }

    /// `tankovault.log` → `tankovault.log.1`. A suffix rather than a replaced extension, so the
    /// generations sort next to the live file and keep their `.log` association.
    fn generation(&self, index: usize) -> PathBuf {
        let mut name = self.path.clone().into_os_string();
        name.push(format!(".{index}"));
        PathBuf::from(name)
    }
}

#[cfg(test)]
mod tests {
    use super::{Rolling, GENERATIONS, MAX_BYTES};
    use std::io::Write as _;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU32, Ordering};
    use tracing_subscriber::fmt::MakeWriter as _;

    fn scratch() -> PathBuf {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "tankovault-log-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).expect("a scratch directory");
        dir
    }

    fn write(sink: &Rolling, bytes: &[u8]) {
        sink.make_writer()
            .write_all(bytes)
            .expect("the sink accepts");
    }

    /// A log that grows without bound fills a reader's profile, and one that is truncated instead
    /// of rolled loses the lines leading up to the crash it was kept for.
    #[test]
    fn the_live_file_rolls_and_keeps_the_previous_generation() {
        let path = scratch().join("tankovault.log");
        let sink = Rolling::open(path.clone());
        let chunk = vec![b'x'; 64 * 1024];
        while path.metadata().is_ok_and(|meta| meta.len() < MAX_BYTES) {
            write(&sink, &chunk);
        }
        write(&sink, b"past the ceiling\n");

        let rolled = PathBuf::from({
            let mut name = path.clone().into_os_string();
            name.push(".1");
            name
        });
        assert!(rolled.is_file(), "the full file is kept as generation 1");
        assert!(
            path.metadata().expect("a fresh live file").len() < MAX_BYTES,
            "the live file starts again rather than growing without bound"
        );
        assert!(
            !PathBuf::from({
                let mut name = path.into_os_string();
                name.push(format!(".{}", GENERATIONS + 1));
                name
            })
            .exists(),
            "generations past the kept set must not accumulate"
        );
    }

    /// The bug this guards: a sink that reports an error when it cannot open its file makes
    /// `tracing` fail the write, and the one caller that must never fail is the panic hook. A
    /// directory that cannot be created has to read as "logged nowhere", not as an error.
    #[test]
    fn a_path_that_cannot_be_opened_discards_rather_than_failing() {
        let blocker = scratch().join("not-a-directory");
        std::fs::write(&blocker, b"").expect("a file to block the path");
        let sink = Rolling::open(blocker.join("logs").join("tankovault.log"));
        write(&sink, b"this line has nowhere to go\n");
    }
}
