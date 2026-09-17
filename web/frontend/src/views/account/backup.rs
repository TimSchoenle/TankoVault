//! Watchlist backup — download the reader's watchlist as a portable file, restore one, and see
//! which restored entries are still waiting for their series to be crawled.
//!
//! A restore is always previewed first: the server computes the import against the current
//! watchlist and rolls it back, and only that exact file and those exact options can then be
//! applied. `replace` can empty a library in one call, so the reader sees what it would remove
//! before the step-up prompt asks them to confirm it is them.

use crate::api;
use crate::components::{
    async_view, use_step_up_gate, OutcomeLine, PanelCard, SkeletonRows, StepUpGuard,
};
use crate::hooks::{use_busy, use_outcome, use_reload, Reload};
use crate::i18n::{use_i18n, Translator};
use crate::icons::Icon;
use crate::state::use_session;
use crate::util::iso_date;
use crate::wire::types::{WatchlistImportMode, WatchlistImportReport, WatchlistPending};
use dioxus::prelude::*;
use inkstone_ui::{Button, Tone};
use progenitor_client::ResponseValue;

/// The largest file offered to the server, matching its import body limit. Checked here so a
/// wrong file is refused before it is read into memory and uploaded.
const MAX_BACKUP_BYTES: u64 = 8 * 1024 * 1024;

#[component]
pub(crate) fn BackupPanel() -> Element {
    let pending_reload = use_reload();
    rsx! {
        ExportCard {}
        ImportCard { pending_reload }
        PendingCard { reload: pending_reload }
    }
}

/// Download the watchlist as a backup file.
#[component]
fn ExportCard() -> Element {
    let i18n = use_i18n();
    let api = api::use_api();
    let busy = use_busy();
    let mut outcome = use_outcome();

    let download = move |_| {
        if !busy.claim() {
            return;
        }
        outcome.set(None);
        let client = api.client();
        spawn(async move {
            let result = match client.export_watchlist().send().await {
                Ok(response) => match serde_json::to_string_pretty(&response.into_inner()) {
                    Ok(json) => crate::platform::save_text_file(
                        "tankovault-watchlist.json",
                        "application/json",
                        &json,
                    )
                    .await
                    .map(|()| i18n.t("account.backup.export.done"))
                    .map_err(|key| i18n.t(key)),
                    Err(_) => Err(i18n.t("account.backup.export.failed")),
                },
                Err(e) => Err(api::friendly_error(i18n, e)),
            };
            outcome.set(Some(result));
            busy.release();
        });
    };

    rsx! {
        PanelCard { icon: Icon::Download, title: i18n.t("account.backup.export.title"),
            p { class: "ik-muted", style: "font-size:13px;margin-top:0;",
                {i18n.t("account.backup.export.intro")}
            }
            OutcomeLine { outcome: outcome.read().clone() }
            Button {
                tone: Tone::Primary,
                style: "margin-top:12px;",
                disabled: busy.is_busy(),
                on_click: download,
                if busy.is_busy() {
                    {i18n.t("account.backup.export.preparing")}
                } else {
                    {i18n.t("account.backup.export.cta")}
                }
            }
        }
    }
}

fn mode_token(mode: WatchlistImportMode) -> &'static str {
    match mode {
        WatchlistImportMode::Merge => "merge",
        WatchlistImportMode::Overwrite => "overwrite",
        WatchlistImportMode::Replace => "replace",
    }
}

fn parse_mode(token: &str) -> WatchlistImportMode {
    match token {
        "overwrite" => WatchlistImportMode::Overwrite,
        "replace" => WatchlistImportMode::Replace,
        _ => WatchlistImportMode::Merge,
    }
}

/// The file and options a preview was computed for. Applying requires these to be unchanged, so
/// the import that runs is the one the reader looked at.
#[derive(Clone)]
struct Previewed {
    document: serde_json::Value,
    mode: WatchlistImportMode,
    match_titles: bool,
    report: WatchlistImportReport,
}

