//! Series detail (DESIGN_SPEC §7.3) — a blurred-cover **hero** (cover + type/status/year +
//! title + stat row + watchlist actions) over a `1fr 340px` **body grid**: synopsis +
//! chapter list on the left; a **Read on** source list, a **Tracking** card, and a
//! **Readers also follow** slot in the right sidebar.
//!
//! Fields the current API does not expose — rating, per-chapter read-state and read-% —
//! are **omitted gracefully** (never fabricated, per the links-&-metadata invariant).
//! Alt-titles, tags, and authors (§9.2) render when present. Related series need
//! `/v1/series/:id/related` (§9.3), so that slot is an honest placeholder.

use crate::api;
use crate::components::{Cover, ErrorBox};
use crate::icons::{Ic, Icon};
use crate::models::*;
use crate::state::use_session;
use crate::Route;
use dioxus::prelude::*;

/// The route gives us a plain `String` (see `crate::Route::Series`); parse it once here so
/// the rest of the view works with the real, compiler-checked `SeriesId`.
#[component]
pub fn Series(id: String) -> Element {
    let Ok(id) = id.parse::<SeriesId>() else {
        return rsx! {
            div { class: "ik-empty", "That series link doesn't look right." }
        };
    };

    let session = use_session();
    let nav = use_navigator();
    let selected = use_signal(|| Option::<SeriesSourceId>::None);
    let mut reload_wl = use_signal(|| 0u32);
    let reload_chapters = use_signal(|| 0u32);
    let api_client = api::use_api();

    let detail = {
        let client = api_client.clone();
        use_resource(move || {
            let client = client.clone();
            async move {
                client
                    .detail()
                    .id(id)
                    .send()
                    .await
                    .map(|r| r.into_inner())
                    .map_err(api::friendly_error)
            }
        })
    };

    let chapters = {
        let client = api_client.clone();
        use_resource(move || {
            let src = *selected.read();
            let _ = reload_chapters.read();
            let client = client.clone();
            async move {
                let mut builder = client.chapters().id(id);
                if let Some(s) = src {
                    builder = builder.source(s.to_string());
                }
                builder
                    .send()
                    .await
                    .map(|r| r.into_inner())
                    .map_err(api::friendly_error)
            }
        })
    };

    let watchlist = {
        let client = api_client.clone();
        use_resource(move || {
            let _ = reload_wl.read();
            let client = client.clone();
            async move {
                if session.is_authenticated() {
                    client
                        .watchlist()
                        .send()
                        .await
                        .map(|r| r.into_inner())
                        .map_err(api::friendly_error)
                } else {
                    Ok(Vec::new())
                }
            }
        })
    };

    let sync_status = {
        let client = api_client.clone();
        use_resource(move || {
            let client = client.clone();
            async move {
                if session.is_authenticated() {
                    api::fetch_json(&client, "/v1/me/sync/anilist/status")
                        .await
                        .ok()
                        .and_then(|v| serde_json::from_value::<SyncStatus>(v).ok())
                } else {
                    None
                }
            }
        })
    };

    let hero = match &*detail.read_unchecked() {
        None => rsx! {
            div { class: "ik-hero",
                div { class: "ik-skeleton ik-skel-cover" }
                div {
                    div { class: "ik-skeleton", style: "height:34px;width:60%;margin-bottom:12px;" }
                    div { class: "ik-skeleton", style: "height:14px;width:90%;margin-bottom:6px;" }
                    div { class: "ik-skeleton", style: "height:14px;width:80%;" }
                }
            }
        },
        Some(Err(e)) => {
            let msg = e.clone();
            rsx! {
                ErrorBox { message: msg, on_retry: move |()| reload_wl += 1 }
            }
        }
        Some(Ok(d)) => {
            let d = d.clone();
            let wl_entry = current_entry(&watchlist, id);
            let chapters_total: i64 = d.sources.iter().map(|s| i64::from(s.chapter_count)).sum();
            let source_count = d.sources.len();
            let bg = d.cover_url.clone().unwrap_or_default();
            rsx! {
                div { class: "ik-hero-wrap",
                    if !bg.is_empty() {
                        div { class: "ik-hero-bg", style: "background-image:url('{bg}');" }
                    }
                    button {
                        class: "ik-btn",
                        style: "margin-bottom:16px;",
                        onclick: move |_| { nav.go_back(); },
                        Ic { icon: Icon::Back, size: 16 }
                        "Back"
                    }
                    div { class: "ik-hero",
                        div { Cover { url: d.cover_url.clone(), title: d.title.clone() } }
                        div {
                            div { class: "ik-flex", style: "margin-bottom:10px;",
                                span { class: "ik-pill", style: "color:{type_color(d.content_type)};border-color:color-mix(in srgb,{type_color(d.content_type)} 55%,transparent);", "{d.content_type.label()}" }
                                span { class: "ik-flex", style: "gap:6px;",
                                    span { class: "ik-status-dot", style: "background:{status_color(d.status)};" }
                                    span { class: "ik-muted", style: "font-size:13px;", "{d.status.label()}" }
                                }
                                if let Some(y) = d.release_year {
                                    span { class: "ik-mono ik-muted", "· {y}" }
                                }
                            }
                            h1 { style: "font-family:var(--font-display);font-size:38px;font-weight:800;letter-spacing:-.02em;line-height:1.05;margin:0 0 6px;", "{d.title}" }
                            // Author/artist byline (§9.2) — shown only when present.
                            if !d.authors.is_empty() {
                                div {
                                    class: "ik-muted",
                                    style: "font-size:14px;margin:0 0 4px;",
                                    "by {d.authors.iter().map(|a| a.name.clone()).collect::<Vec<_>>().join(\", \")}"
                                }
                            }
                            // Alternative titles (§9.2) — shown only when present.
                            if !d.alt_titles.is_empty() {
                                div { class: "ik-muted", style: "font-size:14px;margin:0 0 8px;", "{d.alt_titles.join(\" · \")}" }
                            }
                            // Genre/tag chips (§9.2) — omitted gracefully when none.
                            if !d.tags.is_empty() {
                                div { class: "ik-chips", style: "margin:0 0 10px;",
                                    for tag in d.tags.clone() {
                                        span { key: "{tag.id}", class: "ik-chip", "{tag.name}" }
                                    }
                                }
                            }
                            div { class: "ik-stat-inline",
                                div { class: "item", Ic { icon: Icon::Layers, size: 16 } "{chapters_total} ch" }
                                div { class: "item", Ic { icon: Icon::Layers, size: 16 } "{source_count} sources" }
                            }
                            WatchControls {
                                series_id: id,
                                entry: wl_entry,
                                authed: session.is_authenticated(),
                                reload: reload_wl,
                            }
                        }
                    }
                }
            }
        }
    };

    // Sidebar sources + chapter list depend on loaded detail.
    let sources: Vec<SourceDto> = match &*detail.read_unchecked() {
        Some(Ok(d)) => d.sources.clone(),
        _ => Vec::new(),
    };
    let description: Option<String> = match &*detail.read_unchecked() {
        Some(Ok(d)) => d.description.clone(),
        _ => None,
    };
    let wl_entry_side = current_entry(&watchlist, id);
    let anilist_id: Option<String> = match &*detail.read_unchecked() {
        Some(Ok(d)) => d.anilist_id.clone(),
        _ => None,
    };

    let chapter_body = match &*chapters.read_unchecked() {
        None => rsx! {
            div { class: "ik-chapter-list",
                for _ in 0..6 {
                    div { class: "ik-chapter", div { class: "ik-skeleton", style: "height:14px;width:40%;" } }
                }
            }
        },
        Some(Err(e)) => {
            let msg = e.clone();
            rsx! { div { class: "ik-error", "Could not load chapters: {msg}" } }
        }
        Some(Ok(list)) if list.is_empty() => rsx! {
            div { class: "ik-empty", "No chapters indexed for this source yet." }
        },
        Some(Ok(list)) => {
            let list = list.clone();
            let groups = group_chapters(&list);
            rsx! {
                div { class: "ik-chapter-list",
                    for (i , g) in groups.into_iter().enumerate() {
                        ChapterGroupRow {
                            key: "{i}",
                            group: g,
                            series_id: id,
                            reload: reload_chapters,
                        }
                    }
                }
            }
        }
    };

    rsx! {
        {hero}
        div { class: "ik-body-grid", style: "margin-top:24px;",
            // Left column: synopsis + chapters.
            div {
                if let Some(desc) = description {
                    h3 { class: "ik-dayhead", "Synopsis" }
                    p { class: "ik-muted", style: "margin:0 0 22px;max-width:70ch;line-height:1.7;", "{desc}" }
                }
                h3 { class: "ik-dayhead", "Chapters" }
                {chapter_body}
            }
            // Right sidebar: read-on + tracking + related.
            div {
                div { class: "ik-sidebar-card",
                    h4 { "Read on" }
                    if sources.is_empty() {
                        div { class: "ik-muted", style: "font-size:13px;", "No sources linked yet." }
                    } else {
                        for s in sources {
                            SourceCard { key: "{s.id}", source: s, selected }
                        }
                    }
                }
                div { class: "ik-sidebar-card",
                    h4 { "Tracking" }
                    WatchControls {
                        series_id: id,
                        entry: wl_entry_side,
                        authed: session.is_authenticated(),
                        reload: reload_wl,
                    }
                    div { class: "ik-flex", style: "margin-top:12px;font-size:13px;justify-content:space-between;",
                        match anilist_id {
                            Some(aid) => rsx! {
                                a {
                                    class: "ik-flex ik-icon-link",
                                    style: "gap:6px;",
                                    href: "https://anilist.co/manga/{aid}",
                                    target: "_blank",
                                    rel: "noopener",
                                    title: "View on AniList",
                                    "AniList"
                                    Ic { icon: Icon::OpenInNew, size: 13 }
                                }
                            },
                            None => rsx! { span { "AniList" } },
                        }
                        match &*sync_status.read_unchecked() {
                            Some(Some(s)) if s.linked => rsx! {
                                span { class: "ik-flex", style: "gap:4px;color:var(--jade,#3DA88F);",
                                    Ic { icon: Icon::CloudDone, size: 15 }
                                    "Synced"
                                }
                            },
                            Some(_) => rsx! {
                                Link { to: Route::Account {}, class: "ik-flex", style: "gap:4px;color:inherit;text-decoration:none;",
                                    Ic { icon: Icon::CloudOff, size: 15 }
                                    span { class: "ik-muted", "Not connected" }
                                }
                            },
                            None => rsx! { span { class: "ik-muted", "…" } },
                        }
                    }
                }
                div { class: "ik-sidebar-card",
                    h4 { "Readers also follow" }
                    // TODO(api) §9.3: needs GET /v1/series/:id/related.
                    div { class: "ik-muted", style: "font-size:13px;", "Recommendations arrive with the related-series endpoint." }
                }
            }
        }
    }
}

