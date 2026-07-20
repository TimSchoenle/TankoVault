//! Reusable Inkstone UI components (design §17.3/§17.4): the app shell (left rail + top
//! command bar), cover cards, loading skeletons, and named empty/error states.

use crate::api;
use crate::models::SeriesSummary;
use crate::state::use_session;
use crate::Route;
use dioxus::prelude::*;

/// The persistent app shell: left rail nav + top command bar, with the routed view in the
/// content area (via `Outlet`). Also performs the boot-time silent refresh.
#[component]
pub fn Shell() -> Element {
    let session = use_session();
    let route: Route = use_route();

    // Silent refresh once on boot: adopt an access token from the httpOnly cookie if a
    // session already exists, so a page reload keeps the user signed in (design §17.4).
    use_effect(move || {
        if !*session.ready.read() {
            spawn(async move {
                if let Ok(tok) = api::refresh().await {
                    session.set_token(tok.access_token);
                }
                session.mark_ready();
            });
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

    let unread_count = *unread.0.read();

    rsx! {
        div { class: "ik-app",
            nav { class: "ik-rail",
                div { class: "ik-wordmark",
                    span { "TankoVault" }
                    span { class: "dot", "•" }
                }
                NavLink { to: Route::Discover {}, label: "Discover", current: route.clone(), badge: 0 }
                NavLink { to: Route::Reading {}, label: "Reading", current: route.clone(), badge: 0 }
                NavLink { to: Route::Watchlist {}, label: "Watchlist", current: route.clone(), badge: 0 }
                NavLink {
                    to: Route::Notifications {},
                    label: "Notifications",
                    current: route.clone(),
                    badge: unread_count,
                }
                if session.role.read().is_operator() {
                    NavLink { to: Route::Console {}, label: "Console", current: route.clone(), badge: 0 }
                }
                div { class: "ik-rail-spacer" }
                SessionButton {}
            }
            main { class: "ik-main",
                TopBar {}
                section { class: "ik-content", Outlet::<Route> {} }
            }
        }
    }
}

/// A left-rail navigation entry; renders the active brush stroke when it matches the
/// current route's top-level screen.
#[component]
fn NavLink(to: Route, label: String, current: Route, badge: i64) -> Element {
    let is_active = same_screen(&to, &current);
    let class = if is_active {
        "ik-nav-link active"
    } else {
        "ik-nav-link"
    };
    rsx! {
        Link { to: to.clone(), class: "{class}",
            span { "{label}" }
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

/// Top command bar with instant-search that routes to the Search screen on submit.
#[component]
fn TopBar() -> Element {
    let nav = use_navigator();
    let mut query = use_signal(String::new);
    rsx! {
        header { class: "ik-topbar",
            div { class: "ik-search",
                input {
                    class: "ik-input",
                    r#type: "search",
                    placeholder: "Search series and tags…",
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
            }
        }
    }
}

/// Sign-in / sign-out affordance in the rail footer.
#[component]
fn SessionButton() -> Element {
    let session = use_session();
    if session.is_authenticated() {
        rsx! {
            button {
                class: "ik-btn",
                onclick: move |_| {
                    spawn(async move {
                        let _ = api::logout().await;
                        session.clear();
                    });
                },
                "Sign out"
            }
        }
    } else {
        rsx! {
            Link { to: Route::Login {}, class: "ik-btn primary", "Sign in" }
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
