//! Sync & integrations — one card per registered external tracker.
//!
//! Data-driven from `GET /v1/me/sync/providers` rather than a hardcoded `AniList` block, so a
//! second provider needs no frontend change. Every claim on this screen comes from
//! `GET /v1/me/sync/{provider}/status`: nothing is reported as connected while it is not.

use crate::api;
use crate::components::{
    async_list, async_view, use_step_up_gate, EmptyBox, ErrorLine, OutcomeLine, PanelCard,
    SkeletonBlock, StepUpGuard,
};
use crate::hooks::{use_busy, use_outcome, use_reload, Reload};
use crate::i18n::use_i18n;
use crate::icons::{Ic, Icon};
use crate::models::*;
use crate::state::use_session;
use crate::util::rel_time;
use dioxus::prelude::*;
use inkstone_ui::{Button, ToggleButton, Tone};
use progenitor_client::ResponseValue;
#[component]
pub(crate) fn SyncPanel() -> Element {
    let session = use_session();
    let i18n = use_i18n();
    let api = api::use_api();
    let reload = use_reload();

    let providers = use_resource(move || {
        reload.track();
        let client = api.client();
        let authed = session.is_authenticated();
        async move {
            if !authed {
                return Ok(Vec::new());
            }
            client
                .sync_providers()
                .send()
                .await
                .map(ResponseValue::into_inner)
                .map_err(|e| api::friendly_error(i18n, e))
        }
    });

    rsx! {
        PanelCard { icon: Icon::CloudDone, title: i18n.t("account.tab.sync"),
            {
                async_list(
                    &providers,
                    reload,
                    || rsx! { SkeletonBlock { height: 80 } },
                    &i18n.t("account.sync.noProviders"),
                    |list| rsx! {
                        for provider in list.iter().cloned() {
                            ProviderSyncCard {
                                key: "{provider.slug}",
                                slug: provider.slug,
                                name: provider.name,
                            }
                        }
                    },
                )
            }
        }
    }
}

