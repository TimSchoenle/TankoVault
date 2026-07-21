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
use crate::components::{rel_time, ErrorBox};
use crate::icons::{Ic, Icon};
use crate::models::{
    AuditEntry, FailedTask, MergeCandidate, Provider, ProviderStat, RunState, ScanMode, ScanRun,
    SystemStats,
};
use crate::state::use_session;
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
    Users,
    Audit,
}

impl ConsoleTab {
    const ALL: [ConsoleTab; 8] = [
        Self::Overview,
        Self::LiveScans,
        Self::Providers,
        Self::Solver,
        Self::AdapterTest,
        Self::Merge,
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
                                HealthPill { state: p.state.clone() }
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

    let id = provider.id.clone();
    let original_base = provider.base_url.clone();
    let is_disabled = provider.state == "disabled";

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
    let mut rps = use_signal(|| provider.politeness.rps.to_string());
    let mut concurrency = use_signal(|| provider.politeness.concurrency.to_string());
    let mut crawl_delay_ms = use_signal(|| provider.politeness.crawl_delay_ms.to_string());
    let mut user_agent = use_signal(|| provider.politeness.user_agent.clone());

    // Commits the edit. Cloneable so both the direct-save and the confirm-migration
    // buttons can drive it without duplicating the request logic.
    let saver = {
        let id = id.clone();
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
            let id = id.clone();
            confirm_migrate.set(false);
            busy.set(true);
            msg.set(None);
            let mut reload = reload;
            spawn(async move {
                let outcome = match session.token_value() {
                    Some(t) => api::update_provider(&t, &id, &name_v, &base_v, &cfg, &pol).await,
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
        let mut saver = saver.clone();
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
        let mut saver = saver.clone();
        move |_| saver()
    };

    let toggle_state = {
        let id = id.clone();
        move |_| {
            let id = id.clone();
            let target = if is_disabled { "active" } else { "disabled" };
            let mut reload = reload;
            busy.set(true);
            msg.set(None);
            spawn(async move {
                if let Some(t) = session.token_value() {
                    if let Err(e) = api::set_provider_state(&t, &id, target).await {
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
        let id = id.clone();
        move |_| {
            let id = id.clone();
            let m = *scan_mode.read();
            msg.set(None);
            spawn(async move {
                if let Some(t) = session.token_value() {
                    match api::trigger_scan(&t, Some(&id), m).await {
                        Ok(_) => msg.set(Some("Scan queued for this provider.".to_owned())),
                        Err(e) => msg.set(Some(e)),
                    }
                }
            });
        }
    };

    let delete = {
        let id = id.clone();
        move |_| {
            let id = id.clone();
            let mut reload = reload;
            busy.set(true);
            spawn(async move {
                let outcome = match session.token_value() {
                    Some(t) => api::delete_provider(&t, &id).await,
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
    let test_id = id.clone();

    rsx! {
        div { class: "ik-tile", style: "margin-bottom:12px;",
            div { class: "ik-flex", style: "justify-content:space-between;align-items:flex-start;gap:12px;",
                div { class: "grow",
                    div { class: "ik-flex",
                        span { style: "font-weight:600;", "{provider.name}" }
                        HealthPill { state: provider.state.clone() }
                    }
                    div { class: "ik-muted ik-mono", style: "font-size:12px;margin-top:2px;word-break:break-all;",
                        "{provider.slug} · {provider.adapter} · {provider.base_url}"
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
fn AdapterTestPanel(provider_id: String) -> Element {
    let session = use_session();
    let mut path = use_signal(String::new);
    let mut running = use_signal(|| false);
    let mut result = use_signal(|| Option::<Result<serde_json::Value, String>>::None);

    let run = {
        let provider_id = provider_id.clone();
        move |_| {
            let provider_id = provider_id.clone();
            let p = path.read().trim().to_owned();
            running.set(true);
            spawn(async move {
                let out = match session.token_value() {
                    Some(t) => {
                        let path_opt = if p.is_empty() { None } else { Some(p.as_str()) };
                        api::test_adapter(&t, &provider_id, path_opt).await
                    }
                    None => Err("You are signed out.".to_owned()),
                };
                result.set(Some(out));
                running.set(false);
            });
        }
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
    let id = provider.id.clone();
    let blocked = matches!(
        provider.state.as_str(),
        "blocked" | "disabled" | "challenged"
    );

    let resolve = {
        let id = id.clone();
        move |_| {
            let id = id.clone();
            let mut reload = reload;
            spawn(async move {
                if let Some(t) = session.token_value() {
                    if api::resolve_provider(&t, &id).await.is_ok() {
                        reload += 1;
                    }
                }
            });
        }
    };
    let reenable = {
        let id = id.clone();
        move |_| {
            let id = id.clone();
            let mut reload = reload;
            spawn(async move {
                if let Some(t) = session.token_value() {
                    if api::set_provider_state(&t, &id, "active").await.is_ok() {
                        reload += 1;
                    }
                }
            });
        }
    };

    rsx! {
        div { class: "ik-row",
            div { class: "grow",
                div { style: "font-weight:600;", "{provider.name}" }
                div { class: "ik-mono ik-muted", style: "font-size:12px;", "{provider.base_url}" }
            }
            HealthPill { state: provider.state.clone() }
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
                            option { value: "{p.id}", selected: sel.as_deref() == Some(p.id.as_str()), "{p.name}" }
                        }
                    }
                }
                if let Some(pid) = chosen.read().clone() {
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
    let id = candidate.id.clone();
    let a = candidate.series_id.clone();
    let b = candidate.candidate_id.clone();

    let merge = {
        let (a, b) = (a.clone(), b.clone());
        let mut reload = reload;
        move |_| {
            let (a, b) = (a.clone(), b.clone());
            spawn(async move {
                if let Some(t) = session.token_value() {
                    if api::merge_series(&t, &a, &b).await.is_ok() {
                        reload += 1;
                    }
                }
            });
        }
    };

    let dismiss = {
        let id = id.clone();
        let mut reload = reload;
        move |_| {
            let id = id.clone();
            spawn(async move {
                if let Some(t) = session.token_value() {
                    if api::dismiss_candidate(&t, &id).await.is_ok() {
                        reload += 1;
                    }
                }
            });
        }
    };

    rsx! {
        div { class: "ik-row",
            div { class: "grow",
                div { class: "ik-flex", style: "justify-content:space-between;",
                    span { style: "font-weight:600;", "{candidate.series_title}" }
                    span { class: "ik-mono ik-muted", "{pct}% match" }
                }
                div { class: "ik-muted", style: "font-size:13px;", "↔ {candidate.candidate_title}" }
            }
            button { class: "ik-btn primary", onclick: merge, "Merge" }
            button { class: "ik-btn", onclick: dismiss, "Distinct" }
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

/// Short prefix of a UUID for compact display (e.g. run scope).
fn short_id(id: &str) -> String {
    id.chars().take(8).collect()
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
