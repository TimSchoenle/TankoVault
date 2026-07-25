//! Watchlist (`DESIGN_SPEC` §7.4) — a horizontally-scrolling **kanban** of status columns.
//! Cards move between columns with native HTML5 drag-and-drop; a per-card status `<select>`
//! is kept as the accessible, keyboard-operable equivalent (quality floor §11 — drag is never
//! the only mover). Each card carries a notify toggle and a remove action.

use crate::api;
use crate::components::{async_list, Cover, SignInGate};
use crate::hooks::{use_busy, use_outcome, use_reload, Busy, Reload};
use crate::icons::{Ic, Icon};
use crate::models::*;
use crate::state::use_session;
use crate::Route;
use dioxus::prelude::*;
use progenitor_client::ResponseValue;

/// The card being dragged: its id and current notify flag, which is preserved across the move
/// so a drop only changes the column.
type Dragging = Option<(SeriesId, bool)>;

/// The provider the "Sync now" button drives. Per-provider control lives on Account → Sync.
const SYNC_PROVIDER: &str = "anilist";

#[component]
pub(crate) fn Watchlist() -> Element {
    let session = use_session();
    let api = api::use_api();
    let reload = use_reload();
    let syncing = use_busy();
    let mut outcome = use_outcome();

    // Drag state, shared between the source cards and the drop-target columns.
    let dragging = use_signal(|| Dragging::None);
    let dragover = use_signal(|| Option::<WatchStatus>::None);

    let items = use_resource(move || {
        reload.track();
        let client = api.client();
        let authed = session.is_authenticated();
        async move {
            if !authed {
                return Ok(Vec::new());
            }
            client
                .watchlist()
                .send()
                .await
                .map(ResponseValue::into_inner)
                .map_err(api::friendly_error)
        }
    });

    if !session.is_authenticated() {
        return rsx! {
            h1 { class: "ik-page-title", "Watchlist" }
            SignInGate {}
        };
    }

    let sync_now = move |_| {
        if !syncing.claim() {
            return;
        }
        outcome.set(None);
        let client = api.client();
        spawn(async move {
            let opts = SyncOpts {
                policy: Some(ConflictPolicy::NewestWins.token().to_owned()),
            };
            // Pull first, then push: importing the remote list before reflecting local state
            // means a title added on the other side is not immediately overwritten.
            let result = match client
                .sync_pull()
                .provider(SYNC_PROVIDER)
                .body(SyncPullBody::Variant1(opts.clone()))
                .send()
                .await
            {
                Ok(_) => client
                    .sync_push()
                    .provider(SYNC_PROVIDER)
                    .body(SyncPushBody::Variant1(opts))
                    .send()
                    .await
                    .map(|_| "Synced with AniList.".to_owned())
                    .map_err(api::friendly_error),
                Err(e) => Err(api::friendly_error(e)),
            };
            if result.is_ok() {
                reload.bump();
            }
            outcome.set(Some(result));
            syncing.release();
        });
    };

    rsx! {
        div { class: "ik-page-head",
            div {
                h1 { class: "ik-page-title", style: "margin-bottom:2px;", "Watchlist" }
                div { class: "ik-muted", style: "font-size:13px;",
                    "Drag a title between columns to change its status — or use the picker on each card."
                }
            }
            button { class: "ik-btn", disabled: syncing.is_busy(), onclick: sync_now,
                Ic { icon: Icon::CloudSync, size: 16 }
                if syncing.is_busy() { "Syncing…" } else { "Sync AniList" }
            }
        }
        crate::components::OutcomeLine { outcome: outcome.read().clone() }
        {
            async_list(
                &items,
                reload,
                || rsx! {
                    div { class: "ik-board",
                        for _ in 0..5 {
                            div { class: "ik-col",
                                div { class: "ik-skeleton", style: "height:16px;width:60%;margin-bottom:12px;" }
                                div { class: "ik-skeleton", style: "height:64px;" }
                            }
                        }
                    }
                },
                "Your watchlist is empty. Find a series and add it to start tracking.",
                |items| rsx! {
                    div { class: "ik-board",
                        for status in WatchStatus::columns().iter().copied() {
                            Column {
                                key: "{status.token()}",
                                status,
                                items: items.iter().filter(|i| i.status == status).cloned().collect::<Vec<_>>(),
                                reload,
                                dragging,
                                dragover,
                            }
                        }
                    }
                },
            )
        }
    }
}

/// Column accent + icon per the design's status→role-colour map (§7.4).
fn column_style(status: WatchStatus) -> (Icon, &'static str) {
    match status {
        WatchStatus::Reading => (Icon::Fire, "var(--acc)"),
        WatchStatus::Planned => (Icon::Schedule, "var(--color-type-manga)"),
        WatchStatus::Completed => (Icon::TaskAlt, "var(--jade-bright)"),
        WatchStatus::Paused => (Icon::PauseCircle, "var(--color-status-hiatus)"),
        WatchStatus::Dropped => (Icon::Cancel, "var(--muted)"),
    }
}

