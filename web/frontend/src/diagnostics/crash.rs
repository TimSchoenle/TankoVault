//! The report a panic leaves behind.
//!
//! **This crate builds with `panic = "abort"`, so the hook below is the only code that runs
//! between a panic and the process disappearing.** It therefore has to be complete in one pass,
//! and it must not panic itself — a panic inside a panic hook aborts immediately, with the report
//! half-written and nothing said.
//!
//! The panic is recorded three ways, because the three answer different questions. The rolling
//! log puts it in sequence with whatever the app was doing. The report file is the artefact a
//! reader can attach to an issue without knowing where logs live. The dialog exists because
//! neither of those is any use to somebody whose only evidence is that the window vanished: a
//! desktop app that dies silently is indistinguishable from one they closed themselves, and that
//! is the complaint this module was written for.
//!
//! Frames in the backtrace may be bare addresses. The shipped profile carries no debug info and
//! strips symbols, so the *location* line — which comes from `Location` and is compiled in
//! regardless — is the field to read first.

use std::panic::PanicHookInfo;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

/// Reports kept in the log directory, oldest removed as new ones land.
///
/// The interesting report is the one that matches the reader's complaint, and an unbounded
/// directory of them in a user profile is its own defect.
const KEEP_REPORTS: usize = 10;

/// Prefix and extension the pruner recognises. Also what a reader is told to look for.
const REPORT_PREFIX: &str = "crash-";
const REPORT_EXTENSION: &str = "log";

/// Install the panic hook. `dir` is where reports go, or `None` when the platform exposes no
/// directory to write them in — the panic is still logged and still announced.
pub(super) fn install(dir: Option<PathBuf>) {
    let inherited = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        // Two threads can panic at once. The first owns the report; the second must not race it
        // into the same file or stack a second dialog on top of the first.
        static REPORTING: AtomicBool = AtomicBool::new(false);
        if REPORTING.swap(true, Ordering::SeqCst) {
            inherited(info);
            return;
        }

        let report = Report::capture(info);
        tracing::error!(
            thread = report.thread.as_str(),
            location = report.location.as_str(),
            "panic: {}",
            report.message
        );
        let written = dir.as_deref().and_then(|dir| write(dir, &report));
        // The panic explains this session ending and is already recorded, so the marker must go
        // with it — otherwise the next start reports an unexplained kill as well.
        super::session::end();
        announce(written.as_deref());
        inherited(info);
    }));
}

/// Everything about a panic that is knowable from inside the hook.
struct Report {
    when: String,
    thread: String,
    location: String,
    message: String,
    backtrace: String,
}

impl Report {
    fn capture(info: &PanicHookInfo<'_>) -> Self {
        let current = std::thread::current();
        Self {
            when: chrono::Local::now().to_rfc3339(),
            thread: current.name().unwrap_or("<unnamed>").to_owned(),
            location: info
                .location()
                .map_or_else(|| "<unknown>".to_owned(), ToString::to_string),
            message: info
                .payload_as_str()
                .unwrap_or("<panic payload was not a string>")
                .to_owned(),
            // Forced, not `capture`: `RUST_BACKTRACE` is never set for an app started from a
            // shortcut, and a report without frames is most of the value gone.
            backtrace: std::backtrace::Backtrace::force_capture().to_string(),
        }
    }

    fn render(&self) -> String {
        format!(
            "{product} {version} crash report\n\
             \n\
             when       {when}\n\
             commit     {commit}\n\
             build      {build}\n\
             platform   {os} {arch}\n\
             pid        {pid}\n\
             thread     {thread}\n\
             location   {location}\n\
             panic      {message}\n\
             \n\
             backtrace\n\
             {backtrace}\n",
            product = crate::build_info::PRODUCT_NAME,
            version = crate::build_info::VERSION,
            when = self.when,
            commit = crate::build_info::commit().unwrap_or("unrecorded"),
            build = if cfg!(debug_assertions) {
                "debug"
            } else {
                "release (symbols stripped — read `location` before the frames)"
            },
            os = std::env::consts::OS,
            arch = std::env::consts::ARCH,
            pid = std::process::id(),
            thread = self.thread,
            location = self.location,
            message = self.message,
            backtrace = self.backtrace,
        )
    }
}

