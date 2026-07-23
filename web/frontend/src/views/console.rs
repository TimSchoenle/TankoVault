//! Operator Console (§17.2.7, RBAC). A full admin control surface:
//! - a live **system overview** (providers, catalogue, scan-queue and failure KPIs);
//! - the live **scan queue** — trigger runs, watch active runs' progress, and triage the
//!   most recent task failures with their errors;
//! - a per-provider **statistics table** (series/sources/chapters, freshness, last run);
//! - full **provider lifecycle** control — create, edit (name / domain-migration `base_url`
//!   / adapter config / crawl politeness), enable/disable, per-provider scan, live adapter
//!   dry-run, and delete;
//! - the canonicalisation **merge queue**; and
//! - the privileged-action **audit trail**.
//!
//! The read-only panels auto-refresh on a shared tick (pausable); every mutating call is
//! RBAC-gated server-side (create/delete require Admin; the rest require Operator).

use crate::api;
use crate::components::{rel_time, Cover, ErrorBox};
use crate::icons::{Ic, Icon};
use crate::models::{
    AdminSyncAccount, AdminSyncMapping, AuditEntry, FailedTask, MergeCandidate, Provider,
    ProviderId, ProviderInfo, ProviderStat, RunState, ScanMode, ScanRun, SeriesId, SeriesSummary,
    SuggestedMatch, SystemStats, UnmappedSeries, UnmatchedRemoteEntry, UserId,
};
use crate::state::use_session;
use crate::wire::{AdapterKind, ProviderState};
use dioxus::prelude::*;

/// Auto-refresh cadence for the read-only dashboard panels.
const REFRESH_MS: u32 = 4000;

/// The operator console's top-level tabs (DESIGN_SPEC §7.8), in order.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ConsoleTab {
    Overview,
    LiveScans,
    Providers,
    Solver,
    AdapterTest,
    Merge,
    Sync,
    Users,
    Audit,
}

impl ConsoleTab {
    const ALL: [ConsoleTab; 9] = [
        Self::Overview,
        Self::LiveScans,
        Self::Providers,
        Self::Solver,
        Self::AdapterTest,
        Self::Merge,
        Self::Sync,
        Self::Users,
        Self::Audit,
    ];
    fn label(self) -> &'static str {
        match self {
            Self::Overview => "Overview",
            Self::LiveScans => "Live scans",
            Self::Providers => "Providers",
            Self::Solver => "Challenge & solver",
            Self::AdapterTest => "Adapter test",
            Self::Merge => "Merge queue",
            Self::Sync => "Sync",
            Self::Users => "Users",
            Self::Audit => "Audit",
        }
    }
    fn icon(self) -> Icon {
        match self {
            Self::Overview => Icon::Dashboard,
            Self::LiveScans => Icon::Radar,
            Self::Providers => Icon::Public,
            Self::Solver => Icon::ShieldLock,
            Self::AdapterTest => Icon::Code,
            Self::Merge => Icon::Merge,
            Self::Sync => Icon::CloudSync,
            Self::Users => Icon::Group,
            Self::Audit => Icon::History,
        }
    }
}

/// Selectable adapter implementations (token, human label). Mirrors `AdapterKind`.
const ADAPTER_KINDS: &[(&str, &str)] = &[
    ("generic_config", "Generic (config-driven)"),
    ("madara", "Madara / WordPress"),
    ("custom", "Custom (built-in)"),
];

/// The wire token for a loaded provider's adapter kind (matches the SQL enum / `ADAPTER_KINDS`).
fn adapter_token(a: AdapterKind) -> &'static str {
    match a {
        AdapterKind::GenericConfig => "generic_config",
        AdapterKind::Madara => "madara",
        AdapterKind::Custom => "custom",
    }
}

#[component]
pub fn Console() -> Element {
    let session = use_session();

    if !session.role.read().is_operator() {
        return rsx! {
            h1 { class: "ik-page-title", "Console" }
            div { class: "ik-empty", "This area is for operators. Ask an admin for access." }
        };
    }

    // A single tick drives every read-only panel's refetch; the background loop bumps it on
    // a cadence while `auto` is on, and the "Refresh" control bumps it on demand.
    let mut tick = use_signal(|| 0u32);
    let auto = use_signal(|| true);
    let mut tab = use_signal(|| ConsoleTab::Overview);
    use_future(move || async move {
        loop {
            gloo_timers::future::TimeoutFuture::new(REFRESH_MS).await;
            if *auto.peek() {
                tick += 1;
            }
        }
    });

    let current = *tab.read();
    let panel = match current {
        ConsoleTab::Overview => rsx! {
            SystemOverview { tick }
            ProviderStatsTable { tick }
        },
        ConsoleTab::LiveScans => rsx! { ScanQueue { tick } },
        ConsoleTab::Providers => rsx! { ProvidersPanel {} },
        ConsoleTab::Solver => rsx! { SolverPanel { tick } },
        ConsoleTab::AdapterTest => rsx! { AdapterTestTab {} },
        ConsoleTab::Merge => rsx! { MergeQueue {} },
        ConsoleTab::Sync => rsx! { SyncAdminPanel {} },
        ConsoleTab::Users => rsx! { UsersPanel { tick } },
        ConsoleTab::Audit => rsx! { AuditPanel { tick } },
    };

    rsx! {
        div { class: "ik-flex", style: "justify-content:space-between;align-items:center;flex-wrap:wrap;",
            div { class: "ik-flex", style: "gap:9px;",
                Ic { icon: Icon::Dashboard, size: 22 }
                h1 { class: "ik-page-title", style: "margin:0;", "Operator Console" }
            }
            LiveControls { tick, auto }
        }
        div { class: "ik-tabs", style: "margin-top:14px;",
            for t in ConsoleTab::ALL {
                button {
                    class: if current == t { "ik-tab active" } else { "ik-tab" },
                    style: "display:inline-flex;align-items:center;gap:6px;",
                    onclick: move |_| tab.set(t),
                    Ic { icon: t.icon(), size: 15 }
                    span { "{t.label()}" }
                }
            }
        }
        {panel}
    }
}

/// Live-refresh status pill plus pause/resume and manual-refresh controls.
#[component]
fn LiveControls(tick: Signal<u32>, auto: Signal<bool>) -> Element {
    let is_auto = *auto.read();
    let pill_class = if is_auto { "ik-live on" } else { "ik-live" };
    rsx! {
        div { class: "ik-flex",
            span { class: "{pill_class}",
                span { class: "ik-live-dot" }
                if is_auto { "Live · 4s" } else { "Paused" }
            }
            button {
                class: "ik-btn",
                onclick: move |_| {
                    let mut a = auto;
                    let cur = *a.peek();
                    a.set(!cur);
                },
                if is_auto { "Pause" } else { "Resume" }
            }
            button {
                class: "ik-btn",
                onclick: move |_| {
                    let mut t = tick;
                    t += 1;
                },
                "Refresh"
            }
        }
    }
}

/// System-wide KPI header — the at-a-glance health of the whole system.
#[component]
fn SystemOverview(tick: Signal<u32>) -> Element {
    let session = use_session();
    let res = use_resource(move || {
        let _ = tick.read();
        async move {
            match session.token_value() {
                Some(t) => Some(api::system_stats(&t).await),
                None => None,
            }
        }
    });

    let body = match &*res.read_unchecked() {
        None | Some(None) => rsx! { div { class: "ik-skeleton", style: "height:104px;" } },
        Some(Some(Err(e))) => {
            rsx! {
                p { class: "ik-muted", style: "font-size:13px;", "Stats unavailable: {e}" }
            }
        }
        Some(Some(Ok(s))) => {
            let s = s.clone();
            rsx! {
                KpiGrid { stats: s }
            }
        }
    };

    rsx! {
        section { style: "margin-bottom:18px;", {body} }
    }
}

/// The grid of KPI tiles.
#[component]
fn KpiGrid(stats: SystemStats) -> Element {
    let s = stats;
    let runs_accent = (if s.runs_active > 0 { "good" } else { "" }).to_owned();
    let fail_accent = (if s.tasks_failed_24h > 0 { "warn" } else { "" }).to_owned();
    let merge_accent = (if s.pending_merges > 0 { "warn" } else { "" }).to_owned();
    rsx! {
        div { class: "ik-kpis",
            Kpi {
                label: "Providers",
                value: fmt_int(s.providers_total),
                sub: format!("{} active · {} unhealthy · {} off", s.providers_active, s.providers_unhealthy, s.providers_disabled),
                accent: "",
            }
            Kpi {
                label: "Series",
                value: fmt_int(s.series_total),
                sub: format!("{} source links", fmt_int(s.sources_total)),
                accent: "",
            }
            Kpi {
                label: "Chapters",
                value: fmt_int(s.chapters_total),
                sub: format!("+{} in 7d", fmt_int(s.chapters_7d)),
                accent: "",
            }
            Kpi {
                label: "New · 24h",
                value: fmt_int(s.chapters_24h),
                sub: format!("{} in the last hour", fmt_int(s.chapters_1h)),
                accent: "",
            }
            Kpi {
                label: "Active scans",
                value: fmt_int(s.runs_active),
                sub: format!("{} running now", fmt_int(s.runs_running)),
                accent: runs_accent,
            }
            Kpi {
                label: "Queue depth",
                value: fmt_int(s.tasks_queued),
                sub: format!("{} in flight", fmt_int(s.tasks_running)),
                accent: "",
            }
            Kpi {
                label: "Failures · 24h",
                value: fmt_int(s.tasks_failed_24h),
                sub: "tasks failed".to_owned(),
                accent: fail_accent,
            }
            Kpi {
                label: "Merge queue",
                value: fmt_int(s.pending_merges),
                sub: "pending review".to_owned(),
                accent: merge_accent,
            }
            Kpi {
                label: "Users",
                value: fmt_int(s.users_total),
                sub: "registered".to_owned(),
                accent: "",
            }
        }
    }
}

/// A single KPI tile: label, big value, and a supporting sub-line. `accent` is `""`,
/// `"good"`, or `"warn"`.
#[component]
fn Kpi(label: String, value: String, sub: String, accent: String) -> Element {
    rsx! {
        div { class: "ik-kpi",
            div { class: "ik-kpi-label", "{label}" }
            div { class: "ik-kpi-value {accent}", "{value}" }
            if !sub.is_empty() {
                div { class: "ik-kpi-sub", "{sub}" }
            }
        }
    }
}

