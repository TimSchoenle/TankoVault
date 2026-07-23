//! Reusable Inkstone UI components (design §17.3/§17.4): the app shell (left rail + top
//! command bar), cover cards, loading skeletons, and named empty/error states.

use crate::api;
use crate::icons::{Ic, Icon};
use crate::models::SeriesSummary;
use crate::state::use_session;
use crate::Route;
use dioxus::prelude::*;

/// How long before the access token's `exp` the background refresh in [`Shell`] fires.
/// Comfortably inside the server's 15-minute default `access_ttl_minutes`.
const REFRESH_BUFFER_MS: f64 = 60_000.0;

/// Poll cadence for the background refresh loop while signed out (no cookie to adopt yet).
const SIGNED_OUT_POLL_MS: u32 = 15_000;

/// The persistent app shell: left rail nav + top command bar, with the routed view in the
/// content area (via `Outlet`). Also performs the boot-time and recurring silent refresh.
#[component]
pub fn Shell() -> Element {
    let session = use_session();
    let route: Route = use_route();

    // Silent refresh once on boot (adopt an access token from the httpOnly cookie if a
    // session already exists, so a page reload keeps the user signed in), then again shortly
    // before each access token expires, for as long as the tab stays open (design §17.4).
    //
    // Without the recurring half, the in-memory token would go stale ~15 min after boot/sign-in
    // (server default `access_ttl_minutes`) and every authenticated call — including the SSE
    // stream below — starts failing with 401 until the user manually reloads the page: a
    // reload is the only thing that re-runs the boot refresh. Worse for the SSE stream
    // specifically, since the browser's `EventSource` bakes the token into its URL and, per
    // spec, gives up reconnecting for good the first time a reconnect attempt gets a non-200
    // response — so a stale-token 401 kills it permanently, not just until the next retry.
    // Refreshing before expiry keeps `session`'s token current, which — since the stream
    // below is keyed on it — transparently tears down and re-opens the connection with a
    // valid token before that can happen.
    use_future(move || async move {
        loop {
            let booted = *session.ready.peek();
            let wait_ms = match session.token_expires_in_ms() {
                Some(ms) if ms > REFRESH_BUFFER_MS => ms - REFRESH_BUFFER_MS,
                Some(_) => 0.0,
                // Signed out and we've already tried once: nothing to refresh; poll for a
                // sign-in rather than hammering the endpoint.
                None if booted => {
                    gloo_timers::future::TimeoutFuture::new(SIGNED_OUT_POLL_MS).await;
                    continue;
                }
                None => 0.0,
            };
            gloo_timers::future::TimeoutFuture::new(wait_ms as u32).await;

            match api::refresh().await {
                Ok(tok) => session.set_token(tok.access_token),
                Err(_) => session.clear(),
            }
            if !booted {
                session.mark_ready();
            }
        }
    });

    let unread = use_context::<UnreadBadge>();

    // Live notifications: while signed in, subscribe to the SSE stream and keep the rail's
    // unread badge current (design §14, §17.4). `use_resource` restarts when the token
    // changes — dropping the previous `EventSource` and closing its connection — so a
    // sign-out or refresh transparently re-establishes (or tears down) the stream.
    use_resource(move || {
        let token = session.token_value();
        async move {
            if let Some(token) = token {
                crate::live::run(token, unread).await;
            }
        }
    });

    // Apply the persisted appearance knobs once on boot (the Appearance panel writes the
    // `tv-*` keys; DESIGN_SPEC §8). Theme falls back to the OS `prefers-color-scheme` on the
    // first visit; accent/density/cover stay unset (→ vermilion/standard/ink defaults).
    use_effect(|| {
        let _ = document::eval(
            "var d=document.documentElement;\
             var t=localStorage.getItem('tv-theme');\
             if(!t){t=(window.matchMedia&&window.matchMedia('(prefers-color-scheme: light)').matches)?'light':'dark';}\
             d.setAttribute('data-theme',t);\
             var a=localStorage.getItem('tv-accent'); if(a){d.setAttribute('data-accent',a);}\
             var de=localStorage.getItem('tv-density'); if(de){d.setAttribute('data-density',de);}\
             var c=localStorage.getItem('tv-cover'); if(c){d.setAttribute('data-cover',c);}",
        );
    });

    let unread_count = *unread.0.read();
    let is_operator = session.role.read().is_operator();

    rsx! {
        div { class: "ik-app",
            nav { class: "ik-rail",
                // Brand lockup.
                div { class: "ik-brand",
                    div { class: "ik-brand-tile", Ic { icon: Icon::MenuBook, size: 22 } }
                    div {
                        div { class: "ik-wordmark",
                            "Tankō"
                            span { class: "acc", "Vault" }
                        }
                        div { class: "ik-brand-tag", "SOURCE · TRACK · SYNC" }
                    }
                }

                NavGroup { label: "MAIN" }
                NavLink { to: Route::Home {}, label: "Home", icon: Icon::Home, current: route.clone(), badge: 0 }
                NavLink { to: Route::Discover {}, label: "Discover", icon: Icon::Explore, current: route.clone(), badge: 0 }
                NavLink { to: Route::Search { q: String::new() }, label: "Search", icon: Icon::Search, current: route.clone(), badge: 0 }

                NavGroup { label: "LIBRARY" }
                NavLink { to: Route::Watchlist {}, label: "Watchlist", icon: Icon::Watchlist, current: route.clone(), badge: 0 }
                NavLink {
                    to: Route::Notifications {},
                    label: "Notifications",
                    icon: Icon::Notifications,
                    current: route.clone(),
                    badge: unread_count,
                }

                if is_operator {
                    NavGroup { label: "OPERATOR" }
                    NavLink { to: Route::Console {}, label: "Console", icon: Icon::Console, current: route.clone(), badge: 0 }
                    NavLink { to: Route::Account {}, label: "Account", icon: Icon::Account, current: route.clone(), badge: 0 }
                } else {
                    NavGroup { label: "ACCOUNT" }
                    NavLink { to: Route::Account {}, label: "Account", icon: Icon::Account, current: route.clone(), badge: 0 }
                }

                div { class: "ik-rail-spacer" }
                UserFooter {}
            }
            main { class: "ik-main",
                TopBar {}
                section { class: "ik-content", Outlet::<Route> {} }
            }
        }
    }
}