/// Find this series' watchlist entry (if any) from the loaded watchlist resource.
fn current_entry(
    watchlist: &Resource<Result<Vec<WatchlistItem>, String>>,
    series_id: SeriesId,
) -> Option<WatchlistItem> {
    match &*watchlist.read_unchecked() {
        Some(Ok(list)) => list.iter().find(|i| i.series_id == series_id).cloned(),
        _ => None,
    }
}

fn type_color(t: ContentType) -> &'static str {
    match t {
        ContentType::Manga => "#6FA8DC",
        ContentType::Manhwa => "var(--acc2)",
        ContentType::Manhua => "#3DA88F",
        ContentType::Webtoon => "#CBA43C",
        ContentType::Unknown => "var(--muted)",
    }
}

fn status_color(s: SeriesStatus) -> &'static str {
    match s {
        SeriesStatus::Ongoing => "#3DA88F",
        SeriesStatus::Completed => "#6FA8DC",
        SeriesStatus::Hiatus => "#CBA43C",
        SeriesStatus::Cancelled | SeriesStatus::Unknown => "var(--muted)",
    }
}

/// A "Read on" source card in the sidebar. Selecting it loads that source's chapters in the
/// left column; the trailing link opens the resolved source page in a new tab.
#[component]
fn SourceCard(source: SourceDto, selected: Signal<Option<SeriesSourceId>>) -> Element {
    let mut selected = selected;
    let is_selected = *selected.read() == Some(source.id);
    let sid = source.id;
    let initial = source
        .provider_name
        .chars()
        .next()
        .unwrap_or('?')
        .to_uppercase()
        .to_string();
    let border = if is_selected {
        "border-color:var(--acc);"
    } else {
        ""
    };
    rsx! {
        div { class: "ik-source-card",
            button {
                class: "ik-flex",
                style: "flex:1;background:none;border:none;cursor:pointer;color:inherit;text-align:left;{border}",
                onclick: move |_| selected.set(Some(sid)),
                div { class: "ik-source-tile", "{initial}" }
                div { class: "grow",
                    div { class: "ik-flex", style: "gap:6px;",
                        span { style: "font-weight:600;font-size:13px;", "{source.provider_name}" }
                        if source.is_primary {
                            span { class: "ik-pill jade", style: "font-size:10px;", "Primary" }
                        }
                    }
                    div { class: "ik-mono ik-muted", style: "font-size:11px;", "{source.chapter_count} ch" }
                }
            }
            a { class: "ik-btn-icon ik-btn", href: "{source.url}", target: "_blank", rel: "noopener", title: "Open source",
                Ic { icon: Icon::OpenInNew, size: 16 }
            }
        }
    }
}