/// Live scan queue: trigger a global run, watch every active run's progress, browse recent
/// run history, and triage the most recent task failures with their errors. Auto-refreshes
/// on the shared console tick.
#[component]
fn ScanQueue(tick: Signal<u32>) -> Element {
    let session = use_session();
    let mut mode = use_signal(|| ScanMode::Fast);
    let mut message = use_signal(|| Option::<String>::None);

    let runs = use_resource(move || {
        let _ = tick.read();
        async move {
            match session.token_value() {
                Some(t) => Some(api::recent_runs(&t).await),
                None => None,
            }
        }
    });
    let failures = use_resource(move || {
        let _ = tick.read();
        async move {
            match session.token_value() {
                Some(t) => Some(api::scan_failures(&t).await),
                None => None,
            }
        }
    });

    let trigger = move |_| {
        let m = *mode.read();
        let mut tick = tick;
        spawn(async move {
            if let Some(t) = session.token_value() {
                match api::trigger_scan(&t, None, m).await {
                    Ok(_) => {
                        message.set(Some("Scan queued for all providers.".to_owned()));
                        tick += 1;
                    }
                    Err(e) => message.set(Some(e)),
                }
            }
        });
    };

    let all_runs = match &*runs.read_unchecked() {
        Some(Some(Ok(list))) => list.clone(),
        _ => Vec::new(),
    };
    let active: Vec<ScanRun> = all_runs
        .iter()
        .filter(|r| matches!(r.state, RunState::Running | RunState::Queued))
        .cloned()
        .collect();

    rsx! {
        section { class: "ik-tile", style: "margin-bottom:18px;",
            div { class: "ik-flex", style: "justify-content:space-between;flex-wrap:wrap;",
                h3 { style: "margin:0;", "Scan queue" }
                div { class: "ik-flex",
                    select {
                        class: "ik-input",
                        style: "width:auto;",
                        value: if *mode.read() == ScanMode::Full { "full" } else { "fast" },
                        onchange: move |e| {
                            mode.set(if e.value() == "full" { ScanMode::Full } else { ScanMode::Fast });
                        },
                        option { value: "fast", "Fast scan (new chapters)" }
                        option { value: "full", "Full scan (rebuild)" }
                    }
                    button { class: "ik-btn primary", onclick: trigger, "Trigger scan (all)" }
                }
            }
            if let Some(m) = message.read().clone() {
                p { class: "ik-muted", style: "margin:8px 0 0;", "{m}" }
            }

            div { style: "margin-top:12px;",
                div { class: "ik-subhead", "Active runs" }
                if active.is_empty() {
                    p { class: "ik-muted", style: "font-size:13px;margin:6px 0 0;", "No runs in flight." }
                } else {
                    for r in active {
                        RunProgress { key: "{r.id}", run: r }
                    }
                }
            }

            RunHistory { runs: all_runs }
            FailuresPanel { failures: match &*failures.read_unchecked() {
                Some(Some(Ok(list))) => list.clone(),
                _ => Vec::new(),
            } }
        }
    }
}

/// Compact table of the most recent runs (any state).
#[component]
fn RunHistory(runs: Vec<ScanRun>) -> Element {
    if runs.is_empty() {
        return rsx! {
            div { style: "margin-top:16px;",
                div { class: "ik-subhead", "Recent runs" }
                p { class: "ik-muted", style: "font-size:13px;margin:6px 0 0;", "No scan runs yet." }
            }
        };
    }
    rsx! {
        div { style: "margin-top:16px;",
            div { class: "ik-subhead", "Recent runs" }
            div { class: "ik-tablewrap",
                table { class: "ik-table ik-table-compact",
                    thead {
                        tr {
                            th { "State" }
                            th { "Mode" }
                            th { "Scope" }
                            th { "Progress" }
                            th { "Started" }
                            th { "Finished" }
                        }
                    }
                    tbody {
                        for r in runs {
                            RunHistoryRow { key: "{r.id}", run: r }
                        }
                    }
                }
            }
        }
    }
}

#[component]
fn RunHistoryRow(run: ScanRun) -> Element {
    let (pill, label) = run_state_pill(run.state);
    let pct = (run.progress() * 100.0).round() as i32;
    let scope = match &run.provider_id {
        Some(id) => format!("#{}", short_id(id)),
        None => "all providers".to_owned(),
    };
    rsx! {
        tr {
            td { span { class: "{pill}", "{label}" } }
            td { class: "ik-mono", "{run.mode:?}" }
            td { class: "ik-mono ik-muted", "{scope}" }
            td {
                div { class: "ik-flex", style: "gap:8px;",
                    span { class: "ik-mono", style: "font-size:12px;min-width:82px;",
                        "{run.done_tasks}/{run.total_tasks}"
                        if run.failed_tasks > 0 {
                            span { style: "color:var(--vermilion);", " ·{run.failed_tasks}✗" }
                        }
                    }
                    div { class: "ik-progress", style: "flex:1;min-width:60px;",
                        span { style: "width:{pct}%;" }
                    }
                }
            }
            td { class: "ik-muted ik-mono", style: "font-size:12px;", "{rel_time(run.started_at.as_deref())}" }
            td { class: "ik-muted ik-mono", style: "font-size:12px;", "{rel_time(run.finished_at.as_deref())}" }
        }
    }
}

/// Recent task failures with their errors — the operator's triage feed.
#[component]
fn FailuresPanel(failures: Vec<FailedTask>) -> Element {
    if failures.is_empty() {
        return rsx! {
            div { style: "margin-top:16px;",
                div { class: "ik-subhead", "Recent failures" }
                p { class: "ik-muted", style: "font-size:13px;margin:6px 0 0;", "No task failures recorded. Clean." }
            }
        };
    }
    rsx! {
        div { style: "margin-top:16px;",
            div { class: "ik-subhead", "Recent failures" }
            div { style: "margin-top:8px;display:grid;gap:8px;",
                for f in failures {
                    div { key: "{f.id}", class: "ik-fail",
                        div { class: "ik-flex", style: "justify-content:space-between;gap:10px;",
                            div { class: "ik-flex", style: "gap:8px;flex-wrap:wrap;",
                                span { class: "ik-pill vermilion", "{f.kind}" }
                                span { class: "ik-mono ik-muted", style: "font-size:12px;",
                                    "{f.provider_slug.clone().unwrap_or_else(|| \"—\".to_owned())} · {f.mode:?} · attempt {f.attempts}"
                                }
                            }
                            span { class: "ik-muted ik-mono", style: "font-size:12px;", "{rel_time(f.finished_at.as_deref())}" }
                        }
                        p { class: "ik-mono", style: "margin:6px 0 0;font-size:12px;color:var(--vermilion);word-break:break-word;",
                            "{f.error.clone().unwrap_or_else(|| \"(no error message)\".to_owned())}"
                        }
                    }
                }
            }
        }
    }
}

#[component]
fn RunProgress(run: ScanRun) -> Element {
    let pct = (run.progress() * 100.0).round() as i32;
    let width = format!("width:{pct}%;");
    rsx! {
        div { style: "margin-top:12px;",
            div { class: "ik-flex", style: "justify-content:space-between;font-size:13px;",
                span { "{run.state.label()} · {run.mode:?}" }
                span { class: "ik-mono", "{run.done_tasks}/{run.total_tasks} ({run.failed_tasks} failed)" }
            }
            div { class: "ik-progress", style: "margin-top:6px;", span { style: "{width}" } }
        }
    }
}

/// Provider management: health tiles, an admin-only create form, and a full editor card
/// per provider (edit, state toggle, scan, adapter test, delete).
#[component]
fn ProvidersPanel() -> Element {
    let session = use_session();
    let is_admin = session.role.read().is_admin();
    let mut reload = use_signal(|| 0u32);
    let resource = use_resource(move || {
        let _ = reload.read();
        async move {
            match session.token_value() {
                Some(t) => api::providers(&t).await,
                None => Ok(Vec::new()),
            }
        }
    });

    let body = match &*resource.read_unchecked() {
        None => rsx! { div { class: "ik-skeleton", style: "height:80px;" } },
        Some(Err(e)) => {
            let msg = e.clone();
            rsx! {
                ErrorBox { message: msg, on_retry: move |()| reload += 1 }
            }
        }
        Some(Ok(list)) => {
            let tiles = list.clone();
            let cards = list.clone();
            rsx! {
                if tiles.is_empty() {
                    div { class: "ik-empty", "No providers yet. Add one below." }
                } else {
                    div { class: "ik-tiles",
                        for p in tiles {
                            div { class: "ik-tile",
                                div { style: "font-weight:600;", "{p.name}" }
                                HealthPill { state: provider_state_token(p.state).to_owned() }
                            }
                        }
                    }
                }
                for p in cards {
                    ProviderCard { key: "{p.id}", provider: p, reload }
                }
            }
        }
    };

    rsx! {
        section { style: "margin-bottom:18px;",
            h3 { "Providers" }
            if is_admin {
                CreateProviderForm { reload }
            }
            {body}
        }
    }
}

#[component]
fn HealthPill(state: String) -> Element {
    let class = match state.as_str() {
        "active" => "ik-pill jade",
        "blocked" | "disabled" => "ik-pill vermilion",
        _ => "ik-pill",
    };
    let label = if state.is_empty() {
        "unknown".to_owned()
    } else {
        state
    };
    rsx! {
        span { class: "{class}", "{label}" }
    }
}

/// The wire token for a loaded provider's health state (matches the SQL enum / `HealthPill`).
fn provider_state_token(s: ProviderState) -> &'static str {
    match s {
        ProviderState::Active => "active",
        ProviderState::Degraded => "degraded",
        ProviderState::Challenged => "challenged",
        ProviderState::Solving => "solving",
        ProviderState::Blocked => "blocked",
        ProviderState::Disabled => "disabled",
    }
}

