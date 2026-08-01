//! The Tracking sidebar: conflict resolution, the progress editor, external trackers, the
//! per-series alert switch and the sync history for this title.
//!
//! Everything here is fetched, never assumed. Two blocks the design calls for have no endpoint
//! and are therefore **absent rather than stubbed**:
//!
//! - *Your notes* (score, rereads, started/finished dates, free text) — no personal-fields API
//!   exists, so four boxes that could only ever read `—` would be a dead control.
//!   TODO(api): needs `GET`/`PUT /v1/me/series/:id/notes`.
//! - *New source added* / *Series completed* alerts — the watchlist carries one `notify` flag,
//!   not three, so only the switch that is real is rendered.
//!   TODO(api): needs per-kind notification opt-ins on the watchlist entry.

use crate::api;
use crate::components::{async_view, SkeletonBlock};
use crate::hooks::{use_busy, use_outcome, use_reload, Reload};
use crate::i18n::use_i18n;
use crate::icons::{Ic, Icon};
use crate::models::*;
use crate::util::{monogram, rel_time};
use dioxus::prelude::*;
use progenitor_client::ResponseValue;

/// The conflict resolution that adopts the local value, as the sync service spells it.
const RESOLVE_LOCAL: &str = "local";
/// The conflict resolution that adopts the remote value.
const RESOLVE_REMOTE: &str = "remote";

/// One registered tracker and this reader's link state on it.
#[derive(Clone, PartialEq)]
struct Tracker {
    slug: String,
    name: String,
    status: SyncAccountStatus,
}