#[component]
fn Column(
    status: WatchStatus,
    items: Vec<WatchlistItem>,
    reload: Reload,
    dragging: Signal<Dragging>,
    dragover: Signal<Option<WatchStatus>>,
) -> Element {
    let api = api::use_api();
    let count = items.len();
    let (icon, color) = column_style(status);
    let class = if *dragover.read() == Some(status) {
        "ik-col dragover"
    } else {
        "ik-col"
    };

    let on_drop = move |event: Event<DragData>| {
        event.prevent_default();
        let mut dragover = dragover;
        let mut dragging = dragging;
        dragover.set(None);
        let payload = *dragging.read();
        dragging.set(None);

        let Some((series_id, notify)) = payload else {
            return;
        };
        let client = api.client();
        spawn(async move {
            let body = WatchlistUpsert {
                status: Some(status),
                notify: Some(notify),
            };
            if client
                .put_watchlist()
                .series_id(series_id)
                .body(body)
                .send()
                .await
                .is_ok()
            {
                reload.bump();
            }
        });
    };

    rsx! {
        div {
            class: "{class}",
            ondragover: move |event| {
                event.prevent_default();
                let mut dragover = dragover;
                if *dragover.peek() != Some(status) {
                    dragover.set(Some(status));
                }
            },
            ondragleave: move |_| {
                let mut dragover = dragover;
                if *dragover.peek() == Some(status) {
                    dragover.set(None);
                }
            },
            ondrop: on_drop,
            h3 {
                span { class: "ik-flex", style: "gap:7px;",
                    span { style: "color:{color};display:inline-flex;", Ic { icon, size: 16 } }
                    span { "{status.label()}" }
                }
                span { class: "count", "{count}" }
            }
            for item in items {
                WatchCard { key: "{item.series_id}", item, reload, dragging }
            }
        }
    }
}

#[component]
fn WatchCard(item: WatchlistItem, reload: Reload, dragging: Signal<Dragging>) -> Element {
    let api = api::use_api();
    let busy = use_busy();
    let series_id = item.series_id;
    let notify = item.notify;
    let status = item.status;

    // Title and cover come embedded in the watchlist payload. This card used to fetch
    // `GET /v1/series/{id}` per card to get them, which meant opening the board fired one
    // request per tracked title — a hundred-title watchlist opened a hundred connections and
    // rendered placeholder ids until they landed. The endpoint has embedded both fields for
    // exactly this reason since §9.3; using them makes the board a single request.
    let title = item.series_title.clone();
    let cover = item.cover_url.clone();

    /// Apply a watchlist change, refetching the board on success.
    fn upsert(
        api: api::Api,
        busy: Busy,
        reload: Reload,
        series_id: SeriesId,
        body: WatchlistUpsert,
    ) {
        if !busy.claim() {
            return;
        }
        let client = api.client();
        spawn(async move {
            if client
                .put_watchlist()
                .series_id(series_id)
                .body(body)
                .send()
                .await
                .is_ok()
            {
                reload.bump();
            }
            busy.release();
        });
    }

    let toggle_notify = move |_| {
        upsert(
            api,
            busy,
            reload,
            series_id,
            WatchlistUpsert {
                status: Some(status),
                notify: Some(!notify),
            },
        );
    };

    let move_status = move |event: Event<FormData>| {
        upsert(
            api,
            busy,
            reload,
            series_id,
            WatchlistUpsert {
                status: Some(WatchStatus::parse(&event.value())),
                notify: Some(notify),
            },
        );
    };

    let remove = move |_| {
        if !busy.claim() {
            return;
        }
        let client = api.client();
        spawn(async move {
            if client
                .delete_watchlist()
                .series_id(series_id)
                .send()
                .await
                .is_ok()
            {
                reload.bump();
            }
            busy.release();
        });
    };

    rsx! {
        div {
            class: "ik-wl-card",
            draggable: true,
            ondragstart: move |_| {
                let mut dragging = dragging;
                dragging.set(Some((series_id, notify)));
            },
            ondragend: move |_| {
                let mut dragging = dragging;
                dragging.set(None);
            },
            Link { to: Route::Series { id: series_id.to_string() }, class: "ik-flex",
                div { style: "width:40px;flex:none;",
                    Cover { url: cover, title: title.clone() }
                }
                div { class: "grow", style: "font-weight:600;font-size:13px;line-height:1.3;", "{title}" }
            }
            if item.unread > 0 {
                div { class: "ik-mono", style: "font-size:11px;color:var(--acc);margin-top:6px;",
                    "{item.unread} unread"
                }
            }
            div { class: "ik-flex", style: "margin-top:8px;justify-content:space-between;gap:6px;",
                select {
                    class: "ik-input",
                    style: "padding:4px 6px;font-size:12px;width:auto;",
                    "aria-label": "Move to column",
                    disabled: busy.is_busy(),
                    value: "{status.token()}",
                    onchange: move_status,
                    for option_status in WatchStatus::columns().iter().copied() {
                        option {
                            value: "{option_status.token()}",
                            selected: option_status == status,
                            "{option_status.label()}"
                        }
                    }
                }
                button {
                    class: if notify { "ik-pill vermilion" } else { "ik-pill" },
                    style: "cursor:pointer;background:none;",
                    title: "Toggle notifications",
                    disabled: busy.is_busy(),
                    onclick: toggle_notify,
                    if notify { "Notify on" } else { "Notify off" }
                }
                button {
                    class: "ik-pill",
                    style: "cursor:pointer;background:none;",
                    disabled: busy.is_busy(),
                    onclick: remove,
                    "Remove"
                }
            }
        }
    }
}