/// Admin-only create form. Politeness is left at the polite server defaults on creation and
/// can be tuned immediately afterwards from the provider's editor card.
#[component]
fn CreateProviderForm(reload: Signal<u32>) -> Element {
    let session = use_session();
    let mut open = use_signal(|| false);
    let mut slug = use_signal(String::new);
    let mut name = use_signal(String::new);
    let mut base_url = use_signal(String::new);
    let mut adapter = use_signal(|| "generic_config".to_owned());
    let mut config = use_signal(|| "{}".to_owned());
    let mut busy = use_signal(|| false);
    let mut msg = use_signal(|| Option::<String>::None);

    let submit = move |_| {
        let cfg = match serde_json::from_str::<serde_json::Value>(&config.read()) {
            Ok(v) => v,
            Err(e) => {
                msg.set(Some(format!("Config is not valid JSON: {e}")));
                return;
            }
        };
        let (s, n, b, a) = (
            slug.read().trim().to_owned(),
            name.read().trim().to_owned(),
            base_url.read().trim().to_owned(),
            adapter.read().clone(),
        );
        if s.is_empty() || n.is_empty() || b.is_empty() {
            msg.set(Some("Slug, name and base URL are all required.".to_owned()));
            return;
        }
        busy.set(true);
        msg.set(None);
        let mut reload = reload;
        spawn(async move {
            let outcome = match session.token_value() {
                Some(t) => api::create_provider(&t, &s, &n, &b, &a, &cfg, None).await,
                None => Err("You are signed out.".to_owned()),
            };
            match outcome {
                Ok(_) => {
                    slug.set(String::new());
                    name.set(String::new());
                    base_url.set(String::new());
                    config.set("{}".to_owned());
                    busy.set(false);
                    open.set(false);
                    reload += 1;
                }
                Err(e) => {
                    msg.set(Some(e));
                    busy.set(false);
                }
            }
        });
    };

    rsx! {
        section { class: "ik-tile", style: "margin-bottom:14px;",
            div { class: "ik-flex", style: "justify-content:space-between;",
                h3 { style: "margin:0;", "Add provider" }
                button {
                    class: "ik-btn",
                    onclick: move |_| {
                        let o = *open.read();
                        open.set(!o);
                    },
                    if *open.read() { "Cancel" } else { "New provider" }
                }
            }
            if *open.read() {
                div { style: "margin-top:12px;display:grid;gap:10px;",
                    Field { label: "Slug (stable, unique)",
                        input {
                            class: "ik-input ik-mono",
                            placeholder: "acme-scans",
                            value: "{slug}",
                            oninput: move |e| slug.set(e.value()),
                        }
                    }
                    Field { label: "Display name",
                        input {
                            class: "ik-input",
                            placeholder: "Acme Scans",
                            value: "{name}",
                            oninput: move |e| name.set(e.value()),
                        }
                    }
                    Field { label: "Base URL",
                        input {
                            class: "ik-input ik-mono",
                            placeholder: "https://acmescans.example",
                            value: "{base_url}",
                            oninput: move |e| base_url.set(e.value()),
                        }
                    }
                    Field { label: "Adapter",
                        select {
                            class: "ik-input",
                            value: "{adapter}",
                            onchange: move |e| adapter.set(e.value()),
                            for (token , label) in ADAPTER_KINDS.iter().copied() {
                                option { value: "{token}", "{label}" }
                            }
                        }
                    }
                    Field { label: "Adapter config (JSON)",
                        textarea {
                            class: "ik-input ik-mono",
                            style: "min-height:120px;resize:vertical;",
                            value: "{config}",
                            oninput: move |e| config.set(e.value()),
                        }
                    }
                    div {
                        button {
                            class: "ik-btn primary",
                            disabled: *busy.read(),
                            onclick: submit,
                            "Create provider"
                        }
                    }
                    if let Some(m) = msg.read().clone() {
                        p { style: "margin:0;color:var(--vermilion);font-size:13px;", "{m}" }
                    }
                }
            }
        }
    }
}

/// A labelled form field wrapper for consistent spacing in the editor grids.
#[component]
fn Field(label: String, children: Element) -> Element {
    rsx! {
        label { style: "display:grid;gap:4px;",
            span { class: "ik-muted", style: "font-size:12px;", "{label}" }
            {children}
        }
    }
}

/// Full editor card for one provider. The adapter kind is immutable (recreate to change
/// it); everything else — name, base URL, config, politeness, health state — is editable,
/// plus per-provider scan, a live adapter dry-run, and delete.
#[component]
fn ProviderCard(provider: Provider, reload: Signal<u32>) -> Element {
    let session = use_session();
    let is_admin = session.role.read().is_admin();

    let id = provider.id;
    let original_base = provider.base_url.clone();
    let is_disabled = provider.state == ProviderState::Disabled;

    let mut expanded = use_signal(|| false);
    let mut show_test = use_signal(|| false);
    let mut busy = use_signal(|| false);
    let mut msg = use_signal(|| Option::<String>::None);
    let mut confirm_migrate = use_signal(|| false);
    let mut confirm_delete = use_signal(|| false);
    let mut scan_mode = use_signal(|| ScanMode::Fast);

    let mut name = use_signal(|| provider.name.clone());
    let mut base_url = use_signal(|| provider.base_url.clone());
    let mut config = use_signal(|| config_editor_text(&provider.config));
    let mut rps = use_signal(|| provider.politeness.rps.unwrap_or(1.0).to_string());
    let mut concurrency = use_signal(|| provider.politeness.concurrency.unwrap_or(2).to_string());
    let mut crawl_delay_ms =
        use_signal(|| provider.politeness.crawl_delay_ms.unwrap_or(0).to_string());
    let mut user_agent = use_signal(|| provider.politeness.user_agent.clone().unwrap_or_default());

    // Commits the edit. Cloneable so both the direct-save and the confirm-migration
    // buttons can drive it without duplicating the request logic.
    let saver = {
        move || {
            let cfg = match serde_json::from_str::<serde_json::Value>(&config.read()) {
                Ok(v) => v,
                Err(e) => {
                    msg.set(Some(format!("Config is not valid JSON: {e}")));
                    return;
                }
            };
            let pol = match politeness_json(
                &rps.read(),
                &concurrency.read(),
                &crawl_delay_ms.read(),
                &user_agent.read(),
            ) {
                Ok(v) => v,
                Err(e) => {
                    msg.set(Some(e));
                    return;
                }
            };
            let name_v = name.read().clone();
            let base_v = base_url.read().clone();
            confirm_migrate.set(false);
            busy.set(true);
            msg.set(None);
            let mut reload = reload;
            spawn(async move {
                let outcome = match session.token_value() {
                    Some(t) => api::update_provider(&t, id, &name_v, &base_v, &cfg, &pol).await,
                    None => Err("You are signed out.".to_owned()),
                };
                match outcome {
                    Ok(_) => reload += 1,
                    Err(e) => {
                        msg.set(Some(e));
                        busy.set(false);
                    }
                }
            });
        }
    };

    let on_save = {
        let mut saver = saver;
        let original_base = original_base.clone();
        move |_| {
            if *base_url.read() != original_base && !*confirm_migrate.read() {
                confirm_migrate.set(true);
            } else {
                saver();
            }
        }
    };
    let on_confirm_migrate = {
        let mut saver = saver;
        move |_| saver()
    };

    let toggle_state = {
        move |_| {
            let target = if is_disabled { "active" } else { "disabled" };
            let mut reload = reload;
            busy.set(true);
            msg.set(None);
            spawn(async move {
                if let Some(t) = session.token_value() {
                    if let Err(e) = api::set_provider_state(&t, id, target).await {
                        msg.set(Some(e));
                        busy.set(false);
                        return;
                    }
                }
                reload += 1;
            });
        }
    };

    let scan = {
        move |_| {
            let m = *scan_mode.read();
            msg.set(None);
            spawn(async move {
                if let Some(t) = session.token_value() {
                    match api::trigger_scan(&t, Some(id), m).await {
                        Ok(_) => msg.set(Some("Scan queued for this provider.".to_owned())),
                        Err(e) => msg.set(Some(e)),
                    }
                }
            });
        }
    };

    let delete = {
        move |_| {
            let mut reload = reload;
            busy.set(true);
            spawn(async move {
                let outcome = match session.token_value() {
                    Some(t) => api::delete_provider(&t, id).await,
                    None => Err("You are signed out.".to_owned()),
                };
                match outcome {
                    Ok(()) => reload += 1,
                    Err(e) => {
                        msg.set(Some(e));
                        busy.set(false);
                        confirm_delete.set(false);
                    }
                }
            });
        }
    };

    let base_changed = *base_url.read() != original_base;
    let test_id = id;

    rsx! {
        div { class: "ik-tile", style: "margin-bottom:12px;",
            div { class: "ik-flex", style: "justify-content:space-between;align-items:flex-start;gap:12px;",
                div { class: "grow",
                    div { class: "ik-flex",
                        span { style: "font-weight:600;", "{provider.name}" }
                        HealthPill { state: provider_state_token(provider.state).to_owned() }
                    }
                    div { class: "ik-muted ik-mono", style: "font-size:12px;margin-top:2px;word-break:break-all;",
                        "{provider.slug} · {adapter_token(provider.adapter)} · {provider.base_url}"
                    }
                }
                div { class: "ik-flex", style: "flex-wrap:wrap;justify-content:flex-end;",
                    select {
                        class: "ik-input",
                        style: "width:auto;",
                        value: if *scan_mode.read() == ScanMode::Full { "full" } else { "fast" },
                        onchange: move |e| {
                            scan_mode
                                .set(if e.value() == "full" { ScanMode::Full } else { ScanMode::Fast });
                        },
                        option { value: "fast", "Fast" }
                        option { value: "full", "Full" }
                    }
                    button { class: "ik-btn", onclick: scan, "Scan" }
                    button { class: "ik-btn", disabled: *busy.read(), onclick: toggle_state,
                        if is_disabled { "Enable" } else { "Disable" }
                    }
                    button {
                        class: "ik-btn",
                        onclick: move |_| {
                            let s = *show_test.read();
                            show_test.set(!s);
                        },
                        "Test"
                    }
                    button {
                        class: "ik-btn",
                        onclick: move |_| {
                            let e = *expanded.read();
                            expanded.set(!e);
                        },
                        if *expanded.read() { "Close" } else { "Edit" }
                    }
                    if is_admin {
                        button {
                            class: "ik-btn",
                            style: "color:var(--vermilion);",
                            onclick: move |_| confirm_delete.set(true),
                            "Delete"
                        }
                    }
                }
            }

            if let Some(m) = msg.read().clone() {
                p { style: "margin:8px 0 0;color:var(--vermilion);font-size:13px;", "{m}" }
            }

            if *confirm_delete.read() {
                div { class: "ik-flex", style: "margin-top:10px;",
                    span { class: "ik-muted", style: "font-size:13px;",
                        "Delete this provider and all its source links? This cannot be undone."
                    }
                    button { class: "ik-btn primary", disabled: *busy.read(), onclick: delete, "Confirm delete" }
                    button { class: "ik-btn", onclick: move |_| confirm_delete.set(false), "Cancel" }
                }
            }

            if *expanded.read() {
                div { style: "margin-top:14px;display:grid;gap:10px;border-top:1px solid var(--border);padding-top:14px;",
                    Field { label: "Display name",
                        input {
                            class: "ik-input",
                            value: "{name}",
                            oninput: move |e| name.set(e.value()),
                        }
                    }
                    Field { label: "Base URL (changing it re-resolves every stored link)",
                        input {
                            class: "ik-input ik-mono",
                            value: "{base_url}",
                            oninput: move |e| base_url.set(e.value()),
                        }
                    }
                    Field { label: "Adapter config (JSON)",
                        textarea {
                            class: "ik-input ik-mono",
                            style: "min-height:140px;resize:vertical;",
                            value: "{config}",
                            oninput: move |e| config.set(e.value()),
                        }
                    }
                    div { style: "display:grid;grid-template-columns:repeat(3,1fr);gap:10px;",
                        Field { label: "Requests / sec",
                            input {
                                class: "ik-input ik-mono",
                                value: "{rps}",
                                oninput: move |e| rps.set(e.value()),
                            }
                        }
                        Field { label: "Concurrency",
                            input {
                                class: "ik-input ik-mono",
                                value: "{concurrency}",
                                oninput: move |e| concurrency.set(e.value()),
                            }
                        }
                        Field { label: "Crawl delay (ms)",
                            input {
                                class: "ik-input ik-mono",
                                value: "{crawl_delay_ms}",
                                oninput: move |e| crawl_delay_ms.set(e.value()),
                            }
                        }
                    }
                    Field { label: "User agent",
                        input {
                            class: "ik-input ik-mono",
                            value: "{user_agent}",
                            oninput: move |e| user_agent.set(e.value()),
                        }
                    }
                    div { class: "ik-muted", style: "font-size:12px;",
                        "Rate limits are clamped to the system ceilings on save (≤ 4 req/s, ≤ 8 concurrent)."
                    }
                    if *confirm_migrate.read() {
                        div { class: "ik-flex",
                            span { class: "ik-muted", style: "font-size:13px;",
                                "The base URL changed — re-resolve every stored link to the new domain?"
                            }
                            button { class: "ik-btn primary", disabled: *busy.read(), onclick: on_confirm_migrate, "Confirm migration" }
                            button { class: "ik-btn", onclick: move |_| confirm_migrate.set(false), "Cancel" }
                        }
                    } else {
                        div {
                            button { class: "ik-btn primary", disabled: *busy.read(), onclick: on_save,
                                if base_changed { "Save changes (migrates domain)" } else { "Save changes" }
                            }
                        }
                    }
                }
            }

            if *show_test.read() {
                AdapterTestPanel { provider_id: test_id }
            }
        }
    }
}

