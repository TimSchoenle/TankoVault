//! Watchlist (DESIGN_SPEC §7.4) — a horizontally-scrolling **kanban** of status columns
//! (Reading / Planned / Completed / Paused / Dropped). Cards are moved between columns with
//! native **HTML5 drag-and-drop**; a per-card status `<select>` is kept as the accessible,
//! keyboard-operable equivalent (quality floor §11 — DnD is never the only mover). Each card
//! carries a notify toggle and a remove action.

use crate::api;
use crate::components::{Cover, EmptyBox, ErrorBox, SignInGate};
use crate::icons::{Ic, Icon};
use crate::models::{ConflictPolicy, WatchStatus, WatchlistItem, WatchlistUpsert};
use crate::state::use_session;
use crate::Route;
use dioxus::prelude::*;

/// The card currently being dragged: `(series_id, notify)` — notify is preserved across the
/// status move so a drop only changes the column.
type Dragging = Option<(String, bool)>;

#[component]
pub fn Watchlist() -> Element {
    let session = use_session();
    let mut reload = use_signal(|| 0u32);
    // Drag state shared between the source cards and the drop-target columns.
    let dragging = use_signal(|| Dragging::None);
    let dragover = use_signal(|| Option::<WatchStatus>::None);

    let mut syncing = use_signal(|| false);
    let mut sync_msg: Signal<Option<Result<String, String>>> = use_signal(|| None);
    let sync_now = move |_| {
        if *syncing.peek() {
            return;
        }
        syncing.set(true);
        sync_msg.set(None);
        spawn(async move {
            if let Some(t) = session.token_value() {
                match api::anilist_pull(&t, ConflictPolicy::NewestWins).await {
                    Ok(_) => {
                        let pushed = api::anilist_push(&t, ConflictPolicy::NewestWins).await;
                        sync_msg.set(Some(pushed.map(|_| "Synced with AniList.".to_owned())));
                        reload += 1;
                    }
                    Err(e) => sync_msg.set(Some(Err(e))),
                }
            }
            syncing.set(false);
        });
    };

    let resource = use_resource(move || {
        let _ = reload.read();
        async move {
            match session.token_value() {
                Some(t) => api::watchlist(&t).await,
                None => Ok(Vec::new()),
            }
        }
    });

    if !session.is_authenticated() {
        return rsx! {
            h1 { class: "ik-page-title", "Watchlist" }
            SignInGate {}
        };
    }

    let board = match &*resource.read_unchecked() {
        None => rsx! {
            div { class: "ik-board",
                for _ in 0..5 {
                    div { class: "ik-col",
                        div { class: "ik-skeleton", style: "height:16px;width:60%;margin-bottom:12px;" }
                        div { class: "ik-skeleton", style: "height:64px;" }
                    }
                }
            }
        },
        Some(Err(e)) => {
            let msg = e.clone();
            rsx! {
                ErrorBox { message: msg, on_retry: move |()| reload += 1 }
            }
        }
        Some(Ok(items)) if items.is_empty() => rsx! {
            EmptyBox {
                message: "Your watchlist is empty. Find a series and add it to start tracking."
                    .to_string(),
            }
        },
        Some(Ok(items)) => {
            let items = items.clone();
            rsx! {
                div { class: "ik-board",
                    for status in WatchStatus::COLUMNS {
                        Column {
                            status,
                            items: items.iter().filter(|i| i.status == status).cloned().collect::<Vec<_>>(),
                            reload,
                            dragging,
                            dragover,
                        }
                    }
                }
            }
        }
    };

    rsx! {
        div { class: "ik-flex", style: "justify-content:space-between;align-items:flex-end;flex-wrap:wrap;gap:10px;",
            div {
                h1 { class: "ik-page-title", style: "margin-bottom:2px;", "Watchlist" }
                div { class: "ik-muted", style: "font-size:13px;",
                    "Drag a title between columns to change its status — or use the picker on each card."
                }
            }
            button { class: "ik-btn", disabled: *syncing.read(), onclick: sync_now,
                Ic { icon: Icon::CloudSync, size: 16 }
                if *syncing.read() { "Syncing…" } else { "Sync AniList" }
            }
        }
        match &*sync_msg.read() {
            Some(Ok(m)) => rsx! { p { style: "font-size:13px;color:var(--jade,#3DA88F);margin:8px 0 0;", "{m}" } },
            Some(Err(m)) => rsx! { p { style: "font-size:13px;color:var(--acc);margin:8px 0 0;", "Sync failed: {m}" } },
            None => rsx! {},
        }
        {board}
    }
}

/// Column accent + icon per the design's status→role-color map (§7.4).
fn column_style(status: WatchStatus) -> (Icon, &'static str) {
    match status {
        WatchStatus::Reading => (Icon::Fire, "var(--acc)"),
        WatchStatus::Planned => (Icon::Schedule, "#6FA8DC"),
        WatchStatus::Completed => (Icon::TaskAlt, "var(--jade-bright)"),
        WatchStatus::Paused => (Icon::PauseCircle, "#CBA43C"),
        WatchStatus::Dropped => (Icon::Cancel, "var(--muted)"),
    }
}