/// One provider's connect/disconnect, automatic-sync settings and manual pull/push.
#[component]
fn ProviderSyncCard(slug: String, name: String) -> Element {
    let session = use_session();
    let i18n = use_i18n();
    let api = api::use_api();
    let gate = use_step_up_gate();
    let reload = use_reload();
    let busy = use_busy();
    let mut outcome = use_outcome();
    let mut show_conflicts = use_signal(|| false);

    let status = use_resource({
        let slug = slug.clone();
        move || {
            reload.track();
            let slug = slug.clone();
            let client = api.client();
            let authed = session.is_authenticated();
            async move {
                if !authed {
                    return Ok(SyncAccountStatus {
                        linked: false,
                        username: None,
                        last_synced_at: None,
                    });
                }
                client
                    .sync_status()
                    .provider(slug)
                    .send()
                    .await
                    .map(ResponseValue::into_inner)
                    .map_err(|e| api::friendly_error(i18n, e))
            }
        }
    });

    // The account's persisted automatic-sync settings (design v2 §B.6/§B.8). Must stay generated
    // from the producer's own type — a hand-written struct drifts from the service and silently
    // falls back to hardcoded defaults.
    let settings = use_resource({
        let slug = slug.clone();
        move || {
            reload.track();
            let slug = slug.clone();
            let client = api.client();
            let authed = session.is_authenticated();
            async move {
                if !authed {
                    return None;
                }
                client
                    .sync_settings()
                    .provider(slug)
                    .send()
                    .await
                    .ok()
                    .map(ResponseValue::into_inner)
            }
        }
    });

    let policy = settings
        .read_unchecked()
        .as_ref()
        .and_then(|s| s.as_ref())
        .map_or(ConflictPolicy::NewestWins, |s| s.conflict_policy);
    let auto_sync = settings
        .read_unchecked()
        .as_ref()
        .and_then(|s| s.as_ref())
        .is_none_or(|s| s.auto_sync_enabled);
    let pending = settings
        .read_unchecked()
        .as_ref()
        .and_then(|s| s.as_ref())
        .map_or(0, |s| s.pending_conflicts);
    let linked = matches!(&*status.read_unchecked(), Some(Ok(status)) if status.linked);

    let patch_settings = {
        let slug = slug.clone();
        move |patch: SyncSettingsPatch| {
            let slug = slug.clone();
            let client = api.client();
            spawn(async move {
                let _ = client
                    .sync_settings_patch()
                    .provider(slug)
                    .body(patch)
                    .send()
                    .await;
                reload.bump();
            });
        }
    };

    let toggle_auto = {
        let patch_settings = patch_settings.clone();
        move |_| {
            patch_settings(SyncSettingsPatch {
                auto_sync_enabled: Some(!auto_sync),
                conflict_policy: None,
            });
        }
    };

    let connect = {
        let slug = slug.clone();
        move |_| {
            let slug = slug.clone();
            let client = api.client();
            spawn(async move {
                match client.sync_authorize_url().provider(slug).send().await {
                    Ok(response) => {
                        // A full-page navigation, not a router push: the consent screen lives
                        // on the provider's origin.
                        crate::platform::navigate_to(&response.into_inner().url);
                    }
                    Err(e) => outcome.set(Some(Err(api::friendly_error(i18n, e)))),
                }
            });
        }
    };

    let disconnect = {
        let slug = slug.clone();
        let name = name.clone();
        move |_| {
            let (slug, name) = (slug.clone(), name.clone());
            gate.attempt(move || {
                if !busy.claim() {
                    return;
                }
                outcome.set(None);
                let (slug, name) = (slug.clone(), name.clone());
                // Elevated: unlinking a tracker is an account change, so the API demands a second
                // factor and answers `403` until it has one.
                let client = gate.client(api);
                spawn(async move {
                    match client.sync_disconnect().provider(slug).send().await {
                        Ok(_) => {
                            outcome.set(Some(Ok(
                                i18n.args("account.sync.disconnected", &[("provider", &name)])
                            )));
                            reload.bump();
                        }
                        Err(e) => {
                            if !gate.refused(api::Refusal::of(&e)) {
                                outcome.set(Some(Err(api::friendly_error(i18n, e))));
                            }
                        }
                    }
                    busy.release();
                });
            });
        }
    };

    // Pull and push get their own closures rather than one parameterised by direction: a
    // shared `FnMut` capturing the non-`Copy` slug cannot be moved into both buttons.
    let pull = {
        let slug = slug.clone();
        move |_| {
            if !busy.claim() {
                return;
            }
            outcome.set(None);
            let slug = slug.clone();
            let client = api.client();
            spawn(async move {
                let opts = SyncOpts {
                    policy: Some(policy),
                };
                match client.sync_pull().provider(slug).body(opts).send().await {
                    Ok(_) => {
                        outcome.set(Some(Ok(i18n.t("account.sync.pullStarted"))));
                        reload.bump();
                    }
                    Err(e) => outcome.set(Some(Err(api::friendly_error(i18n, e)))),
                }
                busy.release();
            });
        }
    };

    let push = {
        let slug = slug.clone();
        move |_| {
            if !busy.claim() {
                return;
            }
            outcome.set(None);
            let slug = slug.clone();
            let client = api.client();
            spawn(async move {
                let opts = SyncOpts {
                    policy: Some(policy),
                };
                match client.sync_push().provider(slug).body(opts).send().await {
                    Ok(_) => {
                        outcome.set(Some(Ok(i18n.t("account.sync.pushStarted"))));
                        reload.bump();
                    }
                    Err(e) => outcome.set(Some(Err(api::friendly_error(i18n, e)))),
                }
                busy.release();
            });
        }
    };

    let card_name = name.clone();
    let body = async_view(
        &status,
        reload,
        || rsx! { SkeletonBlock { height: 80 } },
        |status| {
            if !status.linked {
                return rsx! {
                    div { class: "ik-flex", style: "gap:14px;",
                        div { class: "ik-source-tile", style: "width:46px;height:46px;",
                            Ic { icon: Icon::CloudOff, size: 22 }
                        }
                        div { class: "grow",
                            div { style: "font-weight:700;font-size:16px;", "{card_name}" }
                            div { class: "ik-muted", style: "font-size:13px;", {i18n.t("series.notConnected")} }
                        }
                        Button {
                            tone: Tone::Primary,
                            on_click: connect,
                            {i18n.args("account.sync.connect", &[("provider", &card_name)])}
                        }
                    }
                };
            }

            let username = status.username.clone().unwrap_or_else(|| {
                i18n.args("account.sync.anonymousUser", &[("provider", &card_name)])
            });
            let last_sync = rel_time(i18n, status.last_synced_at.as_deref());

            rsx! {
                div { class: "ik-flex", style: "gap:14px;margin-bottom:16px;",
                    div { class: "ik-source-tile", style: "width:46px;height:46px;",
                        Ic { icon: Icon::CloudDone, size: 22 }
                    }
                    div { class: "grow",
                        div { style: "font-weight:700;font-size:16px;", "{card_name}" }
                        div { class: "ik-flex", style: "gap:5px;font-size:13px;color:var(--jade-bright);",
                            Ic { icon: Icon::CloudDone, size: 15 }
                            {
                                i18n.args(
                                    "account.sync.connectedAs",
                                    &[("user", &username), ("when", &last_sync)],
                                )
                            }
                        }
                    }
                    Button {
                        disabled: busy.is_busy(),
                        on_click: disconnect,
                        {i18n.t("account.sync.disconnect")}
                    }
                }
                div { class: "ik-row", style: "margin-bottom:12px;",
                    div { class: "grow",
                        div { style: "font-weight:600;font-size:13px;", {i18n.t("account.sync.auto")} }
                        div { class: "ik-muted", style: "font-size:12px;",
                            {i18n.t("account.sync.autoHint")}
                        }
                    }
                    ToggleButton {
                        on: auto_sync,
                        on_toggle: toggle_auto,
                        if auto_sync {
                            {i18n.t("common.on")}
                        } else {
                            {i18n.t("common.off")}
                        }
                    }
                }
                if pending > 0 {
                    div { class: "ik-row", style: "margin-bottom:12px;",
                        span { class: "grow", style: "font-size:13px;color:var(--acc);",
                            {i18n.plural("account.sync.pending", pending, &[])}
                        }
                        Button {
                            on_click: move |_| show_conflicts.set(true),
                            {i18n.t("account.sync.reviewConflicts")}
                        }
                    }
                }
                div { class: "ik-subhead", style: "margin-bottom:8px;",
                    {i18n.args("account.sync.policyHeading", &[("provider", &card_name)])}
                }
                div { class: "ik-chips",
                    for option in ConflictPolicy::all().iter().copied() {
                        {
                            let patch_settings = patch_settings.clone();
                            rsx! {
                                button {
                                    key: "{option}",
                                    class: if policy == option { "ik-chip active" } else { "ik-chip" },
                                    "aria-pressed": policy == option,
                                    onclick: move |_| patch_settings(SyncSettingsPatch {
                                        auto_sync_enabled: None,
                                        conflict_policy: Some(option),
                                    }),
                                    {i18n.t(option.label_key())}
                                }
                            }
                        }
                    }
                }
                div { class: "ik-flex", style: "gap:10px;flex-wrap:wrap;margin-top:12px;",
                    Button {
                        disabled: busy.is_busy(),
                        on_click: pull,
                        Ic { icon: Icon::CloudSync, size: 16 }
                        {i18n.args("account.sync.pull", &[("provider", &card_name)])}
                    }
                    Button {
                        disabled: busy.is_busy(),
                        on_click: push,
                        Ic { icon: Icon::CloudSync, size: 16 }
                        {i18n.args("account.sync.push", &[("provider", &card_name)])}
                    }
                }
            }
        },
    );

    rsx! {
        {body}
        OutcomeLine { outcome: outcome.read().clone() }
        StepUpGuard { gate }
        if *show_conflicts.read() {
            ConflictInbox { provider: slug.clone(), show: show_conflicts, parent_reload: reload }
        }
        if linked {
            SyncHistory { provider: slug.clone(), refresh: reload }
        }
    }
}