/// Live adapter dry-run against the provider's site. Runs on demand only; shows the raw
/// parsed sample so operators can validate selectors without a deploy.
#[component]
fn AdapterTestPanel(provider_id: ProviderId) -> Element {
    let session = use_session();
    let mut path = use_signal(String::new);
    let mut running = use_signal(|| false);
    let mut result = use_signal(|| Option::<Result<serde_json::Value, String>>::None);

    let run = move |_| {
        let p = path.read().trim().to_owned();
        running.set(true);
        spawn(async move {
            let out = match session.token_value() {
                Some(t) => {
                    let path_opt = if p.is_empty() { None } else { Some(p.as_str()) };
                    api::test_adapter(&t, provider_id, path_opt).await
                }
                None => Err("You are signed out.".to_owned()),
            };
            result.set(Some(out));
            running.set(false);
        });
    };

    let output = match &*result.read() {
        None => rsx! {
            p { class: "ik-muted", style: "font-size:13px;margin:8px 0 0;",
                "Runs the adapter against the live site (latest list, and optionally one series path)."
            }
        },
        Some(Ok(v)) => {
            let text = serde_json::to_string_pretty(v).unwrap_or_else(|_| v.to_string());
            rsx! {
                pre {
                    class: "ik-mono",
                    style: "margin:10px 0 0;padding:12px;background:var(--surface);border:1px solid var(--border);border-radius:10px;font-size:12px;max-height:340px;overflow:auto;white-space:pre-wrap;word-break:break-word;",
                    "{text}"
                }
            }
        }
        Some(Err(e)) => rsx! {
            p { style: "margin:10px 0 0;color:var(--vermilion);font-size:13px;", "{e}" }
        },
    };

    rsx! {
        div { style: "margin-top:14px;border-top:1px solid var(--border);padding-top:14px;",
            div { class: "ik-flex",
                input {
                    class: "ik-input ik-mono",
                    style: "flex:1;",
                    placeholder: "optional series path, e.g. /manga/some-title",
                    value: "{path}",
                    oninput: move |e| path.set(e.value()),
                }
                button { class: "ik-btn primary", disabled: *running.read(), onclick: run,
                    if *running.read() { "Running…" } else { "Run test" }
                }
            }
            {output}
        }
    }
}

/// Challenge & solver (DESIGN_SPEC §7.8.4). The challenge back-end (FlareSolverr) is shown as
/// an informational card; per-provider solve-success metrics need a dedicated endpoint
/// (TODO(api) §9.5), so this lists provider health with a **Re-solve** (fast re-scan) action
/// and a **Re-enable** toggle for blocked/disabled providers.
#[component]
fn SolverPanel(tick: Signal<u32>) -> Element {
    let session = use_session();
    let mut reload = use_signal(|| 0u32);
    let res = use_resource(move || {
        let _ = (tick.read(), reload.read());
        async move {
            match session.token_value() {
                Some(t) => api::providers(&t).await,
                None => Ok(Vec::new()),
            }
        }
    });

    let body = match &*res.read_unchecked() {
        None => rsx! { div { class: "ik-skeleton", style: "height:100px;" } },
        Some(Err(e)) => {
            let msg = e.clone();
            rsx! {
                ErrorBox { message: msg, on_retry: move |()| reload += 1 }
            }
        }
        Some(Ok(list)) if list.is_empty() => rsx! {
            div { class: "ik-empty", "No providers configured." }
        },
        Some(Ok(list)) => {
            let rows = list.clone();
            rsx! {
                for p in rows {
                    SolverRow { key: "{p.id}", provider: p, reload }
                }
            }
        }
    };

    rsx! {
        section { style: "margin-bottom:18px;",
            div { class: "ik-tile", style: "margin-bottom:14px;",
                div { class: "ik-flex", style: "justify-content:space-between;flex-wrap:wrap;",
                    div { class: "ik-flex", style: "gap:9px;",
                        Ic { icon: Icon::ShieldLock, size: 20 }
                        div {
                            div { style: "font-weight:600;", "Challenge solver" }
                            div { class: "ik-mono ik-muted", style: "font-size:12px;", "Backend: FlareSolverr" }
                        }
                    }
                    span { class: "ik-pill jade", "active" }
                }
                p { class: "ik-muted", style: "font-size:13px;margin:10px 0 0;",
                    "Per-provider solve-success rates need the solver-metrics endpoint (TODO(api) §9.5). Until then, re-solve queues a fast re-scan that re-attempts any challenged sources."
                }
            }
            h3 { "Provider states" }
            {body}
        }
    }
}

#[component]
fn SolverRow(provider: Provider, reload: Signal<u32>) -> Element {
    let session = use_session();
    let id = provider.id;
    let blocked = matches!(
        provider.state,
        ProviderState::Blocked | ProviderState::Disabled | ProviderState::Challenged
    );

    let resolve = move |_| {
        let mut reload = reload;
        spawn(async move {
            if let Some(t) = session.token_value() {
                if api::resolve_provider(&t, id).await.is_ok() {
                    reload += 1;
                }
            }
        });
    };
    let reenable = move |_| {
        let mut reload = reload;
        spawn(async move {
            if let Some(t) = session.token_value() {
                if api::set_provider_state(&t, id, "active").await.is_ok() {
                    reload += 1;
                }
            }
        });
    };

    rsx! {
        div { class: "ik-row",
            div { class: "grow",
                div { style: "font-weight:600;", "{provider.name}" }
                div { class: "ik-mono ik-muted", style: "font-size:12px;", "{provider.base_url}" }
            }
            HealthPill { state: provider_state_token(provider.state).to_owned() }
            if blocked {
                button { class: "ik-btn", onclick: reenable, "Re-enable" }
            }
            button { class: "ik-btn primary", onclick: resolve,
                Ic { icon: Icon::Refresh, size: 15 }
                "Re-solve"
            }
        }
    }
}

/// Standalone Adapter-test tab (DESIGN_SPEC §7.8.5): pick a provider, then dry-run its
/// adapter against the live site and inspect the parsed sample (reuses `AdapterTestPanel`).
#[component]
fn AdapterTestTab() -> Element {
    let session = use_session();
    let res = use_resource(move || async move {
        match session.token_value() {
            Some(t) => api::providers(&t).await,
            None => Ok(Vec::new()),
        }
    });
    let mut chosen = use_signal(|| Option::<String>::None);

    let body = match &*res.read_unchecked() {
        None => rsx! { div { class: "ik-skeleton", style: "height:60px;" } },
        Some(Err(e)) => rsx! {
            p { class: "ik-muted", style: "font-size:13px;", "Could not load providers: {e}" }
        },
        Some(Ok(list)) if list.is_empty() => rsx! {
            div { class: "ik-empty", "No providers to test yet." }
        },
        Some(Ok(list)) => {
            let opts = list.clone();
            let sel = chosen.read().clone();
            rsx! {
                div { class: "ik-flex", style: "margin-bottom:4px;",
                    label { class: "ik-muted", style: "font-size:13px;", "Provider" }
                    select {
                        class: "ik-input",
                        style: "width:auto;",
                        onchange: move |e| {
                            let v = e.value();
                            chosen.set(if v.is_empty() { None } else { Some(v) });
                        },
                        option { value: "", selected: sel.is_none(), "— choose a provider —" }
                        for p in opts {
                            option { value: "{p.id}", selected: sel.as_deref() == Some(p.id.to_string().as_str()), "{p.name}" }
                        }
                    }
                }
                if let Some(pid) = chosen.read().clone().and_then(|v| v.parse::<ProviderId>().ok()) {
                    AdapterTestPanel { provider_id: pid }
                }
            }
        }
    };

    rsx! {
        section { style: "margin-bottom:18px;",
            h3 { "Adapter test" }
            p { class: "ik-muted", style: "font-size:13px;margin-top:0;",
                "Dry-run a provider's adapter against the live site without deploying — validate selectors and pagination."
            }
            {body}
        }
    }
}

/// Users tab (DESIGN_SPEC §7.8.7): the registered-user directory from `GET /v1/admin/users`
/// (§9.5) — identity, RBAC role, and how many series each user tracks — plus the aggregate
/// count. Read-only (role management has no endpoint yet).
#[component]
fn UsersPanel(tick: Signal<u32>) -> Element {
    let session = use_session();
    let res = use_resource(move || {
        let _ = tick.read();
        async move {
            match session.token_value() {
                Some(t) => Some(api::admin_users(&t).await),
                None => None,
            }
        }
    });

    let body = match &*res.read_unchecked() {
        None | Some(None) => rsx! { div { class: "ik-skeleton", style: "height:120px;" } },
        Some(Some(Err(e))) => rsx! {
            p { class: "ik-muted", style: "font-size:13px;", "Could not load users: {e}" }
        },
        Some(Some(Ok(list))) if list.is_empty() => rsx! {
            div { class: "ik-empty", "No users registered yet." }
        },
        Some(Some(Ok(list))) => {
            let count = list.len();
            let rows = list.clone();
            rsx! {
                div { class: "ik-kpis", style: "margin-bottom:14px;",
                    div { class: "ik-kpi",
                        div { class: "ik-kpi-label", "Registered users" }
                        div { class: "ik-kpi-value", "{fmt_int(count as i64)}" }
                    }
                }
                div { class: "ik-tablewrap",
                    table { class: "ik-table ik-table-compact",
                        thead {
                            tr {
                                th { "User" }
                                th { "Email" }
                                th { "Role" }
                                th { style: "text-align:right;", "Tracked" }
                                th { "Joined" }
                            }
                        }
                        tbody {
                            for u in rows {
                                UserRowView { key: "{u.id}", user: u }
                            }
                        }
                    }
                }
            }
        }
    };

    rsx! {
        section { style: "margin-bottom:18px;",
            h3 { "Users" }
            {body}
        }
    }
}

