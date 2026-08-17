//! The marker that turns "the window just vanished" into a line in the log.
//!
//! **This is the half of crash reporting that a Rust hook cannot cover.** A panic runs
//! [`super::crash`]'s hook and is recorded in full. An access violation, a stack overflow, a
//! `WebView2` host process dying under the renderer, or the OS killing the app runs *nothing* —
//! there is no unwinding, no hook, and this crate forbids the `unsafe` a structured-exception
//! handler would need.
//!
//! So a session writes a marker when it starts and removes it when the event loop is destroyed.
//! A marker still present at the *next* start is the only evidence that the last one was killed
//! rather than closed, and it is what tells a maintainer to stop looking for a panic.

use std::path::PathBuf;
use std::sync::OnceLock;

/// Its presence is the whole message; the contents are for the human reading the report.
const MARKER_FILE_NAME: &str = "session.running";

fn marker() -> Option<&'static PathBuf> {
    static MARKER: OnceLock<Option<PathBuf>> = OnceLock::new();
    MARKER
        .get_or_init(|| crate::platform::log_dir().map(|dir| dir.join(MARKER_FILE_NAME)))
        .as_ref()
}

/// Record that this process is running, and report a previous session that never removed its own.
///
/// Called only once the instance lock is held, so neither the update relauncher nor a duplicate
/// launch — both of them this same binary, and both exiting before that point — can clear a
/// marker belonging to the copy that is actually running.
pub(super) fn start() {
    let Some(path) = marker() else {
        return;
    };
    if let Ok(previous) = std::fs::read_to_string(path) {
        tracing::error!(
            previous = previous.trim(),
            "the previous session ended without a clean shutdown and without a panic, so it was \
             killed from outside Rust — the webview host, the OS, or a hardware fault"
        );
    }
    let _ = std::fs::write(
        path,
        format!(
            "pid {} · v{} · started {}",
            std::process::id(),
            crate::build_info::VERSION,
            chrono::Local::now().to_rfc3339()
        ),
    );
}

/// Record that this process is ending on purpose.
///
/// Two callers, and both mean "this ending is accounted for": the event loop being destroyed, and
/// the panic hook — a panic writes its own report, so leaving the marker behind would make the
/// next start blame the OS for something Rust already explained.
pub(super) fn end() {
    if let Some(path) = marker() {
        let _ = std::fs::remove_file(path);
    }
}
