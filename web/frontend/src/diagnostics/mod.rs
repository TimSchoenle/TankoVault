//! What the desktop client leaves behind when it goes wrong.
//!
//! A `panic = "abort"` GUI process dies with no console, no dialog and no trace: the reader sees
//! the window vanish and has nothing to send anyone, which is the report this module exists to
//! answer. Three things come out of it, and they cover three different failures:
//!
//! * a **rolling log** in [`crate::platform::log_dir`], carrying this app's own events and those
//!   of the libraries under it (`wry`, `tao`, `reqwest`) — what the app was doing;
//! * a **crash report** per panic, written by [`crash`] — what went wrong, when Rust knows;
//! * a **session marker**, held by [`session`] — that something went wrong, when Rust does not.
//!   A `WebView2` host dying under the renderer runs no Rust code at all, and the marker is the
//!   only evidence such a session leaves.
//!
//! Nothing here is allowed to fail the app it is watching. Every path swallows its own errors: a
//! locked-down profile directory costs the reader diagnostics, and must not cost them the app.
//!
//! Desktop only. The web build has the browser's own console and devtools, and a WASM trap there
//! has no filesystem to write to.

mod crash;
mod session;
mod sink;

use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::fmt::format::{DefaultFields, Format, Full, Writer};
use tracing_subscriber::fmt::time::FormatTime;
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::layer::SubscriberExt as _;
use tracing_subscriber::util::SubscriberInitExt as _;

/// Raises or narrows the level, in `RUST_LOG` syntax — `TANKOVAULT_LOG=debug,wry=trace`.
///
/// Its own name rather than `RUST_LOG`, so that turning this app up does not also turn up every
/// other Rust program in the shell that launched it.
const LEVEL_ENV: &str = "TANKOVAULT_LOG";

/// The live log file. Rolled generations sit beside it as `.1`, `.2`, `.3`.
const LOG_FILE_NAME: &str = "tankovault.log";

/// Start logging and arm the crash reporter. **Call this first in `main`**, before anything that
/// can fail — a crash during startup is exactly the one this app could not previously explain.
///
/// Safe to call more than once: the subscriber is installed with `try_init`, which declines
/// rather than aborts when one is already set.
pub(crate) fn install() {
    let dir = crate::platform::log_dir();
    let file = dir
        .as_ref()
        .map(|dir| sink::Rolling::open(dir.join(LOG_FILE_NAME)));

    // Set here rather than left to `dioxus::launch`, which installs its own default only when no
    // dispatcher exists yet — so this one wins and the libraries below still report into it.
    let _ = tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::builder()
                .with_default_directive(LevelFilter::INFO.into())
                .with_env_var(LEVEL_ENV)
                .from_env_lossy(),
        )
        .with(text_layer(std::io::stderr))
        .with(file.map(text_layer))
        .try_init();

    crash::install(dir.clone());

    tracing::info!(
        version = crate::build_info::VERSION,
        commit = crate::build_info::commit().unwrap_or("unrecorded"),
        os = std::env::consts::OS,
        arch = std::env::consts::ARCH,
        pid = std::process::id(),
        logs = ?dir,
        "starting"
    );
}

/// Record that this process is the running instance. Call once the instance lock is held; see
/// [`session::start`] for why not before.
pub(crate) fn begin_session() {
    session::start();
}

/// Record that this process is ending on purpose, so the next start does not report it as a kill.
pub(crate) fn end_session() {
    tracing::info!("shutting down");
    session::end();
}

/// One plain-text layer, so the file and stderr are formatted identically and a reader pasting
/// either into an issue gives a maintainer the same thing.
fn text_layer<S, W>(
    writer: W,
) -> tracing_subscriber::fmt::Layer<S, DefaultFields, Format<Full, Timestamp>, W>
where
    W: for<'writer> MakeWriter<'writer> + 'static,
{
    tracing_subscriber::fmt::layer()
        // Escape codes in a file are noise, and the Windows console this may or may not have is
        // not worth branching on.
        .with_ansi(false)
        .with_timer(Timestamp)
        .with_thread_names(true)
        .with_writer(writer)
}

/// Local time with an offset, not UTC: the line a reader has to correlate with "it died about ten
/// minutes ago" is the only thing this timestamp is ever used for, and the offset keeps it
/// unambiguous once the file reaches a maintainer in another zone.
struct Timestamp;

impl FormatTime for Timestamp {
    fn format_time(&self, writer: &mut Writer<'_>) -> std::fmt::Result {
        write!(
            writer,
            "{}",
            chrono::Local::now().format("%Y-%m-%dT%H:%M:%S%.3f%:z")
        )
    }
}