#[component]
fn Column(
    status: WatchStatus,
    items: Vec<WatchlistItem>,
    reload: Signal<u32>,
    dragging: Signal<Dragging>,
    dragover: Signal<Option<WatchStatus>>,
) -> Element {
    let session = use_session();
    let count = items.len();
    let (icon, color) = column_style(status);
    let is_over = *dragover.read() == Some(status);
    let class = if is_over { "ik-col dragover" } else { "ik-col" };

    // Drop: move the dragged card into this column (unless it is already here).
    let on_drop = move |evt: Event<DragData>| {
        evt.prevent_default();
        let mut dragover = dragover;
        dragover.set(None);
        let payload = dragging.read().clone();
        let mut dragging = dragging;
        dragging.set(None);
        if let Some((sid, notify)) = payload {
            let mut reload = reload;
            spawn(async move {
                if let Some(t) = session.token_value() {
                    let body = WatchlistUpsert { status, notify };
                    if api::set_watchlist(&t, &sid, &body).await.is_ok() {
                        reload += 1;
                    }
                }
            });
        }
    };

    rsx! {
        div {
            class: "{class}",
            ondragover: move |evt| {
                evt.prevent_default();
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
fn WatchCard(item: WatchlistItem, reload: Signal<u32>, dragging: Signal<Dragging>) -> Element {
    let session = use_session();
    let series_id = item.series_id.clone();
    let detail = use_resource({
        let id = series_id.clone();
        move || {
            let id = id.clone();
            async move { api::series_detail(&id).await }
        }
    });

    let (title, cover) = match &*detail.read_unchecked() {
        Some(Ok(d)) => (d.title.clone(), d.cover_url.clone()),
        _ => (short_id(&item.series_id), None),
    };

    let notify = item.notify;
    let status = item.status;

    let toggle_notify = {
        let sid = series_id.clone();
        let mut reload = reload;
        move |_| {
            let sid = sid.clone();
            spawn(async move {
                if let Some(t) = session.token_value() {
                    let body = WatchlistUpsert {
                        status,
                        notify: !notify,
                    };
                    if api::set_watchlist(&t, &sid, &body).await.is_ok() {
                        reload += 1;
                    }
                }
            });
        }
    };

    let move_status = {
        let sid = series_id.clone();
        let mut reload = reload;
        move |ev: Event<FormData>| {
            let sid = sid.clone();
            let new_status = parse_status(&ev.value());
            spawn(async move {
                if let Some(t) = session.token_value() {
                    let body = WatchlistUpsert {
                        status: new_status,
                        notify,
                    };
                    if api::set_watchlist(&t, &sid, &body).await.is_ok() {
                        reload += 1;
                    }
                }
            });
        }
    };

    let remove = {
        let sid = series_id.clone();
        let mut reload = reload;
        move |_| {
            let sid = sid.clone();
            spawn(async move {
                if let Some(t) = session.token_value() {
                    if api::remove_watchlist(&t, &sid).await.is_ok() {
                        reload += 1;
                    }
                }
            });
        }
    };

    let notify_class = if notify {
        "ik-pill vermilion"
    } else {
        "ik-pill"
    };
    let drag_sid = series_id.clone();

    rsx! {
        div {
            class: "ik-wl-card",
            draggable: true,
            ondragstart: move |_| {
                let mut dragging = dragging;
                dragging.set(Some((drag_sid.clone(), notify)));
            },
            ondragend: move |_| {
                let mut dragging = dragging;
                dragging.set(None);
            },
            Link { to: Route::Series { id: series_id.clone() }, class: "ik-flex",
                div { style: "width:40px;flex:none;",
                    Cover { url: cover, title: title.clone() }
                }
                div { class: "grow", style: "font-weight:600;font-size:13px;line-height:1.3;", "{title}" }
            }
            div { class: "ik-flex", style: "margin-top:8px;justify-content:space-between;gap:6px;",
                select {
                    class: "ik-input",
                    style: "padding:4px 6px;font-size:12px;width:auto;",
                    "aria-label": "Move to column",
                    value: "{status_value(status)}",
                    onchange: move_status,
                    for s in WatchStatus::COLUMNS {
                        option { value: "{status_value(s)}", selected: s == status, "{s.label()}" }
                    }
                }
                button {
                    class: "{notify_class}",
                    style: "cursor:pointer;background:none;",
                    title: "Toggle notifications",
                    onclick: toggle_notify,
                    if notify { "🔔 On" } else { "Notify" }
                }
                button {
                    class: "ik-pill",
                    style: "cursor:pointer;background:none;",
                    onclick: remove,
                    "Remove"
                }
            }
        }
    }
}

fn status_value(s: WatchStatus) -> &'static str {
    match s {
        WatchStatus::Reading => "reading",
        WatchStatus::Planned => "planned",
        WatchStatus::Completed => "completed",
        WatchStatus::Dropped => "dropped",
        WatchStatus::Paused => "paused",
    }
}

fn parse_status(v: &str) -> WatchStatus {
    match v {
        "planned" => WatchStatus::Planned,
        "completed" => WatchStatus::Completed,
        "dropped" => WatchStatus::Dropped,
        "paused" => WatchStatus::Paused,
        _ => WatchStatus::Reading,
    }
}

fn short_id(id: &str) -> String {
    id.get(0..8).unwrap_or(id).to_owned()
}