/// A kicker heading that groups rail destinations (`MAIN` / `LIBRARY` / `OPERATOR`).
#[component]
fn NavGroup(label: String) -> Element {
    rsx! {
        div { class: "ik-navgroup",
            div { class: "ik-navgroup-label", "{label}" }
        }
    }
}

/// A left-rail navigation entry with an icon, label and the animated active bar.
#[component]
fn NavLink(to: Route, label: String, icon: Icon, current: Route, badge: i64) -> Element {
    let is_active = same_screen(&to, &current);
    let class = if is_active {
        "ik-nav-link active"
    } else {
        "ik-nav-link"
    };
    rsx! {
        Link { to: to.clone(), class: "{class}",
            Ic { icon, size: 18 }
            span { class: "label", "{label}" }
            if badge > 0 {
                span { class: "ik-nav-badge", "{badge}" }
            }
        }
    }
}

/// Whether two routes belong to the same top-level rail destination.
fn same_screen(a: &Route, b: &Route) -> bool {
    use std::mem::discriminant;
    // Series detail lives under Discover in the rail's mental model.
    let norm = |r: &Route| match r {
        Route::Series { .. } => Route::Discover {},
        other => other.clone(),
    };
    discriminant(&norm(a)) == discriminant(&norm(b))
}

/// Top command bar: instant-search (Enter → Search, `⌘K`/`Ctrl+K` focuses it), an
/// AniList-sync status pill (stub — no endpoint yet), and the notifications bell.
#[component]
fn TopBar() -> Element {
    let nav = use_navigator();
    let session = use_session();
    let unread = use_context::<UnreadBadge>();
    let unread_count = *unread.0.read();
    let mut query = use_signal(String::new);

    // Global ⌘K / Ctrl+K focuses the search field.
    use_effect(|| {
        let _ = document::eval(
            "document.addEventListener('keydown',function(e){if((e.metaKey||e.ctrlKey)&&e.key==='k'){e.preventDefault();var el=document.getElementById('tv-search');if(el){el.focus();}}});",
        );
    });

    let signed_in = session.is_authenticated();

    // AniList link status for the header pill: real, not a stub — reflects
    // `GET /v1/me/sync/anilist/status` rather than a hardcoded "synced" claim.
    let sync_status = use_resource(move || async move {
        match session.token_value() {
            Some(t) => api::sync_status(&t, "anilist").await.ok(),
            None => None,
        }
    });
    let synced = matches!(
        &*sync_status.read_unchecked(),
        Some(Some(s)) if s.linked
    );

    rsx! {
        header { class: "ik-topbar",
            div { class: "ik-search",
                span { class: "lead", Ic { icon: Icon::Search, size: 16 } }
                input {
                    id: "tv-search",
                    class: "ik-input",
                    r#type: "search",
                    placeholder: "Search series, tags, authors…",
                    value: "{query}",
                    oninput: move |e| query.set(e.value()),
                    onkeydown: move |e| {
                        if e.key() == Key::Enter {
                            let q = query.read().trim().to_owned();
                            if !q.is_empty() {
                                nav.push(Route::Search { q });
                            }
                        }
                    },
                }
                span { class: "kbd", "⌘K" }
            }
            div { class: "ik-topbar-actions",
                if signed_in {
                    Link {
                        to: Route::Account {},
                        class: if synced { "ik-pill jade" } else { "ik-pill" },
                        style: "display:inline-flex;align-items:center;gap:6px;text-decoration:none;",
                        Ic { icon: if synced { Icon::CloudDone } else { Icon::CloudOff }, size: 13 }
                        if synced { "AniList synced" } else { "Connect AniList" }
                    }
                    Link { to: Route::Notifications {}, class: "ik-bell",
                        Ic { icon: Icon::Notifications, size: 18 }
                        if unread_count > 0 {
                            span { class: "dot", "{unread_count}" }
                        }
                    }
                }
            }
        }
    }
}