/// Write the report and return where it landed, or `None` if it could not be written at all.
fn write(dir: &Path, report: &Report) -> Option<PathBuf> {
    std::fs::create_dir_all(dir).ok()?;
    let path = dir.join(format!(
        "{REPORT_PREFIX}{stamp}-{pid}.{REPORT_EXTENSION}",
        stamp = chrono::Local::now().format("%Y%m%d-%H%M%S"),
        pid = std::process::id()
    ));
    std::fs::write(&path, report.render()).ok()?;
    prune(dir);
    Some(path)
}

/// Keep the newest [`KEEP_REPORTS`] and remove the rest.
///
/// Sorted by file name, which is chronological because the stamp in it is fixed-width and
/// big-endian. Nothing here consults the filesystem's own timestamps: a profile restored from a
/// backup carries mtimes that no longer order the reports.
fn prune(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut reports: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension().is_some_and(|ext| ext == REPORT_EXTENSION)
                && path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with(REPORT_PREFIX))
        })
        .collect();
    if reports.len() <= KEEP_REPORTS {
        return;
    }
    reports.sort_unstable();
    for stale in &reports[..reports.len() - KEEP_REPORTS] {
        let _ = std::fs::remove_file(stale);
    }
}

/// Tell the reader, because the window is about to vanish and this is the only warning they get.
///
/// Windows only, and deliberately. `rfd`'s Linux backend goes through GTK or the XDG portal, and
/// reaching either from a thread that is already dying — or from the GTK main thread itself —
/// risks hanging the process instead of ending it, which is a worse failure than the silent exit
/// it was meant to replace. A Linux user has a terminal and the stderr this also writes to.
#[cfg(windows)]
fn announce(report: Option<&Path>) {
    let detail = report.map_or_else(
        || "No crash report could be written — check the log folder.".to_owned(),
        |path| format!("A crash report was written to:\n{}", path.display()),
    );
    let _ = rfd::MessageDialog::new()
        .set_level(rfd::MessageLevel::Error)
        .set_title(format!(
            "{} stopped unexpectedly",
            crate::build_info::PRODUCT_NAME
        ))
        .set_description(detail)
        .set_buttons(rfd::MessageButtons::Ok)
        .show();
}

#[cfg(not(windows))]
fn announce(_report: Option<&Path>) {}

#[cfg(test)]
mod tests {
    use super::{prune, KEEP_REPORTS, REPORT_PREFIX};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU32, Ordering};

    fn scratch() -> PathBuf {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "tankovault-crash-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).expect("a scratch directory");
        dir
    }

    /// Reports accumulate in a user's profile forever otherwise, and the pruner has to drop the
    /// *oldest* — a pruner that kept the wrong end would delete the report the reader just made
    /// while asking for help.
    #[test]
    fn pruning_keeps_the_newest_reports_and_nothing_else() {
        let dir = scratch();
        std::fs::write(dir.join("tankovault.log"), b"not a report").expect("a live log");
        for day in 1..=KEEP_REPORTS + 5 {
            std::fs::write(
                dir.join(format!("{REPORT_PREFIX}202601{day:02}-000000-1.log")),
                b"",
            )
            .expect("a report");
        }

        prune(&dir);

        let mut kept: Vec<String> = std::fs::read_dir(&dir)
            .expect("the directory")
            .flatten()
            .filter_map(|entry| entry.file_name().into_string().ok())
            .filter(|name| name.starts_with(REPORT_PREFIX))
            .collect();
        kept.sort_unstable();
        assert_eq!(kept.len(), KEEP_REPORTS, "only the kept set survives");
        assert_eq!(
            kept.first().map(String::as_str),
            Some(format!("{REPORT_PREFIX}20260106-000000-1.log").as_str()),
            "the oldest reports are the ones dropped"
        );
        assert!(
            dir.join("tankovault.log").is_file(),
            "the live log is not a crash report and must survive pruning"
        );
    }
}