#[component]
pub(super) fn TrackingCard(
    series_id: SeriesId,
    /// `AniList` media id when this series is mapped, so the sidebar can name the mapping.
    anilist_id: Option<String>,
    /// This series' watchlist entry, when the reader tracks it.
    entry: Option<WatchlistItem>,
    authed: bool,
    /// Whole chapters indexed, for the `read / total` line above the stepper.
    total_chapters: i64,
    /// Bumped after a watchlist write.
    reload_wl: Reload,
    /// The screen's read-state signal, owned by [`super::Series`] and shared with the chapter
    /// list: tracked here so a per-chapter toggle refetches the frontier, bumped here so the
    /// stepper refetches the list. A `Reload` private to this card would only ever hear its
    /// own writes.
    reload_progress: Reload,
) -> Element {
    let api = api::use_api();
    let i18n = use_i18n();
    let reload_sync = use_reload();

    // Tracks the shared read-state signal, so the frontier refetches after a per-chapter
    // toggle in the list as well as after the stepper's own write.
    let progress = use_resource(move || {
        reload_progress.track();
        let client = api.client();
        async move {
            if !authed {
                return Ok(ProgressDto {
                    last_read_whole_number: 0.0,
                    last_read_part_number: None,
                });
            }
            client
                .get_progress()
                .series_id(series_id)
                .send()
                .await
                .map(ResponseValue::into_inner)
                .map_err(|e| api::friendly_error(i18n, e))
        }
    });

    // Only this series' conflicts: the account-wide inbox lives in Account → Sync.
    let conflicts = use_resource(move || {
        reload_sync.track();
        let client = api.client();
        async move {
            if !authed {
                return Vec::new();
            }
            client
                .sync_conflicts()
                .send()
                .await
                .map(ResponseValue::into_inner)
                .unwrap_or_default()
                .into_iter()
                .filter(|c| c.series_id == series_id.0)
                .collect::<Vec<_>>()
        }
    });

    // One request for the provider list, then one status probe each, concurrently.
    let trackers = use_resource(move || {
        reload_sync.track();
        let client = api.client();
        async move {
            if !authed {
                return Vec::new();
            }
            let Ok(providers) = client.sync_providers().send().await else {
                return Vec::new();
            };
            let probes = providers.into_inner().into_iter().map(|provider| {
                let client = client.clone();
                async move {
                    let status = client
                        .sync_status()
                        .provider(&provider.slug)
                        .send()
                        .await
                        .map(ResponseValue::into_inner)
                        .unwrap_or(SyncAccountStatus {
                            linked: false,
                            username: None,
                            last_synced_at: None,
                        });
                    Tracker {
                        slug: provider.slug,
                        name: provider.name,
                        status,
                    }
                }
            });
            futures_util::future::join_all(probes).await
        }
    });

    let history = use_resource(move || {
        reload_sync.track();
        reload_progress.track();
        let client = api.client();
        async move {
            if !authed {
                return Vec::new();
            }
            client
                .sync_history()
                .series_id(series_id.0)
                .send()
                .await
                .map(ResponseValue::into_inner)
                .unwrap_or_default()
        }
    });

    if !authed {
        return rsx! {
            div { class: "ik-sidebar-card",
                TrackingHead { conflicts: 0 }
                p { class: "ik-muted", style: "font-size:13px;margin:0;",
                    {i18n.t("series.signInToTrack")}
                }
            }
        };
    }

    let open_conflicts = conflicts.read_unchecked().as_ref().map_or(0, Vec::len);
    let tracker_rows = trackers.read_unchecked().clone().unwrap_or_default();
    let history_rows = history.read_unchecked().clone().unwrap_or_default();
    let conflict_rows = conflicts.read_unchecked().clone().unwrap_or_default();

    rsx! {
        div { class: "ik-sidebar-card",
            TrackingHead { conflicts: open_conflicts }

            for conflict in conflict_rows {
                ConflictCard {
                    key: "{conflict.id}",
                    conflict,
                    reload_sync,
                    reload_progress,
                }
            }

            div { class: "ik-track-sec",
                {
                    async_view(
                        &progress,
                        reload_progress,
                        || rsx! { SkeletonBlock { height: 76 } },
                        |value| rsx! {
                            // Keyed on the fetched frontier so a value that moved elsewhere —
                            // a read toggle in the chapter list — remounts the editor and
                            // discards its draft. Without the key the draft, which outlives
                            // its own write to keep the stepper from flickering, would go on
                            // masking every later server value.
                            ProgressEditor {
                                key: "{value.last_read_whole_number}",
                                series_id,
                                current: value.last_read_whole_number,
                                total: total_chapters,
                                reload_progress,
                            }
                        },
                    )
                }
            }

            div { class: "ik-track-sec",
                div { class: "ik-sec-lbl", style: "margin-bottom:8px;", {i18n.t("series.track.trackers")} }
                if tracker_rows.is_empty() {
                    p { class: "ik-muted", style: "font-size:12.5px;margin:0;",
                        {i18n.t("account.sync.noProviders")}
                    }
                } else {
                    div { class: "ik-listbox",
                        for tracker in tracker_rows {
                            TrackerRow { key: "{tracker.slug}", tracker, reload_sync }
                        }
                        if let Some(entry) = entry.clone() {
                            SyncOptOut { entry, reload_wl }
                        }
                        if let Some(anilist_id) = anilist_id.clone() {
                            div { class: "ik-listfoot",
                                span {
                                    {i18n.t("series.track.mappedTo")}
                                    " "
                                    a {
                                        class: "ik-mono ik-icon-link",
                                        style: "color:var(--muted);",
                                        href: "https://anilist.co/manga/{anilist_id}",
                                        target: "_blank",
                                        rel: "noopener",
                                        "#{anilist_id}"
                                    }
                                }
                            }
                        }
                    }
                }
            }

            if let Some(entry) = entry.clone() {
                AlertSwitches { entry, reload_wl }
            }

            div {
                div { class: "ik-sec-lbl", style: "margin-bottom:6px;", {i18n.t("series.track.history")} }
                if history_rows.is_empty() {
                    p { class: "ik-muted", style: "font-size:12.5px;margin:0;",
                        {i18n.t("series.track.historyEmpty")}
                    }
                } else {
                    div { class: "ik-timeline",
                        for row in history_rows.iter().take(6) {
                            div { key: "{row.id}",
                                span { class: "val", "{row.action}" }
                                " · "
                                {rel_time(i18n, Some(&row.created_at))}
                                " · {row.provider}"
                            }
                        }
                    }
                }
            }
        }
    }
}