/// The rail user footer: avatar + identity + settings gear when signed in; a "Sign in"
/// primary button otherwise. Sign-out lives on the Account screen.
#[component]
fn UserFooter() -> Element {
    let session = use_session();
    if session.is_authenticated() {
        let name = session.username().unwrap_or_else(|| "reader".to_owned());
        let initial = name
            .chars()
            .next()
            .unwrap_or('?')
            .to_uppercase()
            .to_string();
        let role = *session.role.read();
        let status = if role.is_admin() {
            "admin · synced"
        } else if role.is_operator() {
            "operator · synced"
        } else {
            "reader · synced"
        };
        return rsx! {
            div { class: "ik-userbox",
                div { class: "ik-avatar", "{initial}" }
                div { class: "who",
                    div { class: "name", "{name}" }
                    div { class: "sub",
                        span { class: "ik-status-dot" }
                        "{status}"
                    }
                }
                Link { to: Route::Account {}, class: "gear", Ic { icon: Icon::Settings, size: 18 } }
            }
        };
    }
    rsx! {
        div { style: "padding:8px;",
            Link { to: Route::Login {}, class: "ik-btn primary block", "Sign in" }
        }
    }
}

/// A single cover card in the Discover/Search grid.
#[component]
pub fn CoverCard(series: SeriesSummary) -> Element {
    rsx! {
        Link { to: Route::Series { id: series.id.clone() }, class: "ik-card",
            Cover { url: series.cover_url.clone(), title: series.title.clone() }
            div { class: "ik-card-body",
                div { class: "ik-card-title", "{series.title}" }
                div { class: "ik-card-meta",
                    span { "{series.content_type.label()}" }
                    span { "·" }
                    span { "{series.status.label()}" }
                    span { class: "ik-rail-spacer" }
                    span { class: "ik-mono", "{series.source_count} src" }
                }
            }
        }
    }
}