#[component]
fn UserRowView(user: crate::models::UserRow) -> Element {
    let joined = user.created_at.get(0..10).unwrap_or("").to_owned();
    let role_class = match user.role.as_str() {
        "admin" => "ik-pill acc",
        "operator" => "ik-pill jade",
        _ => "ik-pill",
    };
    rsx! {
        tr {
            td { "{user.username}" }
            td { class: "ik-mono ik-muted", style: "font-size:12px;", "{user.email}" }
            td { span { class: "{role_class}", "{user.role}" } }
            td { class: "ik-mono", style: "text-align:right;", "{fmt_int(user.tracked_count)}" }
            td { class: "ik-mono ik-muted", style: "font-size:12px;", "{joined}" }
        }
    }
}

/// Per-provider statistics table (read-only, auto-refreshing): catalogue footprint,
/// content freshness, and last-scan health for every provider at a glance.
#[component]
fn ProviderStatsTable(tick: Signal<u32>) -> Element {
    let session = use_session();
    let res = use_resource(move || {
        let _ = tick.read();
        async move {
            match session.token_value() {
                Some(t) => Some(api::provider_stats(&t).await),
                None => None,
            }
        }
    });

    let body = match &*res.read_unchecked() {
        None | Some(None) => rsx! { div { class: "ik-skeleton", style: "height:120px;" } },
        Some(Some(Err(e))) => {
            rsx! {
                p { class: "ik-muted", style: "font-size:13px;", "Provider stats unavailable: {e}" }
            }
        }
        Some(Some(Ok(list))) if list.is_empty() => rsx! {
            div { class: "ik-empty", "No providers configured yet." }
        },
        Some(Some(Ok(list))) => {
            let rows = list.clone();
            rsx! {
                div { class: "ik-tablewrap",
                    table { class: "ik-table ik-table-compact",
                        thead {
                            tr {
                                th { "Provider" }
                                th { "Adapter" }
                                th { style: "text-align:right;", "Series" }
                                th { style: "text-align:right;", "Sources" }
                                th { style: "text-align:right;", "Chapters" }
                                th { style: "text-align:right;", "24h" }
                                th { style: "text-align:right;", "7d" }
                                th { "Newest" }
                                th { "Last scan" }
                                th { "Last run" }
                            }
                        }
                        tbody {
                            for p in rows {
                                ProviderStatRow { key: "{p.provider_id}", stat: p }
                            }
                        }
                    }
                }
            }
        }
    };

    rsx! {
        section { style: "margin-bottom:18px;",
            h3 { "Provider statistics" }
            {body}
        }
    }
}

#[component]
fn ProviderStatRow(stat: ProviderStat) -> Element {
    let s = stat;
    let blocked = if s.blocked_sources > 0 {
        format!(" · {} off", s.blocked_sources)
    } else {
        String::new()
    };
    let last_run = match (&s.last_run_state, s.last_run_at.as_deref()) {
        (Some(state), at) => format!("{state} · {}", rel_time(at)),
        (None, _) => "—".to_owned(),
    };
    rsx! {
        tr {
            td {
                div { style: "font-weight:600;", "{s.name}" }
                div { class: "ik-flex", style: "gap:6px;margin-top:2px;",
                    HealthPill { state: s.state.clone() }
                    span { class: "ik-mono ik-muted", style: "font-size:11px;", "{s.slug}" }
                }
            }
            td { class: "ik-mono ik-muted", style: "font-size:12px;", "{s.adapter}" }
            td { class: "ik-mono", style: "text-align:right;", "{fmt_int(s.series_count)}" }
            td { class: "ik-mono", style: "text-align:right;",
                "{fmt_int(s.source_count)}"
                if !blocked.is_empty() {
                    span { class: "ik-muted", style: "font-size:11px;", "{blocked}" }
                }
            }
            td { class: "ik-mono", style: "text-align:right;", "{fmt_int(s.chapter_count)}" }
            td { class: "ik-mono", style: "text-align:right;",
                if s.chapters_24h > 0 {
                    span { style: "color:var(--jade);", "+{fmt_int(s.chapters_24h)}" }
                } else {
                    span { class: "ik-muted", "0" }
                }
            }
            td { class: "ik-mono ik-muted", style: "text-align:right;", "{fmt_int(s.chapters_7d)}" }
            td { class: "ik-muted ik-mono", style: "font-size:12px;", "{rel_time(s.last_chapter_at.as_deref())}" }
            td { class: "ik-muted ik-mono", style: "font-size:12px;", "{rel_time(s.last_scanned_at.as_deref())}" }
            td { class: "ik-muted ik-mono", style: "font-size:12px;", "{last_run}" }
        }
    }
}

/// Privileged-action audit trail (design §16): recent operator actions, newest first.
#[component]
fn AuditPanel(tick: Signal<u32>) -> Element {
    let session = use_session();
    let res = use_resource(move || {
        let _ = tick.read();
        async move {
            match session.token_value() {
                Some(t) => Some(api::audit_log(&t).await),
                None => None,
            }
        }
    });

    let body = match &*res.read_unchecked() {
        None | Some(None) => rsx! { div { class: "ik-skeleton", style: "height:80px;" } },
        Some(Some(Err(e))) => {
            rsx! {
                p { class: "ik-muted", style: "font-size:13px;", "Audit log unavailable: {e}" }
            }
        }
        Some(Some(Ok(list))) if list.is_empty() => rsx! {
            div { class: "ik-empty", "No privileged actions recorded yet." }
        },
        Some(Some(Ok(list))) => {
            let rows = list.clone();
            rsx! {
                div { class: "ik-tablewrap",
                    table { class: "ik-table ik-table-compact",
                        thead {
                            tr {
                                th { "When" }
                                th { "Actor" }
                                th { "Action" }
                                th { "Target" }
                            }
                        }
                        tbody {
                            for a in rows {
                                AuditRow { key: "{a.id}", entry: a }
                            }
                        }
                    }
                }
            }
        }
    };

    rsx! {
        section { style: "margin-bottom:18px;",
            h3 { "Audit trail" }
            {body}
        }
    }
}

#[component]
fn AuditRow(entry: AuditEntry) -> Element {
    let a = entry;
    let actor = a.actor.clone().unwrap_or_else(|| "system".to_owned());
    let target = a.target.clone().unwrap_or_else(|| "—".to_owned());
    rsx! {
        tr {
            td { class: "ik-muted ik-mono", style: "font-size:12px;white-space:nowrap;", "{rel_time(Some(a.created_at.as_str()))}" }
            td { "{actor}" }
            td { span { class: "ik-pill", "{a.action}" } }
            td { class: "ik-mono ik-muted", style: "font-size:12px;word-break:break-all;", "{target}" }
        }
    }
}

/// Canonicalisation review queue with merge / dismiss actions.
#[component]
fn MergeQueue() -> Element {
    let session = use_session();
    let mut reload = use_signal(|| 0u32);
    let resource = use_resource(move || {
        let _ = reload.read();
        async move {
            match session.token_value() {
                Some(t) => api::merge_candidates(&t).await,
                None => Ok(Vec::new()),
            }
        }
    });

    let body = match &*resource.read_unchecked() {
        None => rsx! { div { class: "ik-skeleton", style: "height:60px;" } },
        Some(Err(e)) => {
            let msg = e.clone();
            rsx! {
                ErrorBox { message: msg, on_retry: move |()| reload += 1 }
            }
        }
        Some(Ok(list)) if list.is_empty() => rsx! {
            div { class: "ik-empty", "No pending merge candidates. Canonicalisation is clean." }
        },
        Some(Ok(list)) => {
            let list = list.clone();
            rsx! {
                for c in list {
                    MergeRow { key: "{c.id}", candidate: c, reload }
                }
            }
        }
    };

    rsx! {
        section {
            h3 { "Merge queue" }
            {body}
        }
    }
}

#[component]
fn MergeRow(candidate: MergeCandidate, reload: Signal<u32>) -> Element {
    let session = use_session();
    let pct = (candidate.score * 100.0).round() as i32;
    let id = candidate.id;
    let a = candidate.series_id;
    let b = candidate.candidate_id;
    let mut open = use_signal(|| false);
    let mut busy = use_signal(|| false);

    // Higher-confidence matches get a warmer pill so operators can triage at a glance.
    let score_class = if pct >= 90 {
        "ik-mono acc"
    } else if pct >= 75 {
        "ik-mono jade"
    } else {
        "ik-mono ik-muted"
    };

    let merge = {
        let mut reload = reload;
        move |_| {
            if *busy.peek() {
                return;
            }
            busy.set(true);
            spawn(async move {
                if let Some(t) = session.token_value() {
                    if api::merge_series(&t, a, b).await.is_ok() {
                        reload += 1;
                    }
                }
                busy.set(false);
            });
        }
    };

    let dismiss = {
        let mut reload = reload;
        move |_| {
            if *busy.peek() {
                return;
            }
            busy.set(true);
            spawn(async move {
                if let Some(t) = session.token_value() {
                    if api::dismiss_candidate(&t, id).await.is_ok() {
                        reload += 1;
                    }
                }
                busy.set(false);
            });
        }
    };

    let keep_id = a;
    let drop_id = b;
    let reason = candidate.reason.clone();

    rsx! {
        div { class: "ik-card", style: "margin-bottom:10px;",
            div { class: "ik-row",
                div { class: "grow",
                    div { class: "ik-flex", style: "justify-content:space-between;align-items:center;",
                        span { style: "font-weight:600;", "{candidate.series_title}" }
                        span { class: "{score_class}", "{pct}% match" }
                    }
                    div { class: "ik-muted", style: "font-size:13px;", "↔ {candidate.candidate_title}" }
                    if let Some(r) = &reason {
                        div { class: "ik-muted", style: "font-size:12px;", "reason: {r}" }
                    }
                }
                button {
                    class: "ik-btn",
                    onclick: move |_| { let v = *open.peek(); open.set(!v); },
                    if *open.read() { "Hide" } else { "Compare" }
                }
                button { class: "ik-btn primary", disabled: *busy.read(), onclick: merge, "Merge →" }
                button { class: "ik-btn", disabled: *busy.read(), onclick: dismiss, "Distinct" }
            }
            if *open.read() {
                div { class: "ik-flex", style: "gap:14px;margin-top:12px;align-items:stretch;flex-wrap:wrap;",
                    div { style: "flex:1;min-width:240px;",
                        div { class: "ik-pill jade", style: "margin-bottom:6px;", "Keep (canonical)" }
                        SeriesMiniCard { series_id: keep_id }
                    }
                    div { style: "flex:1;min-width:240px;",
                        div { class: "ik-pill", style: "margin-bottom:6px;", "Merge in & delete" }
                        SeriesMiniCard { series_id: drop_id }
                    }
                }
            }
        }
    }
}