#[component]
fn TrackingHead(conflicts: usize) -> Element {
    let i18n = use_i18n();
    let count = i64::try_from(conflicts).unwrap_or(i64::MAX);
    rsx! {
        div { class: "ik-flex", style: "gap:9px;margin-bottom:14px;",
            span { style: "display:flex;color:var(--acc);",
                Ic { icon: Icon::MenuBook, size: 17 }
            }
            h3 { style: "font-family:var(--font-display);font-weight:600;font-size:17px;margin:0;",
                {i18n.t("series.tracking")}
            }
            if conflicts > 0 {
                span { class: "ik-pill acc", style: "margin-left:auto;font-size:10px;",
                    {i18n.plural("series.track.conflicts", count, &[])}
                }
            }
        }
    }
}

/// One unresolved disagreement between local progress and a tracker, with the three ways out:
/// push local, take remote, or make "newest wins" the standing policy.
#[component]
fn ConflictCard(conflict: ConflictRow, reload_sync: Reload, reload_progress: Reload) -> Element {
    let api = api::use_api();
    let i18n = use_i18n();
    let busy = use_busy();
    let mut outcome = use_outcome();
    let id = conflict.id;
    let provider = conflict.provider.clone();

    let mut resolve = move |resolution: &'static str| {
        if !busy.claim() {
            return;
        }
        outcome.set(None);
        let client = api.client();
        spawn(async move {
            let body = ResolveConflict {
                resolution: resolution.to_owned(),
            };
            match client
                .sync_resolve_conflict()
                .id(id)
                .body(body)
                .send()
                .await
            {
                Ok(_) => {
                    reload_sync.bump();
                    reload_progress.bump();
                }
                Err(e) => outcome.set(Some(Err(api::friendly_error(i18n, e)))),
            }
            busy.release();
        });
    };

    let trust_newest = {
        let provider = provider.clone();
        move |_| {
            if !busy.claim() {
                return;
            }
            outcome.set(None);
            let provider = provider.clone();
            let client = api.client();
            spawn(async move {
                let body = SyncSettingsPatch {
                    auto_sync_enabled: None,
                    conflict_policy: Some(ConflictPolicy::NewestWins.into()),
                };
                match client
                    .sync_settings_patch()
                    .provider(provider)
                    .body(body)
                    .send()
                    .await
                {
                    Ok(_) => {
                        outcome.set(Some(Ok(i18n.t("series.track.policySaved"))));
                        reload_sync.bump();
                    }
                    Err(e) => outcome.set(Some(Err(api::friendly_error(i18n, e)))),
                }
                busy.release();
            });
        }
    };

    rsx! {
        div { class: "ik-conflict",
            div { class: "ik-flex", style: "gap:8px;margin-bottom:9px;",
                span { style: "display:flex;color:var(--acc3);",
                    Ic { icon: Icon::CloudSync, size: 15 }
                }
                span { style: "font-weight:600;font-size:13px;",
                    {
                        i18n.args(
                            "series.track.conflictHead",
                            &[("provider", &conflict.provider), ("field", &conflict.field)],
                        )
                    }
                }
            }
            div { class: "ik-flex", style: "gap:8px;margin-bottom:10px;align-items:stretch;",
                div { class: "ik-valbox",
                    div { class: "k", {i18n.t("series.track.here")} }
                    div { class: "v", "{conflict.local_value}" }
                }
                div { class: "ik-valbox",
                    div { class: "k", "{conflict.provider}" }
                    div { class: "v", "{conflict.remote_value}" }
                }
            }
            div { class: "ik-flex", style: "gap:7px;flex-wrap:wrap;",
                button {
                    class: "ik-btn primary sm",
                    disabled: busy.is_busy(),
                    onclick: move |_| resolve(RESOLVE_LOCAL),
                    {i18n.args("series.track.push", &[("value", &conflict.local_value)])}
                }
                button {
                    class: "ik-btn sm",
                    disabled: busy.is_busy(),
                    onclick: move |_| resolve(RESOLVE_REMOTE),
                    {i18n.args("series.track.take", &[("value", &conflict.remote_value)])}
                }
                button {
                    class: "ik-btn bare",
                    style: "font-size:12px;",
                    disabled: busy.is_busy(),
                    onclick: trust_newest,
                    {i18n.t("series.track.trustNewest")}
                }
            }
            crate::components::OutcomeLine { outcome: outcome.read().clone() }
        }
    }
}