/// A cover image with a graceful typographic fallback when no `cover_url` is stored.
#[component]
pub fn Cover(url: Option<String>, title: String) -> Element {
    let initial = title
        .chars()
        .next()
        .unwrap_or('?')
        .to_uppercase()
        .to_string();
    match url {
        Some(u) if !u.is_empty() => rsx! {
            img { class: "ik-cover", src: "{u}", alt: "{title}", loading: "lazy" }
        },
        _ => rsx! {
            div { class: "ik-cover-fallback", "{initial}" }
        },
    }
}

/// Skeleton placeholder grid shown while covers load (never a spinner, per §17.3).
#[component]
pub fn SkeletonGrid(count: usize) -> Element {
    rsx! {
        div { class: "ik-grid",
            for _ in 0..count {
                div { class: "ik-card",
                    div { class: "ik-skeleton ik-skel-cover" }
                    div { class: "ik-card-body",
                        div { class: "ik-skeleton", style: "height:14px;width:80%;margin-bottom:6px;" }
                        div { class: "ik-skeleton", style: "height:12px;width:50%;" }
                    }
                }
            }
        }
    }
}

/// A named error state with a retry affordance (§17.3: "name what failed and how to retry").
#[component]
pub fn ErrorBox(message: String, on_retry: EventHandler<()>) -> Element {
    rsx! {
        div { class: "ik-error",
            p { "Something went wrong: {message}" }
            button { class: "ik-btn", onclick: move |_| on_retry.call(()), "Try again" }
        }
    }
}

/// An inviting empty state (§17.2.1).
#[component]
pub fn EmptyBox(message: String) -> Element {
    rsx! {
        div { class: "ik-empty", "{message}" }
    }
}

/// A thin brush-stroke section divider (the one signature device, §17.1).
#[component]
pub fn Brush() -> Element {
    rsx! {
        div { class: "ik-brush" }
    }
}

/// Format an RFC-3339 timestamp as a coarse "time ago" string, using the browser's own
/// date parser so no date crate is pulled into the wasm bundle. `None`/empty → `—`; an
/// unparseable value falls back to the raw string.
pub fn rel_time(ts: Option<&str>) -> String {
    let Some(s) = ts.filter(|s| !s.is_empty()) else {
        return "—".to_owned();
    };
    let parsed = js_sys::Date::parse(s);
    if parsed.is_nan() {
        return s.to_owned();
    }
    humanize_ms(js_sys::Date::now() - parsed)
}

/// Humanise a millisecond age into a compact relative label.
fn humanize_ms(diff_ms: f64) -> String {
    if diff_ms < 45_000.0 {
        return "just now".to_owned();
    }
    let secs = (diff_ms / 1000.0) as i64;
    let mins = secs / 60;
    if mins < 60 {
        return format!("{mins}m ago");
    }
    let hours = mins / 60;
    if hours < 24 {
        return format!("{hours}h ago");
    }
    let days = hours / 24;
    if days < 30 {
        return format!("{days}d ago");
    }
    let months = days / 30;
    if months < 12 {
        return format!("{months}mo ago");
    }
    format!("{}y ago", days / 365)
}

/// Global unread-notifications badge count, provided at the app root and updated by the
/// Notifications view. A newtype so it is distinct in the context map.
#[derive(Clone, Copy)]
pub struct UnreadBadge(pub Signal<i64>);

/// A "please sign in" gate rendered by protected views when there is no session.
#[component]
pub fn SignInGate() -> Element {
    rsx! {
        div { class: "ik-empty",
            p { "Sign in to see this." }
            Link { to: Route::Login {}, class: "ik-btn primary", "Sign in" }
        }
    }
}