#[component]
fn WatchControls(
    series_id: SeriesId,
    entry: Option<WatchlistItem>,
    authed: bool,
    reload: Signal<u32>,
) -> Element {
    let session = use_session();
    let api_client = api::use_api();

    if !authed {
        return rsx! {
            span { class: "ik-muted", "Sign in to track this series." }
        };
    }

    let in_list = entry.is_some();
    let notify = entry.as_ref().map(|e| e.notify).unwrap_or(true);
    let sync_excluded = entry.as_ref().map(|e| e.sync_excluded).unwrap_or(false);
    let status = entry
        .as_ref()
        .map(|e| e.status)
        .unwrap_or(WatchStatus::Reading);

    // The per-title sync opt-out (design v2 §B.8) is only meaningful once the user has linked
    // at least one external provider — otherwise there is nothing to opt out of.
    let has_linked = {
        let client = api_client.clone();
        use_resource(move || {
            let client = client.clone();
            async move {
                if session.is_authenticated() {
                    let Ok(value) = api::fetch_json(&client, "/v1/me/sync/providers").await else {
                        return false;
                    };
                    let list: Vec<ProviderInfo> = serde_json::from_value(value).unwrap_or_default();
                    for p in list {
                        if p.linked {
                            return true;
                        }
                        let slug = if p.slug.is_empty() {
                            p.id.clone()
                        } else {
                            p.slug.clone()
                        };
                        let path = format!("/v1/me/sync/{slug}/status");
                        if let Ok(v) = api::fetch_json(&client, &path).await {
                            if serde_json::from_value::<SyncStatus>(v)
                                .map(|s| s.linked)
                                .unwrap_or(false)
                            {
                                return true;
                            }
                        }
                    }
                    false
                } else {
                    false
                }
            }
        })
    };
    let show_sync_toggle = in_list && matches!(&*has_linked.read_unchecked(), Some(true));

    let toggle_sync = {
        let sid = series_id;
        let mut reload = reload;
        let client = api_client.clone();
        move |_| {
            let client = client.clone();
            spawn(async move {
                let body = SyncExcluded {
                    excluded: !sync_excluded,
                };
                if client
                    .put_sync_excluded()
                    .series_id(sid)
                    .body(body)
                    .send()
                    .await
                    .is_ok()
                {
                    reload += 1;
                }
            });
        }
    };

    let add = {
        let sid = series_id;
        let mut reload = reload;
        let client = api_client.clone();
        move |_| {
            let client = client.clone();
            spawn(async move {
                let body = WatchlistUpsert {
                    status: Some(WatchStatus::Reading),
                    notify: Some(true),
                };
                if client
                    .put_watchlist()
                    .series_id(sid)
                    .body(body)
                    .send()
                    .await
                    .is_ok()
                {
                    reload += 1;
                }
            });
        }
    };

    let remove = {
        let sid = series_id;
        let mut reload = reload;
        let client = api_client.clone();
        move |_| {
            let client = client.clone();
            spawn(async move {
                if client
                    .delete_watchlist()
                    .series_id(sid)
                    .send()
                    .await
                    .is_ok()
                {
                    reload += 1;
                }
            });
        }
    };

    let toggle_notify = {
        let sid = series_id;
        let mut reload = reload;
        let client = api_client.clone();
        move |_| {
            let client = client.clone();
            spawn(async move {
                let body = WatchlistUpsert {
                    status: Some(status),
                    notify: Some(!notify),
                };
                if client
                    .put_watchlist()
                    .series_id(sid)
                    .body(body)
                    .send()
                    .await
                    .is_ok()
                {
                    reload += 1;
                }
            });
        }
    };

    rsx! {
        div { class: "ik-flex", style: "flex-wrap:wrap;",
            if in_list {
                button { class: "ik-btn", onclick: remove,
                    Ic { icon: Icon::Bookmark, size: 16 }
                    "In watchlist"
                }
                button {
                    class: if notify { "ik-btn primary" } else { "ik-btn" },
                    onclick: toggle_notify,
                    Ic { icon: Icon::Notify, size: 16 }
                    if notify { "Notify on" } else { "Notify off" }
                }
            } else {
                button { class: "ik-btn primary", onclick: add,
                    Ic { icon: Icon::Bookmark, size: 16 }
                    "Add to watchlist"
                }
            }
            if show_sync_toggle {
                button {
                    class: if sync_excluded { "ik-btn" } else { "ik-btn primary" },
                    onclick: toggle_sync,
                    Ic { icon: Icon::CloudSync, size: 16 }
                    if sync_excluded { "Sync off" } else { "Sync on" }
                }
            }
        }
    }
}

