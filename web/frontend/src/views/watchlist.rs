//! Watchlist (§17.2.4) — status columns (Reading / Planned / Completed / Paused /
//! Dropped) with a per-title notify toggle and a status move control. (Native drag-between
//! columns is a follow-up; the status <select> is the accessible equivalent for now.)

use crate::api;
use crate::components::{Cover, EmptyBox, ErrorBox, SignInGate};
use crate::models::{WatchStatus, WatchlistItem, WatchlistUpsert};
use crate::state::use_session;
use crate::Route;
use dioxus::prelude::*;

#[component]
pub fn Watchlist() -> Element {
    let session = use_session();
    let mut reload = use_signal(|| 0u32);

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

    let body = match &*resource.read_unchecked() {
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
                        }
                    }
                }
            }
        }
    };

    rsx! {
        h1 { class: "ik-page-title", "Watchlist" }
        {body}
    }
}

#[component]
fn Column(status: WatchStatus, items: Vec<WatchlistItem>, reload: Signal<u32>) -> Element {
    let count = items.len();
    rsx! {
        div { class: "ik-col",
            h3 {
                span { "{status.label()}" }
                span { class: "ik-mono ik-muted", "{count}" }
            }
            for item in items {
                WatchCard { key: "{item.series_id}", item, reload }
            }
        }
    }
}

#[component]
fn WatchCard(item: WatchlistItem, reload: Signal<u32>) -> Element {
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

    rsx! {
        div { style: "border:1px solid var(--border);border-radius:10px;padding:8px;margin-bottom:8px;",
            Link { to: Route::Series { id: series_id.clone() }, class: "ik-flex",
                div { style: "width:36px;flex:none;",
                    Cover { url: cover, title: title.clone() }
                }
                div { class: "grow", style: "font-weight:600;font-size:13px;", "{title}" }
            }
            div { class: "ik-flex", style: "margin-top:8px;justify-content:space-between;",
                select {
                    class: "ik-input",
                    style: "padding:4px 6px;font-size:12px;width:auto;",
                    value: "{status_value(status)}",
                    onchange: move_status,
                    for s in WatchStatus::COLUMNS {
                        option { value: "{status_value(s)}", selected: s == status, "{s.label()}" }
                    }
                }
                button {
                    class: "{notify_class}",
                    style: "cursor:pointer;background:none;",
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
