//! The console's live push: one stream for the whole surface.
//!
//! Replaces a four-second timer that bumped a shared tick, after which every subscribed panel
//! re-issued its own GET — `system_stats` twice per tick, because the rail and Overview each
//! held one. Here the payload arrives once and both read it.
//!
//! Same ticket discipline as [`crate::live`]: the credential is single-use, so a transport's
//! own reconnect would replay a spent ticket into a `401` loop, and this reconnects itself with
//! a fresh ticket per attempt instead.

use crate::api::Api;
use crate::models::{ScanActivity, ScanRun, SystemStats};
use dioxus::prelude::*;

/// First wait after a failed attempt; doubles up to [`RECONNECT_BACKOFF_MAX_MS`].
const RECONNECT_BACKOFF_START_MS: u32 = 1_000;
/// Ceiling on the reconnect wait.
///
/// Lower than the notification stream's minute: a stale console is a *wrong* console, and an
/// operator watching a queue that stopped moving needs it back sooner than a badge does.
const RECONNECT_BACKOFF_MAX_MS: u32 = 15_000;
/// How long an attempt has to stay open before it counts as having worked.
///
/// The API ends each stream at the access token's lifetime, which looks identical to a failure
/// from here — duration tells them apart: a refused ticket fails in milliseconds, a served
/// stream lasts minutes. Without this, a healthy stream would ratchet the backoff up every time
/// it is recycled.
const SETTLED_MS: f64 = 5_000.0;

/// What the connection is doing, as the bar reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) enum LiveState {
    /// Opening the first attempt; nothing has arrived yet.
    #[default]
    Connecting,
    /// Attached, and pushes are landing.
    Live,
    /// Dropped; the next attempt is scheduled.
    Reconnecting,
    /// The operator detached. Numbers on screen are as old as the last push.
    Paused,
}

impl LiveState {
    /// The catalogue key wording this state for the operator (see [`crate::i18n`]).
    pub(super) fn label_key(self) -> &'static str {
        match self {
            Self::Connecting => "console.live.connecting",
            Self::Live => "console.live.streaming",
            Self::Reconnecting => "console.live.reconnecting",
            Self::Paused => "console.live.detached",
        }
    }

    /// Whether the numbers on screen can be trusted to be current.
    pub(super) fn is_current(self) -> bool {
        self == Self::Live
    }
}

/// The console's pushed state, provided as context so any panel can read it without a fetch.
#[derive(Clone, Copy)]
pub(super) struct ConsoleLive {
    pub(super) state: Signal<LiveState>,
    /// The rail's counts and Overview's tiles — one payload, two readers.
    pub(super) stats: Signal<Option<SystemStats>>,
    pub(super) runs: Signal<Option<Vec<ScanRun>>>,
    /// The task-level state of whatever is in flight. Separate from `runs` because it answers a
    /// different question: the counters say how far a run has got, this says whether it is
    /// actually moving.
    pub(super) activity: Signal<Option<ScanActivity>>,
}

impl ConsoleLive {
    fn new() -> Self {
        Self {
            state: Signal::new(LiveState::Connecting),
            stats: Signal::new(None),
            runs: Signal::new(None),
            activity: Signal::new(None),
        }
    }
}

/// Provide [`ConsoleLive`] and keep it fed for as long as the console is mounted.
///
/// `attached` is read, never written: detaching is the operator's decision, made in the bar.
pub(super) fn use_console_live(api: Api, attached: ReadSignal<bool>) -> ConsoleLive {
    let live = use_context_provider(ConsoleLive::new);
    let session = crate::state::use_session();

    // Restarts on pause and on sign-out, so detaching *closes* the connection rather than
    // merely ignoring it: a paused console must not hold a connection open, and a sign-out must
    // not leave one attached to the previous session.
    use_resource(move || {
        let signed_in = session.is_authenticated();
        let attached = attached();
        async move {
            let mut state = live.state;
            if !signed_in {
                state.set(LiveState::Connecting);
                return;
            }
            if !attached {
                state.set(LiveState::Paused);
                return;
            }
            run(api, live).await;
        }
    });

    live
}

/// Keep one stream open, reconnecting with a fresh ticket after each failure.
///
/// Runs until dropped — the caller's `use_resource` does that on pause, sign-out or unmount,
/// closing the stream.
async fn run(api: Api, live: ConsoleLive) {
    let mut state = live.state;
    let mut backoff_ms = RECONNECT_BACKOFF_START_MS;
    loop {
        // A fresh ticket per attempt: redeeming one spends it.
        let Ok(response) = api.client().stream_ticket().send().await else {
            // Covers a gone-away session (401) and a suspension (403); backing off rather than
            // giving up resumes a recovered session without a reload.
            state.set(LiveState::Reconnecting);
            crate::platform::sleep_ms(backoff_ms).await;
            backoff_ms = backoff_ms.saturating_mul(2).min(RECONNECT_BACKOFF_MAX_MS);
            continue;
        };
        let ticket = response.into_inner().ticket;

        if consume(&api, &ticket, live).await {
            backoff_ms = RECONNECT_BACKOFF_START_MS;
        }
        state.set(LiveState::Reconnecting);
        crate::platform::sleep_ms(backoff_ms).await;
        backoff_ms = backoff_ms.saturating_mul(2).min(RECONNECT_BACKOFF_MAX_MS);
    }
}

/// Open one stream with `ticket` and pump it until it ends.
///
/// Returns whether the attempt is judged to have *worked* — see [`SETTLED_MS`] for why that is
/// a duration rather than a status.
async fn consume(api: &Api, ticket: &str, live: ConsoleLive) -> bool {
    let url = format!("{}{}", api.base_url(), crate::api::admin_stream_url(ticket));
    // Every name off one connection: they arrive on their own cadences, and a stream per name
    // would stall `runs` behind the ten-second `stats` tick.
    let Some(mut stream) = crate::platform::subscribe(&url, &["stats", "runs", "activity"]).await
    else {
        // A malformed URL or a refused connection; neither is actionable here.
        return false;
    };

    let mut connection = live.state;
    let mut stats = live.stats;
    let mut runs = live.runs;
    let mut activity = live.activity;

    let started = crate::platform::now_ms();
    while let Some((name, text)) = stream.next().await {
        // Only on a delivered payload: the bar must not claim "live" on the strength of an open
        // socket alone. Guarded, because `set` invalidates unconditionally and this would
        // otherwise re-render the whole console twice a second to write the same value.
        if *connection.peek() != LiveState::Live {
            connection.set(LiveState::Live);
        }
        match name.as_str() {
            "stats" => {
                if let Ok(value) = serde_json::from_str::<SystemStats>(&text) {
                    stats.set(Some(value));
                }
            }
            "runs" => {
                if let Ok(value) = serde_json::from_str::<Vec<ScanRun>>(&text) {
                    runs.set(Some(value));
                }
            }
            "activity" => {
                if let Ok(value) = serde_json::from_str::<ScanActivity>(&text) {
                    activity.set(Some(value));
                }
            }
            // The subscription asked for two names; anything else is a server the client does
            // not yet know about, which is not a reason to drop the stream.
            _ => {}
        }
    }

    stream.close();
    crate::platform::now_ms() - started >= SETTLED_MS
}