#[component]
fn ChapterRow(
    chapter: crate::models::ChapterDto,
    series_id: SeriesId,
    reload: Signal<u32>,
    /// True for a sub-chapter part release (fractional `number`, e.g. `152.6`) — sources
    /// sometimes ship these ahead of the compiled full chapter. Styled distinctly and never
    /// counted as a full chapter for tracking (§ chapter grouping).
    #[props(default = false)]
    is_part: bool,
) -> Element {
    let api_client = api::use_api();
    let mut reload = reload;
    let mut busy = use_signal(|| false);

    let num = trim_num(chapter.number);
    let label = chapter
        .title
        .clone()
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| {
            if is_part {
                format!("Part {num}")
            } else {
                format!("Chapter {num}")
            }
        });
    let date = chapter
        .published_at
        .as_deref()
        .and_then(|p| p.get(0..10))
        .unwrap_or("")
        .to_owned();
    let url = chapter.url.clone();
    // Auth-scoped read-state (§9.2): `Some(true)` dims the row + shows a check; anonymous
    // callers get `None` and the row renders unmarked with no mark-read control — there's
    // nothing to track without a signed-in session.
    let is_read = chapter.read.unwrap_or(false);
    let can_track = chapter.read.is_some();
    let row_style = if is_read { "opacity:.55;" } else { "" };
    let row_class = if is_part {
        "ik-chapter part"
    } else {
        "ik-chapter"
    };

    // Per-chapter mark read/unread (design v2 §A.3): the dedicated endpoint applies the
    // two-scalar rule server-side, correctly advancing/retreating either the whole-chapter or
    // the part frontier for this exact `number` — a part release never corrupts whole-chapter
    // progress, and unmarking retreats to the previous chapter without assuming contiguity.
    let chapter_number = chapter.number;
    let mark = move |_| {
        if *busy.peek() {
            return;
        }
        busy.set(true);
        let client = api_client.clone();
        spawn(async move {
            let body = ChapterRead { read: !is_read };
            if client
                .put_chapter_progress()
                .series_id(series_id)
                .number(chapter_number)
                .body(body)
                .send()
                .await
                .is_ok()
            {
                reload += 1;
            }
            busy.set(false);
        });
    };

    rsx! {
        div { class: "{row_class}", style: "{row_style}",
            span { class: "num", "#{num}" }
            if is_part {
                span { class: "ik-part-pill", "Part" }
            }
            span { "{label}" }
            if is_read {
                span { class: "ik-flex ik-muted", style: "gap:4px;font-size:11px;",
                    Ic { icon: Icon::Check, size: 13 }
                    "Read"
                }
            }
            span { class: "date", "{date}" }
            if can_track {
                button {
                    class: "ik-btn",
                    style: "margin-left:12px;padding:4px 10px;",
                    disabled: *busy.read(),
                    onclick: mark,
                    if is_read { "Mark unread" } else { "Mark read" }
                }
            }
            a {
                class: "ik-btn",
                style: "margin-left:8px;padding:4px 10px;",
                href: "{url}",
                target: "_blank",
                rel: "noopener",
                "Open"
            }
        }
    }
}