/// The whole-chapter progress frontier: a stepper, and one button that marks everything up to
/// it read. Both write immediately and roll the optimistic value back if the write fails.
#[component]
fn ProgressEditor(
    series_id: SeriesId,
    current: f64,
    total: i64,
    reload_progress: Reload,
) -> Element {
    let api = api::use_api();
    let i18n = use_i18n();
    let busy = use_busy();
    // `None` means "show whatever the server last said"; `Some` is an unconfirmed local edit.
    // It survives its own write on purpose — clearing it there would snap the stepper back to
    // the pre-write number for the length of the refetch. What ends it is the refetched value:
    // the caller keys this component on it, so a frontier that moved (here, or by a toggle in
    // the chapter list) remounts the editor and takes the draft with it.
    let mut draft = use_signal(|| Option::<f64>::None);
    let mut error = use_signal(|| Option::<String>::None);

    let value = draft.read().unwrap_or(current);
    // The frontier is a whole-chapter count, so it is displayed and stepped as an integer.
    let shown = crate::util::chapter_number(value);
    let ceiling = {
        #[expect(
            clippy::cast_precision_loss,
            reason = "a whole-chapter count, far inside f64's exact integer range"
        )]
        {
            total as f64
        }
    };

    let mut write = move |next: f64| {
        if !busy.claim() {
            return;
        }
        draft.set(Some(next));
        error.set(None);
        let client = api.client();
        spawn(async move {
            let outcome = client
                .put_progress()
                .series_id(series_id)
                .body(ProgressUpdate {
                    last_read_whole_number: next,
                })
                .send()
                .await;
            match outcome {
                Ok(_) => reload_progress.bump(),
                Err(e) => {
                    // Roll back to the server's value rather than leaving a number that was
                    // never persisted sitting in the editor.
                    draft.set(None);
                    error.set(Some(api::friendly_error(i18n, e)));
                    reload_progress.bump();
                }
            }
            busy.release();
        });
    };

    let mark_to = move |_| {
        if !busy.claim() {
            return;
        }
        error.set(None);
        let target = draft.peek().unwrap_or(current);
        let client = api.client();
        spawn(async move {
            match client
                .mark_read_to()
                .series_id(series_id)
                .body(MarkReadTo { number: target })
                .send()
                .await
            {
                Ok(_) => reload_progress.bump(),
                Err(e) => error.set(Some(api::friendly_error(i18n, e))),
            }
            busy.release();
        });
    };

    rsx! {
        div { class: "ik-flex", style: "align-items:baseline;gap:8px;margin-bottom:8px;",
            span { class: "ik-sec-lbl", {i18n.t("series.track.progress")} }
            span { class: "ik-mono ik-muted", style: "margin-left:auto;font-size:11.5px;",
                "{shown} / {total}"
            }
        }
        div { class: "ik-flex", style: "gap:8px;",
            div { class: "ik-stepper",
                button {
                    disabled: busy.is_busy() || value <= 0.0,
                    title: i18n.t("series.track.decrement"),
                    onclick: move |_| write((value - 1.0).max(0.0)),
                    Ic { icon: Icon::Remove, size: 15 }
                }
                span { class: "val", "{shown}" }
                button {
                    disabled: busy.is_busy() || (total > 0 && value >= ceiling),
                    title: i18n.t("series.track.increment"),
                    onclick: move |_| write(value + 1.0),
                    Ic { icon: Icon::Add, size: 15 }
                }
            }
            button {
                class: "ik-btn sm",
                style: "flex:1;justify-content:center;",
                disabled: busy.is_busy(),
                onclick: mark_to,
                {i18n.t("series.track.markUpTo")}
            }
        }
        if let Some(message) = error.read().clone() {
            crate::components::ErrorLine { message }
        }
    }
}