/// Choose a backup file, preview what restoring it would do, then apply it.
#[component]
fn ImportCard(pending_reload: Reload) -> Element {
    let i18n = use_i18n();
    let api = api::use_api();
    let gate = use_step_up_gate();
    let busy = use_busy();
    let mut outcome = use_outcome();
    let mut document = use_signal(|| None::<serde_json::Value>);
    let mut file_name = use_signal(String::new);
    let mut mode = use_signal(|| WatchlistImportMode::Merge);
    let mut match_titles = use_signal(|| false);
    let mut previewed = use_signal(|| None::<Previewed>);
    let mut applied = use_signal(|| None::<WatchlistImportReport>);

    let choose = move |event: FormEvent| {
        previewed.set(None);
        applied.set(None);
        outcome.set(None);
        document.set(None);
        let Some(file) = event.files().into_iter().next() else {
            return;
        };
        file_name.set(file.name());
        if file.size() > MAX_BACKUP_BYTES {
            outcome.set(Some(Err(i18n.t("account.backup.import.tooLarge"))));
            return;
        }
        spawn(async move {
            let parsed = file
                .read_string()
                .await
                .ok()
                .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok());
            match parsed {
                Some(value @ serde_json::Value::Object(_)) => document.set(Some(value)),
                _ => outcome.set(Some(Err(i18n.t("account.backup.import.notJson")))),
            }
        });
    };

    let preview = move |_| {
        let Some(body) = document.peek().clone() else {
            return;
        };
        if !busy.claim() {
            return;
        }
        outcome.set(None);
        applied.set(None);
        let chosen_mode = *mode.peek();
        let titles = *match_titles.peek();
        let client = api.client();
        spawn(async move {
            match client
                .preview_watchlist_import()
                .mode(chosen_mode)
                .match_titles(titles)
                .body(body.clone())
                .send()
                .await
            {
                Ok(response) => previewed.set(Some(Previewed {
                    document: body,
                    mode: chosen_mode,
                    match_titles: titles,
                    report: response.into_inner(),
                })),
                Err(e) => {
                    previewed.set(None);
                    outcome.set(Some(Err(api::friendly_error(i18n, e))));
                }
            }
            busy.release();
        });
    };

    let apply = move |_| {
        gate.attempt(move || {
            let Some(plan) = previewed.peek().clone() else {
                return;
            };
            if !busy.claim() {
                return;
            }
            outcome.set(None);
            // Elevated: `overwrite` rewrites every entry the file names and `replace` removes
            // every one it lacks.
            let client = gate.client(api);
            spawn(async move {
                match client
                    .import_watchlist()
                    .mode(plan.mode)
                    .match_titles(plan.match_titles)
                    .body(plan.document)
                    .send()
                    .await
                {
                    Ok(response) => {
                        previewed.set(None);
                        applied.set(Some(response.into_inner()));
                        outcome.set(Some(Ok(i18n.t("account.backup.import.done"))));
                        pending_reload.bump();
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
    };

    let current_mode = *mode.read();
    let has_file = document.read().is_some();
    // The preview only stands for the options it was computed with.
    let preview_current = previewed
        .read()
        .as_ref()
        .is_some_and(|p| p.mode == current_mode && p.match_titles == *match_titles.read());

    rsx! {
        PanelCard { icon: Icon::History, title: i18n.t("account.backup.import.title"),
            p { class: "ik-muted", style: "font-size:13px;margin-top:0;",
                {i18n.t("account.backup.import.intro")}
            }
            div { class: "ik-field",
                label { r#for: "tv-backup-file", {i18n.t("account.backup.import.file")} }
                input {
                    id: "tv-backup-file",
                    class: "ik-input",
                    r#type: "file",
                    accept: "application/json,.json",
                    onchange: choose,
                }
                if !file_name.read().is_empty() {
                    div { class: "ik-mono ik-muted", style: "font-size:11px;margin-top:4px;",
                        "{file_name}"
                    }
                }
            }
            div { class: "ik-field",
                label { r#for: "tv-backup-mode", {i18n.t("account.backup.import.mode")} }
                select {
                    id: "tv-backup-mode",
                    class: "ik-input",
                    value: mode_token(current_mode),
                    onchange: move |e| mode.set(parse_mode(&e.value())),
                    for m in [WatchlistImportMode::Merge, WatchlistImportMode::Overwrite, WatchlistImportMode::Replace] {
                        option {
                            key: "{mode_token(m)}",
                            value: mode_token(m),
                            {i18n.t(&format!("account.backup.import.modes.{}.label", mode_token(m)))}
                        }
                    }
                }
                if current_mode == WatchlistImportMode::Replace {
                    div { class: "ik-error", style: "font-size:12px;margin-top:6px;padding:8px 10px;",
                        {i18n.t("account.backup.import.modes.replace.hint")}
                    }
                } else {
                    div { class: "ik-muted", style: "font-size:12px;margin-top:4px;",
                        {i18n.t(&format!("account.backup.import.modes.{}.hint", mode_token(current_mode)))}
                    }
                }
            }
            label { style: "display:flex;gap:8px;align-items:flex-start;font-size:13px;",
                input {
                    r#type: "checkbox",
                    checked: *match_titles.read(),
                    onchange: move |e| match_titles.set(e.checked()),
                }
                span {
                    {i18n.t("account.backup.import.matchTitles")}
                    div { class: "ik-muted", style: "font-size:12px;",
                        {i18n.t("account.backup.import.matchTitlesHint")}
                    }
                }
            }
            OutcomeLine { outcome: outcome.read().clone() }
            StepUpGuard { gate }
            if let Some(plan) = previewed.read().as_ref().filter(|_| preview_current) {
                ReportView { report: plan.report.clone(), title: i18n.t("account.backup.import.previewTitle") }
            }
            if let Some(report) = applied.read().clone() {
                ReportView { report, title: i18n.t("account.backup.import.resultTitle") }
            }
            div { style: "display:flex;gap:8px;margin-top:12px;flex-wrap:wrap;",
                Button {
                    disabled: busy.is_busy() || !has_file,
                    on_click: preview,
                    {i18n.t("account.backup.import.preview")}
                }
                Button {
                    tone: if current_mode == WatchlistImportMode::Replace { Tone::Danger } else { Tone::Primary },
                    disabled: busy.is_busy() || !preview_current,
                    on_click: apply,
                    {i18n.t("account.backup.import.apply")}
                }
            }
        }
    }
}

/// One figure in a report, omitted when zero so the list reads as what happens.
fn figure(i18n: Translator, key: &str, count: i64) -> Option<String> {
    (count > 0).then(|| i18n.plural(key, count, &[]))
}

/// What a preview would do, or what an import did.
#[component]
fn ReportView(report: WatchlistImportReport, title: String) -> Element {
    let i18n = use_i18n();
    let figures: Vec<String> = [
        ("account.backup.report.added", report.added),
        ("account.backup.report.updated", report.updated),
        ("account.backup.report.unchanged", report.unchanged),
        ("account.backup.report.removed", report.removed),
        ("account.backup.report.progress", report.progress_written),
        ("account.backup.report.pending", report.pending),
        ("account.backup.report.duplicates", report.duplicates),
    ]
    .into_iter()
    .filter_map(|(key, count)| figure(i18n, key, count))
    .collect();

    rsx! {
        div { class: "ik-subhead", style: "margin-top:16px;", "{title}" }
        if figures.is_empty() {
            p { class: "ik-muted", style: "font-size:13px;", {i18n.t("account.backup.report.nothing")} }
        } else {
            ul { style: "font-size:13px;margin:6px 0 0;padding-left:18px;",
                for (i, line) in figures.into_iter().enumerate() {
                    li { key: "{i}", "{line}" }
                }
            }
        }
        if report.removed > 0 {
            div { class: "ik-error", style: "font-size:12px;margin-top:8px;padding:8px 10px;",
                {i18n.plural("account.backup.report.removedWarning", report.removed, &[])}
            }
        }
        if !report.title_matches.is_empty() {
            div { class: "ik-muted", style: "font-size:12px;margin-top:8px;",
                {i18n.t("account.backup.report.titleMatches")}
            }
            ul { style: "font-size:12px;margin:4px 0 0;padding-left:18px;",
                for issue in report.title_matches.iter() {
                    li { key: "t{issue.index}", "{issue.title}" }
                }
            }
        }
        if !report.pending_entries.is_empty() {
            div { class: "ik-muted", style: "font-size:12px;margin-top:8px;",
                {i18n.t("account.backup.report.pendingList")}
            }
            ul { style: "font-size:12px;margin:4px 0 0;padding-left:18px;",
                for issue in report.pending_entries.iter() {
                    li { key: "p{issue.index}", "{issue.title}" }
                }
            }
        }
    }
}

/// Restored entries still waiting for their series, and a way to stop waiting.
#[component]
fn PendingCard(reload: Reload) -> Element {
    let i18n = use_i18n();
    let api = api::use_api();
    let session = use_session();
    let busy = use_busy();
    let mut outcome = use_outcome();

    let pending = use_resource(move || {
        reload.track();
        let client = api.client();
        let authed = session.is_authenticated();
        async move {
            if !authed {
                return Ok(WatchlistPending {
                    total: 0,
                    items: Vec::new(),
                });
            }
            client
                .watchlist_import_pending()
                .send()
                .await
                .map(ResponseValue::into_inner)
                .map_err(|e| api::friendly_error(i18n, e))
        }
    });

    let discard = move |_| {
        if !busy.claim() {
            return;
        }
        outcome.set(None);
        let client = api.client();
        spawn(async move {
            match client.clear_watchlist_import_pending().send().await {
                Ok(_) => {
                    outcome.set(Some(Ok(i18n.t("account.backup.pending.discarded"))));
                    reload.bump();
                }
                Err(e) => outcome.set(Some(Err(api::friendly_error(i18n, e)))),
            }
            busy.release();
        });
    };

    rsx! {
        PanelCard { icon: Icon::CloudSync, title: i18n.t("account.backup.pending.title"),
            p { class: "ik-muted", style: "font-size:13px;margin-top:0;",
                {i18n.t("account.backup.pending.intro")}
            }
            OutcomeLine { outcome: outcome.read().clone() }
            {
                async_view(
                    &pending,
                    reload,
                    || rsx! { SkeletonRows { count: 2 } },
                    |view| {
                        if view.total == 0 {
                            return rsx! {
                                p { class: "ik-muted", style: "font-size:13px;", {i18n.t("account.backup.pending.empty")} }
                            };
                        }
                        let shown = i64::try_from(view.items.len()).unwrap_or(i64::MAX);
                        rsx! {
                            div { style: "font-size:13px;font-weight:600;",
                                {i18n.plural("account.backup.pending.count", view.total, &[])}
                            }
                            for (i, item) in view.items.iter().enumerate() {
                                div { key: "{i}", class: "ik-row",
                                    div { class: "grow",
                                        div { style: "font-weight:600;font-size:13px;", "{item.title}" }
                                        div { class: "ik-mono ik-muted", style: "font-size:11px;",
                                            {i18n.args("account.backup.pending.queuedOn", &[("date", iso_date(Some(&item.queued_at)))])}
                                            if let Some(url) = item.source_urls.first() {
                                                " · {url}"
                                            }
                                        }
                                    }
                                }
                            }
                            if view.total > shown {
                                div { class: "ik-muted", style: "font-size:12px;",
                                    {i18n.plural("account.backup.pending.more", view.total - shown, &[])}
                                }
                            }
                            Button {
                                tone: Tone::Danger,
                                style: "margin-top:12px;",
                                disabled: busy.is_busy(),
                                on_click: discard,
                                {i18n.t("account.backup.pending.discard")}
                            }
                        }
                    },
                )
            }
        }
    }
}