/// A compact "recent sync activity" log for one provider (design v2 §B.4/§B.6): what the
/// automatic engine actually did, so "automatic" never means "invisible".
#[component]
fn SyncHistory(provider: String, refresh: Reload) -> Element {
    let session = use_session();
    let i18n = use_i18n();
    let api = api::use_api();

    let entries = use_resource(move || {
        refresh.track();
        let provider = provider.clone();
        let client = api.client();
        let authed = session.is_authenticated();
        async move {
            if !authed {
                return Ok(Vec::new());
            }
            client
                .sync_history()
                .provider(provider)
                .send()
                .await
                .map(|r| r.into_inner().into_iter().take(HISTORY_ROWS).collect())
                .map_err(|e| api::friendly_error(i18n, e))
        }
    });

    rsx! {
        div { class: "ik-panel", style: "max-width:560px;margin-top:14px;",
            div { class: "ik-flex", style: "margin-bottom:12px;",
                Ic { icon: Icon::CloudSync, size: 16 }
                strong { {i18n.t("account.sync.history")} }
            }
            {
                async_list(
                    &entries,
                    refresh,
                    || rsx! { SkeletonBlock { height: 60 } },
                    &i18n.t("account.sync.historyEmpty"),
                    |rows| rsx! {
                        for row in rows.iter() {
                            div { class: "ik-row", key: "{row.id}",
                                div { class: "grow",
                                    div { style: "font-weight:600;font-size:13px;", "{row.series_title}" }
                                    div { class: "ik-mono ik-muted", style: "font-size:11px;",
                                        "{row.action} · {rel_time(i18n, Some(&row.created_at))}"
                                    }
                                }
                            }
                        }
                    },
                )
            }
        }
    }
}