/// Compact read-only "manga info" card for a single series, used by the merge compare view
/// and the Sync inspector. Fetches the public series detail so operators can eyeball cover,
/// type/status, sources and tags before acting.
#[component]
fn SeriesMiniCard(series_id: SeriesId) -> Element {
    let res = use_resource(move || async move { api::series_detail(series_id).await });

    match &*res.read_unchecked() {
        None => rsx! { div { class: "ik-skeleton", style: "height:120px;" } },
        Some(Err(e)) => rsx! {
            div { class: "ik-empty", style: "font-size:12px;", "Could not load series: {e}" }
        },
        Some(Ok(d)) => {
            let d = d.clone();
            let year = d.release_year.map(|y| y.to_string()).unwrap_or_default();
            let tags: Vec<String> = d.tags.iter().take(6).map(|t| t.name.clone()).collect();
            rsx! {
                div { class: "ik-card", style: "padding:10px;",
                    div { class: "ik-flex", style: "gap:10px;align-items:flex-start;",
                        div { style: "width:56px;flex:0 0 auto;",
                            Cover { url: d.cover_url.clone(), title: d.title.clone() }
                        }
                        div { class: "grow",
                            div { style: "font-weight:600;", "{d.title}" }
                            div { class: "ik-flex", style: "gap:6px;margin-top:4px;flex-wrap:wrap;",
                                span { class: "ik-pill", "{d.content_type.label()}" }
                                span { class: "ik-pill", "{d.status.label()}" }
                                if !year.is_empty() {
                                    span { class: "ik-pill", "{year}" }
                                }
                                span { class: "ik-pill", "{d.sources.len()} sources" }
                            }
                            div { class: "ik-mono ik-muted", style: "font-size:11px;margin-top:4px;word-break:break-all;",
                                "{d.id}"
                            }
                        }
                    }
                    if !d.sources.is_empty() {
                        div { style: "margin-top:8px;",
                            for s in d.sources.iter().take(5) {
                                div { class: "ik-muted", style: "font-size:12px;",
                                    "· {s.provider_name} — {s.chapter_count} ch"
                                }
                            }
                        }
                    }
                    if !tags.is_empty() {
                        div { class: "ik-flex", style: "gap:4px;margin-top:8px;flex-wrap:wrap;",
                            for t in tags {
                                span { class: "ik-pill", style: "font-size:11px;", "{t}" }
                            }
                        }
                    }
                }
            }
        }
    }
}

#[component]
fn SyncAdminPanel() -> Element {
    let session = use_session();
    let mut reload = use_signal(|| 0u32);
    // The series currently open in the "manga info" inspector, shared with the assign queue
    // so "Inspect" jumps straight to the editable per-provider mapping view.
    let selected = use_signal(|| Option::<String>::None);

    let accounts = use_resource(move || {
        let _ = reload.read();
        async move {
            match session.token_value() {
                Some(t) => api::admin_sync_accounts(&t).await,
                None => Ok(Vec::new()),
            }
        }
    });

    let accounts_body = match &*accounts.read_unchecked() {
        None => rsx! { div { class: "ik-skeleton", style: "height:60px;" } },
        Some(Err(e)) => {
            let msg = e.clone();
            rsx! { ErrorBox { message: msg, on_retry: move |()| reload += 1 } }
        }
        Some(Ok(list)) if list.is_empty() => rsx! {
            div { class: "ik-empty", "No linked external accounts." }
        },
        Some(Ok(list)) => {
            let list = list.clone();
            rsx! {
                for a in list {
                    SyncAccountRow { key: "{a.user_id}-{a.provider}", account: a, reload }
                }
            }
        }
    };

    rsx! {
        section { style: "margin-bottom:24px;",
            h3 { "Linked accounts" }
            {accounts_body}
        }
        section { style: "margin-bottom:24px;",
            h3 { "Series sync inspector" }
            p { class: "ik-muted", style: "font-size:13px;margin-top:0;",
                "Open any series to see its info and what it is synced to. Fix a wrong external id or add a missing one by hand."
            }
            SeriesSyncInspector { selected, reload }
        }
        section { style: "margin-bottom:24px;",
            h3 { "Assign queue" }
            p { class: "ik-muted", style: "font-size:13px;margin-top:0;",
                "Series with no mapping for the selected provider yet — the ones auto-matching was not confident about. Assign an id or open the inspector."
            }
            AssignQueue { selected, reload }
        }
        section {
            h3 { "Match every loaded entry" }
            p { class: "ik-muted", style: "font-size:13px;margin-top:0;",
                "Fetched remote entries the auto-matcher could not confidently link. Each one comes with ranked suggestions and a link to open it on the provider; inspect any candidate, then match it — this maps it, imports it onto the user's watchlist, and clears it here."
            }
            UnmatchedRemoteQueue { reload }
        }
    }
}

#[component]
fn SyncAccountRow(account: AdminSyncAccount, reload: Signal<u32>) -> Element {
    let session = use_session();
    let mut busy = use_signal(|| false);
    let last_sync = rel_time(account.last_synced_at.as_deref());

    let pull = {
        let user_id = UserId(account.user_id);
        let provider = account.provider.clone();
        let mut reload = reload;
        move |_| {
            if *busy.peek() {
                return;
            }
            busy.set(true);
            let provider = provider.clone();
            spawn(async move {
                if let Some(t) = session.token_value() {
                    if api::admin_sync_pull(&t, user_id, &provider).await.is_ok() {
                        reload += 1;
                    }
                }
                busy.set(false);
            });
        }
    };

    let push = {
        let user_id = UserId(account.user_id);
        let provider = account.provider.clone();
        let mut reload = reload;
        move |_| {
            if *busy.peek() {
                return;
            }
            busy.set(true);
            let provider = provider.clone();
            spawn(async move {
                if let Some(t) = session.token_value() {
                    if api::admin_sync_push(&t, user_id, &provider).await.is_ok() {
                        reload += 1;
                    }
                }
                busy.set(false);
            });
        }
    };

    let unlink = {
        let user_id = UserId(account.user_id);
        let provider = account.provider.clone();
        let mut reload = reload;
        move |_| {
            if *busy.peek() {
                return;
            }
            busy.set(true);
            let provider = provider.clone();
            spawn(async move {
                if let Some(t) = session.token_value() {
                    if api::admin_sync_unlink(&t, user_id, &provider).await.is_ok() {
                        reload += 1;
                    }
                }
                busy.set(false);
            });
        }
    };

    rsx! {
        div { class: "ik-row",
            div { class: "grow",
                div { class: "ik-flex", style: "justify-content:space-between;",
                    span { style: "font-weight:600;", "{account.username}" }
                    span { class: "ik-pill", "{account.provider}" }
                }
                div { class: "ik-muted", style: "font-size:13px;",
                    if let Some(u) = &account.external_username {
                        "Connected as {u} · last sync {last_sync}"
                    } else {
                        "last sync {last_sync}"
                    }
                }
                div { class: "ik-mono ik-muted", style: "font-size:11px;",
                    if account.auto_sync_enabled { "auto-sync on" } else { "auto-sync off" }
                    " · policy {account.conflict_policy}"
                    if account.pending_conflicts > 0 {
                        span { style: "color:var(--acc);", " · {account.pending_conflicts} pending conflicts" }
                    }
                }
                if let Some(err) = &account.last_error {
                    div { style: "font-size:12px;color:var(--acc);", "{err}" }
                }
            }
            button { class: "ik-btn", disabled: *busy.read(), onclick: pull, "Force pull" }
            button { class: "ik-btn", disabled: *busy.read(), onclick: push, "Force push" }
            button { class: "ik-btn", disabled: *busy.read(), onclick: unlink, "Unlink" }
        }
    }
}

/// Either the editable per-series "manga info" view (when a series is selected) or a title
/// search + recently-mapped list to open one.
#[component]
fn SeriesSyncInspector(selected: Signal<Option<String>>, reload: Signal<u32>) -> Element {
    let session = use_session();
    let mut query = use_signal(String::new);

    // All hooks are declared unconditionally (Rules of Hooks) before we branch on whether a
    // series is currently open in the editor.
    let results = use_resource(move || {
        let q = query.read().clone();
        async move {
            if q.trim().len() < 2 {
                return Ok(Vec::new());
            }
            api::list_series(Some(&q), 12).await
        }
    });

    let mappings = use_resource(move || {
        let _ = reload.read();
        async move {
            match session.token_value() {
                Some(t) => api::admin_sync_mappings(&t).await,
                None => Ok(Vec::new()),
            }
        }
    });

    if let Some(sid) = selected.read().clone() {
        return rsx! {
            SeriesSyncEditor { key: "{sid}", series_id: sid, selected, reload }
        };
    }

    let results_body = match &*results.read_unchecked() {
        Some(Ok(list)) if !list.is_empty() => {
            let list = list.clone();
            rsx! {
                div { style: "margin-top:8px;",
                    for s in list {
                        SeriesPickRow { key: "{s.id}", series: s, selected }
                    }
                }
            }
        }
        Some(Err(e)) => rsx! {
            div { class: "ik-empty", style: "font-size:12px;", "Search failed: {e}" }
        },
        _ => rsx! {},
    };

    let mappings_body = match &*mappings.read_unchecked() {
        None => rsx! { div { class: "ik-skeleton", style: "height:40px;" } },
        Some(Err(e)) => {
            let msg = e.clone();
            rsx! { ErrorBox { message: msg, on_retry: move |()| reload += 1 } }
        }
        Some(Ok(list)) if list.is_empty() => rsx! {
            div { class: "ik-empty", "No series↔external mappings yet." }
        },
        Some(Ok(list)) => {
            let list = list.clone();
            rsx! {
                for m in list {
                    MappingPickRow { key: "{m.series_id}-{m.provider}", mapping: m, selected }
                }
            }
        }
    };

    rsx! {
        input {
            class: "ik-input",
            style: "width:100%;",
            r#type: "text",
            placeholder: "Search a series by title to open its info…",
            value: "{query}",
            oninput: move |e| query.set(e.value()),
        }
        {results_body}
        div { class: "ik-muted", style: "font-size:12px;margin:14px 0 6px;", "Recently mapped" }
        {mappings_body}
    }
}

/// A search-result row that opens the series in the inspector.
#[component]
fn SeriesPickRow(series: SeriesSummary, selected: Signal<Option<String>>) -> Element {
    let sid = series.id;
    rsx! {
        div { class: "ik-row",
            div { class: "grow",
                span { style: "font-weight:600;", "{series.title}" }
                div { class: "ik-muted", style: "font-size:12px;", "{series.source_count} sources" }
            }
            button { class: "ik-btn", onclick: move |_| selected.set(Some(sid.to_string())), "Open" }
        }
    }
}

/// A recently-mapped row that opens the series in the inspector.
#[component]
fn MappingPickRow(mapping: AdminSyncMapping, selected: Signal<Option<String>>) -> Element {
    let updated = rel_time(Some(&mapping.updated_at));
    let sid = mapping.series_id;
    rsx! {
        div { class: "ik-row",
            div { class: "grow",
                div { class: "ik-flex", style: "justify-content:space-between;",
                    span { style: "font-weight:600;", "{mapping.series_title}" }
                    span { class: "ik-pill", "{mapping.provider}" }
                }
                div { class: "ik-mono ik-muted", style: "font-size:12px;",
                    "id {mapping.external_id} · updated {updated}"
                }
            }
            button { class: "ik-btn", onclick: move |_| selected.set(Some(sid.to_string())), "Open" }
        }
    }
}