/// One external tracker: its link state, when it last synced, and the link/unlink action.
#[component]
fn TrackerRow(tracker: Tracker, reload_sync: Reload) -> Element {
    let api = api::use_api();
    let i18n = use_i18n();
    let busy = use_busy();
    let linked = tracker.status.linked;
    let tile = monogram(&tracker.name);

    let sub = if linked {
        let when = rel_time(i18n, tracker.status.last_synced_at.as_deref());
        match tracker.status.username.clone() {
            Some(user) => format!("{user} · {when}"),
            None => when,
        }
    } else {
        i18n.t("series.track.notLinked")
    };

    let toggle = {
        let slug = tracker.slug.clone();
        move |_| {
            if !busy.claim() {
                return;
            }
            let slug = slug.clone();
            let client = api.client();
            spawn(async move {
                if linked {
                    let _ = client.sync_disconnect().provider(slug).send().await;
                    reload_sync.bump();
                } else if let Ok(response) = client.sync_authorize_url().provider(slug).send().await
                {
                    // A full-page navigation, not a router push: the consent screen lives on
                    // the provider's origin.
                    crate::browser::navigate_to(&response.into_inner().url);
                }
                busy.release();
            });
        }
    };

    rsx! {
        div { class: "ik-listrow",
            span { class: if linked { "ik-mono-tile lg jade" } else { "ik-mono-tile lg" }, "{tile}" }
            div { style: "min-width:0;",
                div { style: "font-weight:600;font-size:13px;", "{tracker.name}" }
                div { class: "ik-mono", style: "font-size:10.5px;color:var(--muted);margin-top:1px;",
                    "{sub}"
                }
            }
            button {
                class: if linked { "ik-btn xs acc" } else { "ik-btn xs" },
                style: "margin-left:auto;",
                disabled: busy.is_busy(),
                onclick: toggle,
                if linked {
                    {i18n.t("series.track.unlink")}
                } else {
                    {i18n.t("series.track.link")}
                }
            }
        }
    }
}

/// The per-title sync opt-out: whether this series is pushed to linked trackers at all.
///
/// It rides with the tracker list rather than the alert switches because it governs *those*
/// rows — excluding a title is a statement about sync, not about notifications.
#[component]
fn SyncOptOut(entry: WatchlistItem, reload_wl: Reload) -> Element {
    let api = api::use_api();
    let i18n = use_i18n();
    let busy = use_busy();
    let series_id = entry.series_id;
    let excluded = entry.sync_excluded;

    let toggle = move |_| {
        if !busy.claim() {
            return;
        }
        let client = api.client();
        spawn(async move {
            if client
                .put_sync_excluded()
                .series_id(series_id)
                .body(SyncExcluded {
                    excluded: !excluded,
                })
                .send()
                .await
                .is_ok()
            {
                reload_wl.bump();
            }
            busy.release();
        });
    };

    rsx! {
        div { class: "ik-listrow",
            span { style: "font-size:12.5px;color:var(--text-2);", {i18n.t("series.track.syncThis")} }
            button {
                class: if excluded { "ik-switch sm" } else { "ik-switch sm on" },
                style: "margin-left:auto;",
                disabled: busy.is_busy(),
                "aria-pressed": if excluded { "false" } else { "true" },
                "aria-label": i18n.t("series.track.syncThis"),
                onclick: toggle,
            }
        }
    }
}

/// The per-series alert switches. Only the one the watchlist actually stores is offered — see
/// the module docs for the two the design draws that have no field behind them.
#[component]
fn AlertSwitches(entry: WatchlistItem, reload_wl: Reload) -> Element {
    let api = api::use_api();
    let i18n = use_i18n();
    let busy = use_busy();
    let series_id = entry.series_id;
    let status = entry.status;
    let notify = entry.notify;

    let toggle = move |_| {
        if !busy.claim() {
            return;
        }
        let client = api.client();
        spawn(async move {
            let body = WatchlistUpsert {
                status: Some(status),
                notify: Some(!notify),
            };
            if client
                .put_watchlist()
                .series_id(series_id)
                .body(body)
                .send()
                .await
                .is_ok()
            {
                reload_wl.bump();
            }
            busy.release();
        });
    };

    rsx! {
        div { class: "ik-track-sec",
            div { class: "ik-sec-lbl", style: "margin-bottom:6px;", {i18n.t("series.track.alerts")} }
            div { class: "ik-alertrow",
                span { {i18n.t("series.track.newChapter")} }
                button {
                    class: if notify { "ik-switch sm on" } else { "ik-switch sm" },
                    style: "margin-left:auto;",
                    disabled: busy.is_busy(),
                    "aria-pressed": if notify { "true" } else { "false" },
                    "aria-label": i18n.t("series.track.newChapter"),
                    onclick: toggle,
                }
            }
        }
    }
}