/// How many history rows the activity log shows. The endpoint pages at 50; this panel is a
/// glance, not an audit trail.
const HISTORY_ROWS: usize = 8;

/// The reader-facing conflict inbox (design v2 §B.8): every pending conflict for `provider`,
/// each with a plain-language "keep mine / take theirs" choice.
#[component]
fn ConflictInbox(provider: String, show: Signal<bool>, parent_reload: Reload) -> Element {
    let session = use_session();
    let i18n = use_i18n();
    let api = api::use_api();
    let reload = use_reload();

    let conflicts = use_resource(move || {
        reload.track();
        let client = api.client();
        let authed = session.is_authenticated();
        async move {
            if !authed {
                return Ok(Vec::new());
            }
            client
                .sync_conflicts()
                .send()
                .await
                .map(ResponseValue::into_inner)
                .map_err(|e| api::friendly_error(i18n, e))
        }
    });

    let provider_filter = provider.clone();
    rsx! {
        div { class: "ik-panel", style: "max-width:560px;margin-top:14px;",
            div { class: "ik-flex", style: "margin-bottom:12px;",
                strong { class: "grow", {i18n.t("account.sync.conflictsHeading")} }
                Button {
                    on_click: move |_| show.set(false),
                    {i18n.t("common.close")}
                }
            }
            {
                async_view(
                    &conflicts,
                    reload,
                    || rsx! { SkeletonBlock { height: 60 } },
                    |all| {
                        let rows: Vec<&ConflictRow> = all
                            .iter()
                            .filter(|c| c.provider == provider_filter)
                            .collect();
                        if rows.is_empty() {
                            return rsx! {
                                EmptyBox { message: i18n.t("account.sync.conflictsEmpty") }
                            };
                        }
                        rsx! {
                            for conflict in rows {
                                ConflictRowView {
                                    key: "{conflict.id}",
                                    conflict: conflict.clone(),
                                    reload,
                                    parent_reload,
                                }
                            }
                        }
                    },
                )
            }
        }
    }
}

/// One conflict: the disagreeing values plus the two resolutions.
#[component]
fn ConflictRowView(conflict: ConflictRow, reload: Reload, parent_reload: Reload) -> Element {
    let api = api::use_api();
    let i18n = use_i18n();
    let busy = use_busy();
    let mut error = use_signal(|| Option::<String>::None);
    let id = conflict.id;

    let mut resolve = move |resolution: &'static str| {
        if !busy.claim() {
            return;
        }
        error.set(None);
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
                    // Refresh both the inbox and the parent card, whose pending badge is now
                    // one lower.
                    reload.bump();
                    parent_reload.bump();
                }
                Err(e) => error.set(Some(api::friendly_error(i18n, e))),
            }
            busy.release();
        });
    };

    rsx! {
        div { class: "ik-row",
            div { class: "grow",
                div { style: "font-weight:600;font-size:13px;", "{conflict.series_title}" }
                div { class: "ik-mono ik-muted", style: "font-size:11px;",
                    {
                        i18n.args(
                            "account.sync.conflictValues",
                            &[
                                ("field", &conflict.field),
                                ("local", &conflict.local_value),
                                ("remote", &conflict.remote_value),
                            ],
                        )
                    }
                }
                if let Some(message) = error.read().clone() {
                    ErrorLine { message }
                }
            }
            Button {
                disabled: busy.is_busy(),
                on_click: move |_| resolve("local"),
                {i18n.t("account.sync.keepMine")}
            }
            Button {
                disabled: busy.is_busy(),
                on_click: move |_| resolve("remote"),
                {i18n.t("account.sync.takeTheirs")}
            }
        }
    }
}