/// The editable per-series "manga info" view: the series card plus one editor row per known
/// sync provider (prefilled with its current external id), so an operator can add, correct
/// or clear a mapping by hand.
#[component]
fn SeriesSyncEditor(
    series_id: String,
    selected: Signal<Option<String>>,
    reload: Signal<u32>,
) -> Element {
    let session = use_session();
    // `selected` (and therefore this component's `series_id` prop) is a plain `String` shared
    // with the search/pick-row flow above; parse it once here at the boundary.
    let Ok(sid) = series_id.parse::<SeriesId>() else {
        return rsx! { div { class: "ik-empty", "That series id doesn't look right." } };
    };

    let mappings = use_resource(move || {
        let _ = reload.read();
        async move {
            match session.token_value() {
                Some(t) => api::admin_sync_mappings_for_series(&t, sid).await,
                None => Ok(Vec::new()),
            }
        }
    });

    let providers = use_resource(move || async move {
        match session.token_value() {
            Some(t) => api::sync_providers(&t).await,
            None => Ok(Vec::new()),
        }
    });

    let map_list: Vec<AdminSyncMapping> = match &*mappings.read_unchecked() {
        Some(Ok(l)) => l.clone(),
        _ => Vec::new(),
    };
    let prov_list: Vec<ProviderInfo> = match &*providers.read_unchecked() {
        Some(Ok(l)) => l.clone(),
        _ => Vec::new(),
    };

    rsx! {
        div { class: "ik-flex", style: "justify-content:space-between;align-items:center;margin-bottom:10px;",
            button { class: "ik-btn", onclick: move |_| selected.set(None), "← Back to search" }
            button { class: "ik-btn", onclick: move |_| reload += 1, "Refresh" }
        }
        SeriesMiniCard { series_id: sid }
        div { class: "ik-muted", style: "font-size:12px;margin:14px 0 6px;", "External sync mappings" }
        if prov_list.is_empty() && map_list.is_empty() {
            div { class: "ik-empty", "No sync providers registered." }
        }
        for p in prov_list.clone() {
            {
                let current = map_list
                    .iter()
                    .find(|m| m.provider == p.slug)
                    .map(|m| m.external_id.clone());
                rsx! {
                    MappingEditorRow {
                        key: "{p.slug}",
                        series_id: sid,
                        provider: p.slug.clone(),
                        provider_name: p.name.clone(),
                        current,
                        reload,
                    }
                }
            }
        }
        for m in map_list.clone() {
            if !prov_list.iter().any(|p| p.slug == m.provider) {
                MappingEditorRow {
                    key: "orphan-{m.provider}",
                    series_id: sid,
                    provider: m.provider.clone(),
                    provider_name: m.provider.clone(),
                    current: Some(m.external_id.clone()),
                    reload,
                }
            }
        }
    }
}

/// One provider's mapping editor: an input prefilled with the current external id, plus
/// Save (upsert) and, when a mapping exists, Clear (delete).
#[component]
fn MappingEditorRow(
    series_id: SeriesId,
    provider: String,
    provider_name: String,
    current: Option<String>,
    reload: Signal<u32>,
) -> Element {
    let session = use_session();
    let has_current = current.is_some();
    let mut value = use_signal(|| current.clone().unwrap_or_default());
    let mut busy = use_signal(|| false);
    let pill_class = if has_current {
        "ik-pill jade"
    } else {
        "ik-pill"
    };

    let save = {
        let provider = provider.clone();
        let mut reload = reload;
        move |_| {
            if *busy.peek() {
                return;
            }
            let ext = value.peek().trim().to_string();
            if ext.is_empty() {
                return;
            }
            busy.set(true);
            let provider = provider.clone();
            spawn(async move {
                if let Some(t) = session.token_value() {
                    if api::admin_upsert_sync_mapping(&t, series_id, &provider, &ext)
                        .await
                        .is_ok()
                    {
                        reload += 1;
                    }
                }
                busy.set(false);
            });
        }
    };

    let clear = {
        let provider = provider.clone();
        let mut reload = reload;
        move |_| {
            if *busy.peek() {
                return;
            }
            busy.set(true);
            let provider = provider.clone();
            spawn(async move {
                if let Some(t) = session.token_value() {
                    if api::admin_clear_sync_mapping(&t, series_id, &provider)
                        .await
                        .is_ok()
                    {
                        reload += 1;
                    }
                }
                busy.set(false);
            });
        }
    };

    rsx! {
        div { class: "ik-row",
            div { style: "min-width:120px;",
                span { class: "{pill_class}", "{provider_name}" }
            }
            input {
                class: "ik-input ik-mono",
                style: "flex:1;",
                r#type: "text",
                placeholder: "external id (e.g. AniList media id)",
                value: "{value}",
                oninput: move |e| value.set(e.value()),
            }
            button { class: "ik-btn primary", disabled: *busy.read(), onclick: save, "Save" }
            if has_current {
                button { class: "ik-btn", disabled: *busy.read(), onclick: clear, "Clear" }
            }
        }
    }
}

/// The assign queue: pick a provider, optionally filter by title, and hand-assign an external
/// id to any series the automatic matcher left unmapped (or open it in the inspector).
#[component]
fn AssignQueue(selected: Signal<Option<String>>, reload: Signal<u32>) -> Element {
    let session = use_session();
    let mut provider = use_signal(|| "anilist".to_string());
    let mut query = use_signal(String::new);

    let providers = use_resource(move || async move {
        match session.token_value() {
            Some(t) => api::sync_providers(&t).await,
            None => Ok(Vec::new()),
        }
    });

    let list = use_resource(move || {
        let p = provider.read().clone();
        let q = query.read().clone();
        let _ = reload.read();
        async move {
            match session.token_value() {
                Some(t) => api::admin_unmapped_series(&t, &p, Some(&q)).await,
                None => Ok(Vec::new()),
            }
        }
    });

    let prov_list: Vec<ProviderInfo> = match &*providers.read_unchecked() {
        Some(Ok(l)) => l.clone(),
        _ => Vec::new(),
    };

    let body = match &*list.read_unchecked() {
        None => rsx! { div { class: "ik-skeleton", style: "height:60px;" } },
        Some(Err(e)) => {
            let msg = e.clone();
            rsx! { ErrorBox { message: msg, on_retry: move |()| reload += 1 } }
        }
        Some(Ok(l)) if l.is_empty() => rsx! {
            div { class: "ik-empty", "Nothing unmapped for this provider — nice." }
        },
        Some(Ok(l)) => {
            let l = l.clone();
            let prov = provider.read().clone();
            rsx! {
                for s in l {
                    AssignRow { key: "{s.series_id}", series: s, provider: prov.clone(), selected, reload }
                }
            }
        }
    };

    rsx! {
        div { class: "ik-flex", style: "gap:8px;margin-bottom:10px;flex-wrap:wrap;",
            select {
                class: "ik-input",
                value: "{provider}",
                onchange: move |e| provider.set(e.value()),
                if prov_list.is_empty() {
                    option { value: "anilist", "anilist" }
                }
                for p in prov_list.clone() {
                    option { value: "{p.slug}", "{p.name}" }
                }
            }
            input {
                class: "ik-input",
                style: "flex:1;",
                r#type: "text",
                placeholder: "Filter unmapped by title…",
                value: "{query}",
                oninput: move |e| query.set(e.value()),
            }
        }
        {body}
    }
}

/// One assign-queue row: title + source count, an external-id input to Assign, and Inspect
/// to open the full editor.
#[component]
fn AssignRow(
    series: UnmappedSeries,
    provider: String,
    selected: Signal<Option<String>>,
    reload: Signal<u32>,
) -> Element {
    let session = use_session();
    let mut value = use_signal(String::new);
    let mut busy = use_signal(|| false);

    let assign = {
        let series_id = SeriesId(series.series_id);
        let provider = provider.clone();
        let mut reload = reload;
        move |_| {
            if *busy.peek() {
                return;
            }
            let ext = value.peek().trim().to_string();
            if ext.is_empty() {
                return;
            }
            busy.set(true);
            let provider = provider.clone();
            spawn(async move {
                if let Some(t) = session.token_value() {
                    if api::admin_upsert_sync_mapping(&t, series_id, &provider, &ext)
                        .await
                        .is_ok()
                    {
                        reload += 1;
                    }
                }
                busy.set(false);
            });
        }
    };

    let sid = series.series_id;
    rsx! {
        div { class: "ik-row",
            div { class: "grow",
                div { style: "font-weight:600;", "{series.series_title}" }
                div { class: "ik-muted", style: "font-size:12px;", "{series.source_count} sources · {provider}" }
            }
            input {
                class: "ik-input ik-mono",
                style: "width:200px;",
                r#type: "text",
                placeholder: "external id",
                value: "{value}",
                oninput: move |e| value.set(e.value()),
            }
            button { class: "ik-btn primary", disabled: *busy.read(), onclick: assign, "Assign" }
            button { class: "ik-btn", onclick: move |_| selected.set(Some(sid.to_string())), "Inspect" }
        }
    }
}

/// The reverse assign queue: pick a provider, optionally filter, and match every fetched
/// remote entry the auto-matcher could not confidently link to a local series.
#[component]
fn UnmatchedRemoteQueue(reload: Signal<u32>) -> Element {
    let session = use_session();
    let mut provider = use_signal(|| "anilist".to_string());
    let mut query = use_signal(String::new);

    let providers = use_resource(move || async move {
        match session.token_value() {
            Some(t) => api::sync_providers(&t).await,
            None => Ok(Vec::new()),
        }
    });

    let list = use_resource(move || {
        let p = provider.read().clone();
        let q = query.read().clone();
        let _ = reload.read();
        async move {
            match session.token_value() {
                Some(t) => api::admin_unmatched_remote(&t, &p, Some(&q)).await,
                None => Ok(Vec::new()),
            }
        }
    });

    let prov_list: Vec<ProviderInfo> = match &*providers.read_unchecked() {
        Some(Ok(l)) => l.clone(),
        _ => Vec::new(),
    };

    let body = match &*list.read_unchecked() {
        None => rsx! { div { class: "ik-skeleton", style: "height:60px;" } },
        Some(Err(e)) => {
            let msg = e.clone();
            rsx! { ErrorBox { message: msg, on_retry: move |()| reload += 1 } }
        }
        Some(Ok(l)) if l.is_empty() => rsx! {
            div { class: "ik-empty", "Every fetched entry for this provider is matched — nice." }
        },
        Some(Ok(l)) => {
            let l = l.clone();
            rsx! {
                for e in l {
                    UnmatchedRemoteRow { key: "{e.user_id}-{e.external_id}", entry: e, reload }
                }
            }
        }
    };

    rsx! {
        div { class: "ik-flex", style: "gap:8px;margin-bottom:10px;flex-wrap:wrap;",
            select {
                class: "ik-input",
                value: "{provider}",
                onchange: move |e| provider.set(e.value()),
                if prov_list.is_empty() {
                    option { value: "anilist", "anilist" }
                }
                for p in prov_list.clone() {
                    option { value: "{p.slug}", "{p.name}" }
                }
            }
            input {
                class: "ik-input",
                style: "flex:1;",
                r#type: "text",
                placeholder: "Filter unmatched by title…",
                value: "{query}",
                oninput: move |e| query.set(e.value()),
            }
        }
        {body}
    }
}