/// One visual row-group in the chapter list, keyed by whole chapter number. Sources
/// sometimes ship sub-chapter part releases (fractional `number`, e.g. `152.1`..`152.6`)
/// ahead of the compiled full chapter. Until the full chapter appears, its parts render
/// directly as the visible frontier; once it appears, the parts collapse into a
/// dropdown nested under it rather than cluttering the main list.
#[derive(Clone, PartialEq)]
struct ChapterGroup {
    full: Option<crate::models::ChapterDto>,
    /// Part releases sharing this group's whole-chapter number, newest (highest) first.
    parts: Vec<crate::models::ChapterDto>,
}

/// Groups a newest-first chapter list by `floor(number)`. Relies on the list being sorted
/// descending by `number`, which guarantees every part release sorts directly above its whole
/// chapter (e.g. `152.6` > `152.1` > `152`), so same-group rows are always contiguous.
fn group_chapters(list: &[crate::models::ChapterDto]) -> Vec<ChapterGroup> {
    let mut groups: Vec<(i64, ChapterGroup)> = Vec::new();
    for chapter in list.iter().cloned() {
        let key = chapter.number.floor() as i64;
        let is_full = chapter.number.fract() == 0.0;
        match groups.last_mut() {
            Some((k, g)) if *k == key => {
                if is_full {
                    g.full = Some(chapter);
                } else {
                    g.parts.push(chapter);
                }
            }
            _ => {
                let mut g = ChapterGroup {
                    full: None,
                    parts: Vec::new(),
                };
                if is_full {
                    g.full = Some(chapter);
                } else {
                    g.parts.push(chapter);
                }
                groups.push((key, g));
            }
        }
    }
    groups.into_iter().map(|(_, g)| g).collect()
}