/// The canonical web URL for a fetched remote entry, so an operator can open the original
/// listing on the provider's site to compare it against local candidates. Only providers with
/// a known URL scheme return `Some`; `external_id` is the provider's media id.
fn provider_entry_url(provider: &str, external_id: &str) -> Option<String> {
    match provider {
        // AniList media ids resolve under /manga/ for every reading medium (manga/manhwa/
        // manhua/novel all live there), so a single scheme covers the tracker's content.
        "anilist" => Some(format!("https://anilist.co/manga/{external_id}")),
        _ => None,
    }
}

/// One reverse-queue row: shows the remote entry (with a link to open it on the provider),
/// automatic ranked match suggestions, and a manual search fallback. Every candidate can be
/// inspected in place (a full "manga info" card) before matching.
#[component]
fn UnmatchedRemoteRow(entry: UnmatchedRemoteEntry, reload: Signal<u32>) -> Element {
    let session = use_session();
    let mut search = use_signal(String::new);

    // Automatic suggestions from the server-side matcher, loaded once for this entry.
    let entry_title = entry.title.clone();
    let entry_ct = entry.content_type.clone();
    let entry_year = entry.start_year;
    let suggestions = use_resource(move || {
        let title = entry_title.clone();
        let ct = entry_ct.clone();
        async move {
            match session.token_value() {
                Some(t) => api::admin_suggest_matches(&t, &title, Some(&ct), entry_year).await,
                None => Ok(Vec::new()),
            }
        }
    });

    // Manual search fallback for the cases the matcher misses entirely.
    let results = use_resource(move || {
        let q = search.read().clone();
        async move {
            let q = q.trim().to_string();
            if q.len() < 3 {
                return Ok(Vec::new());
            }
            api::list_series(Some(&q), 8).await
        }
    });

    let suggested: Vec<SuggestedMatch> = match &*suggestions.read_unchecked() {
        Some(Ok(l)) => l.clone(),
        _ => Vec::new(),
    };
    let manual: Vec<SeriesSummary> = match &*results.read_unchecked() {
        Some(Ok(l)) => l.clone(),
        _ => Vec::new(),
    };

    let type_line = {
        let mut parts = vec![entry.status.clone()];
        if !entry.content_type.is_empty() && entry.content_type != "unknown" {
            parts.push(entry.content_type.clone());
        }
        if let Some(y) = entry.start_year {
            parts.push(y.to_string());
        }
        parts.push(format!("#{}", entry.external_id));
        parts.join(" · ")
    };

    let entry_url = provider_entry_url(&entry.provider, &entry.external_id);
    let suggestions_pending = (*suggestions.read_unchecked()).is_none();

    rsx! {
        div { class: "ik-row", style: "flex-direction:column;align-items:stretch;gap:8px;",
            div { class: "ik-flex", style: "justify-content:space-between;gap:8px;align-items:flex-start;",
                div { style: "min-width:0;",
                    div { style: "font-weight:600;", "{entry.title}" }
                    div { class: "ik-muted", style: "font-size:12px;", "{entry.username} · {type_line}" }
                }
                if let Some(url) = entry_url {
                    a {
                        class: "ik-btn",
                        style: "flex:0 0 auto;",
                        href: "{url}",
                        target: "_blank",
                        rel: "noopener noreferrer",
                        "Open on {entry.provider} ↗"
                    }
                }
            }

            div { class: "ik-muted", style: "font-size:11px;text-transform:uppercase;letter-spacing:.04em;",
                "Suggested matches"
            }
            if suggestions_pending {
                div { class: "ik-skeleton", style: "height:40px;" }
            } else if suggested.is_empty() {
                div { class: "ik-muted", style: "font-size:12px;",
                    "No automatic suggestions — search below to match by hand."
                }
            } else {
                div { class: "ik-flex", style: "flex-direction:column;gap:6px;",
                    for s in suggested {
                        CandidateMatchRow {
                            key: "sug-{s.series_id}",
                            series_id: SeriesId(s.series_id),
                            title: s.title.clone(),
                            meta: suggestion_meta(&s),
                            score: Some(s.score),
                            user_id: UserId(entry.user_id),
                            provider: entry.provider.clone(),
                            external_id: entry.external_id.clone(),
                            reload,
                        }
                    }
                }
            }

            input {
                class: "ik-input",
                r#type: "text",
                placeholder: "Or search local series to match by hand…",
                value: "{search}",
                oninput: move |e| search.set(e.value()),
            }
            if !manual.is_empty() {
                div { class: "ik-flex", style: "flex-direction:column;gap:6px;",
                    for c in manual {
                        CandidateMatchRow {
                            key: "man-{c.id}",
                            series_id: c.id,
                            title: c.title.clone(),
                            meta: format!("{} · {} src", c.content_type.label(), c.source_count),
                            score: None,
                            user_id: UserId(entry.user_id),
                            provider: entry.provider.clone(),
                            external_id: entry.external_id.clone(),
                            reload,
                        }
                    }
                }
            }
        }
    }
}

/// A short one-line descriptor for a suggested series (type · year · sources).
fn suggestion_meta(s: &SuggestedMatch) -> String {
    let mut parts = Vec::new();
    if !s.content_type.is_empty() && s.content_type != "unknown" {
        parts.push(s.content_type.clone());
    }
    if let Some(y) = s.release_year {
        parts.push(y.to_string());
    }
    parts.push(format!("{} src", s.source_count));
    parts.join(" · ")
}

/// One matchable candidate (from suggestions or manual search): shows the series, an optional
/// confidence score, an "Inspect" toggle that expands the full series info card so the entries
/// behind the suggested id can actually be reviewed, and a "Match" button that assigns it.
#[component]
fn CandidateMatchRow(
    series_id: SeriesId,
    title: String,
    meta: String,
    score: Option<f32>,
    user_id: UserId,
    provider: String,
    external_id: String,
    reload: Signal<u32>,
) -> Element {
    let session = use_session();
    let mut busy = use_signal(|| false);
    let mut show = use_signal(|| false);

    let match_it = {
        let provider = provider.clone();
        let external_id = external_id.clone();
        move |_| {
            if *busy.peek() {
                return;
            }
            busy.set(true);
            let provider = provider.clone();
            let external_id = external_id.clone();
            let mut reload = reload;
            spawn(async move {
                if let Some(t) = session.token_value() {
                    if api::admin_assign_remote_entry(
                        &t,
                        user_id,
                        &provider,
                        &external_id,
                        series_id,
                    )
                    .await
                    .is_ok()
                    {
                        reload += 1;
                    }
                }
                busy.set(false);
            });
        }
    };

    let score_badge = score.map(|s| {
        let pct = (s.clamp(0.0, 1.0) * 100.0).round() as i32;
        let cls = if s >= 0.85 {
            "ik-pill jade"
        } else if s >= 0.6 {
            "ik-pill"
        } else {
            "ik-pill vermilion"
        };
        (cls, pct)
    });

    let sid_for_card = series_id;
    let show_now = *show.read();

    rsx! {
        div { class: "ik-card", style: "padding:8px;",
            div { class: "ik-flex", style: "justify-content:space-between;gap:8px;align-items:center;",
                div { style: "min-width:0;",
                    div { style: "font-weight:600;", "{title}" }
                    div { class: "ik-muted", style: "font-size:11px;", "{meta}" }
                }
                div { class: "ik-flex", style: "gap:4px;align-items:center;flex:0 0 auto;",
                    if let Some((cls, pct)) = score_badge {
                        span { class: "{cls}", style: "font-size:11px;", "{pct}%" }
                    }
                    button {
                        class: "ik-btn",
                        onclick: move |_| show.set(!show_now),
                        if show_now { "Hide" } else { "Inspect" }
                    }
                    button {
                        class: "ik-btn primary",
                        disabled: *busy.read(),
                        onclick: match_it,
                        "Match"
                    }
                }
            }
            if show_now {
                div { style: "margin-top:8px;",
                    SeriesMiniCard { series_id: sid_for_card }
                }
            }
        }
    }
}

/// Group an integer with thousands separators (`12345` → `12,345`) for readable counts.
fn fmt_int(n: i64) -> String {
    let digits = n.unsigned_abs().to_string();
    let len = digits.len();
    let mut out = String::with_capacity(len + len / 3);
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (len - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    if n < 0 {
        format!("-{out}")
    } else {
        out
    }
}

/// Short prefix of a UUID (or any id newtype wrapping one) for compact display (e.g. run
/// scope).
fn short_id(id: impl std::fmt::Display) -> String {
    id.to_string().chars().take(8).collect()
}

/// The pill class + human label for a run state.
fn run_state_pill(state: RunState) -> (&'static str, &'static str) {
    match state {
        RunState::Completed => ("ik-pill jade", "Completed"),
        RunState::Running => ("ik-pill run", "Running"),
        RunState::Queued => ("ik-pill", "Queued"),
        RunState::Failed => ("ik-pill vermilion", "Failed"),
        RunState::Cancelled => ("ik-pill", "Cancelled"),
    }
}

/// Build the politeness JSON payload from the editor's string fields, or a human error.
fn politeness_json(
    rps: &str,
    concurrency: &str,
    crawl_delay_ms: &str,
    user_agent: &str,
) -> Result<serde_json::Value, String> {
    let rps: f64 = rps
        .trim()
        .parse()
        .map_err(|_| "Requests/sec must be a number.".to_owned())?;
    let concurrency: u32 = concurrency
        .trim()
        .parse()
        .map_err(|_| "Concurrency must be a whole number.".to_owned())?;
    let crawl_delay_ms: u64 = crawl_delay_ms
        .trim()
        .parse()
        .map_err(|_| "Crawl delay must be a whole number of milliseconds.".to_owned())?;
    Ok(serde_json::json!({
        "rps": rps,
        "concurrency": concurrency,
        "crawl_delay_ms": crawl_delay_ms,
        "user_agent": user_agent,
    }))
}

/// Pretty-print a stored adapter config for the editor textarea (empty / null → `{}`).
fn config_editor_text(v: &serde_json::Value) -> String {
    if v.is_null() {
        "{}".to_owned()
    } else {
        serde_json::to_string_pretty(v).unwrap_or_else(|_| "{}".to_owned())
    }
}