#[component]
fn ChapterGroupRow(group: ChapterGroup, series_id: SeriesId, reload: Signal<u32>) -> Element {
    let mut expanded = use_signal(|| false);
    let has_full = group.full.is_some();
    let parts = group.parts.clone();

    // Ascending range for the toggle label ("152.1–152.6"): `parts` is newest-first, so the
    // lowest number is last and the highest is first.
    let lo = parts.last().map(|c| c.number).unwrap_or_default();
    let hi = parts.first().map(|c| c.number).unwrap_or_default();
    let count = parts.len();
    let toggle_label = format!(
        "{count} early part release{} ({}\u{2013}{})",
        if count == 1 { "" } else { "s" },
        trim_num(lo),
        trim_num(hi)
    );

    rsx! {
        if let Some(c) = group.full.clone() {
            ChapterRow {
                chapter: c,
                series_id: series_id,
                reload,
                is_part: false,
            }
        }
        // Only a whole chapter with parts nested under it gets a toggle — a lone full
        // chapter or lone parts (no full chapter released yet) render with no affordance.
        if has_full && !parts.is_empty() {
            div {
                class: "ik-chapter-toggle",
                onclick: move |_| { let v = !*expanded.read(); expanded.set(v); },
                Ic {
                    icon: Icon::ChevronRight,
                    size: 14,
                    class: if *expanded.read() { "ik-chevron open" } else { "ik-chevron" },
                }
                span { "{toggle_label}" }
            }
        }
        // No full chapter yet: the parts are the reading frontier, so they stay visible
        // rather than collapsed.
        if !has_full || *expanded.read() {
            for c in parts.iter().cloned() {
                ChapterRow {
                    key: "{c.number}",
                    chapter: c,
                    series_id: series_id,
                    reload,
                    is_part: true,
                }
            }
        }
    }
}

fn trim_num(n: f64) -> String {
    if n.fract() == 0.0 {
        format!("{}", n as i64)
    } else {
        format!("{n}")
    }
}
