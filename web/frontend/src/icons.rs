//! Inline-SVG icon module (`IMPLEMENTATION_PLAN` §4). A single Rust enum + `Ic` component,
//! no web font: works offline, tree-shaken, crisp at any size. Glyphs are 24×24 Lucide
//! (MIT) paths, drawn with `currentColor` so a text-color utility tints them.

use dioxus::prelude::*;

/// Every glyph the `TankoVault` design uses (`DESIGN_SPEC` §6–7). The full inventory is
/// vendored up front; not every glyph is referenced yet (later screens use the rest), so
/// the unused variants are allowed until F2–F5 land.
#[allow(dead_code)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Icon {
    // nav
    Home,
    Explore,
    Search,
    Watchlist,
    Notifications,
    Console,
    Account,
    Settings,
    MenuBook,
    // actions / status
    PlayCircle,
    Bolt,
    AutoAwesome,
    Layers,
    Star,
    Tune,
    Bookmark,
    Notify,
    CloudDone,
    CloudOff,
    CloudSync,
    ArrowForward,
    ChevronRight,
    ChevronDown,
    Close,
    Check,
    OpenInNew,
    // console
    Radar,
    Merge,
    Group,
    History,
    ShieldLock,
    Code,
    Dashboard,
    // watchlist columns
    Fire,
    Schedule,
    TaskAlt,
    PauseCircle,
    Cancel,
    // discover / misc
    Add,
    Remove,
    Refresh,
    Public,
    Block,
    Person,
    Palette,
    Mail,
    Devices,
    Back,
    Download,
    Delete,
    Flag,
    // fallback
    Circle,
}

/// Render a glyph. `size` in px (default 20); `class` applies a text-color utility.
#[component]
pub(crate) fn Ic(
    icon: Icon,
    #[props(default = 20)] size: u32,
    #[props(default)] class: String,
) -> Element {
    let d = path_for(icon);
    rsx! {
        svg {
            class: "{class}",
            width: "{size}",
            height: "{size}",
            view_box: "0 0 24 24",
            fill: "none",
            stroke: "currentColor",
            stroke_width: "2",
            stroke_linecap: "round",
            stroke_linejoin: "round",
            "aria-hidden": "true",
            dangerous_inner_html: "{d}",
        }
    }
}

/// The inner `<path>`/shape markup for each glyph.
fn path_for(icon: Icon) -> &'static str {
    match icon {
        Icon::Home => {
            r#"<path d="M3 9.5 12 3l9 6.5V20a1 1 0 0 1-1 1h-5v-6H9v6H4a1 1 0 0 1-1-1z"/>"#
        }
        Icon::Explore => r#"<circle cx="12" cy="12" r="9"/><path d="m15.5 8.5-2 5-5 2 2-5z"/>"#,
        Icon::Search => r#"<circle cx="11" cy="11" r="7"/><path d="m21 21-4.3-4.3"/>"#,
        Icon::Watchlist => {
            r#"<path d="M4 4h13a2 2 0 0 1 2 2v15l-6-4-6 4V6a2 2 0 0 1 2-2z"/><path d="M8 4v13"/>"#
        }
        // The bell is one glyph; the two names are the rail entry and the per-title toggle.
        Icon::Notifications | Icon::Notify => {
            r#"<path d="M6 8a6 6 0 0 1 12 0c0 7 3 9 3 9H3s3-2 3-9"/><path d="M10.3 21a2 2 0 0 0 3.4 0"/>"#
        }
        // The console and its dashboard tab share one glyph; one arm keeps the path data
        // single-sourced so a retouch cannot drift between them.
        Icon::Console | Icon::Dashboard => {
            r#"<rect x="3" y="3" width="7" height="9" rx="1"/><rect x="14" y="3" width="7" height="5" rx="1"/><rect x="14" y="12" width="7" height="9" rx="1"/><rect x="3" y="16" width="7" height="5" rx="1"/>"#
        }
        // One avatar glyph, named for the rail destination and for a user row.
        Icon::Account | Icon::Person => {
            r#"<circle cx="12" cy="8" r="4"/><path d="M4 21a8 8 0 0 1 16 0"/>"#
        }
        Icon::Settings => {
            r#"<circle cx="12" cy="12" r="3"/><path d="M19.4 15a1.6 1.6 0 0 0 .3 1.8l.1.1a2 2 0 1 1-2.8 2.8l-.1-.1a1.6 1.6 0 0 0-2.7 1.1V21a2 2 0 1 1-4 0v-.1a1.6 1.6 0 0 0-2.7-1.1l-.1.1a2 2 0 1 1-2.8-2.8l.1-.1A1.6 1.6 0 0 0 4.6 15H4.5a2 2 0 1 1 0-4h.1a1.6 1.6 0 0 0 1.1-2.7l-.1-.1a2 2 0 1 1 2.8-2.8l.1.1A1.6 1.6 0 0 0 11 4.6V4.5a2 2 0 1 1 4 0v.1a1.6 1.6 0 0 0 2.7 1.1l.1-.1a2 2 0 1 1 2.8 2.8l-.1.1a1.6 1.6 0 0 0 1.1 2.7h.1a2 2 0 1 1 0 4h-.1a1.6 1.6 0 0 0-1.2.9z"/>"#
        }
        Icon::MenuBook => {
            r#"<path d="M12 6C10.5 5 8 4.5 4 4.5V19c4 0 6.5.5 8 1.5 1.5-1 4-1.5 8-1.5V4.5c-4 0-6.5.5-8 1.5z"/><path d="M12 6v14.5"/>"#
        }
        Icon::PlayCircle => r#"<circle cx="12" cy="12" r="9"/><path d="m10 9 5 3-5 3z"/>"#,
        Icon::Bolt => r#"<path d="M13 2 4 14h7l-1 8 9-12h-7z"/>"#,
        Icon::AutoAwesome => {
            r#"<path d="M12 3l2 5 5 2-5 2-2 5-2-5-5-2 5-2z"/><path d="M19 15l.8 2 2 .8-2 .8-.8 2-.8-2-2-.8 2-.8z"/>"#
        }
        Icon::Layers => {
            r#"<path d="m12 2 9 5-9 5-9-5z"/><path d="m3 12 9 5 9-5"/><path d="m3 17 9 5 9-5"/>"#
        }
        Icon::Star => {
            r#"<path d="m12 3 2.9 5.9 6.5.9-4.7 4.6 1.1 6.5L12 18l-5.8 3 1.1-6.5L2.6 9.8l6.5-.9z"/>"#
        }
        Icon::Tune => {
            r#"<path d="M4 6h10M18 6h2M4 12h2M10 12h10M4 18h8M16 18h4"/><circle cx="16" cy="6" r="2"/><circle cx="8" cy="12" r="2"/><circle cx="14" cy="18" r="2"/>"#
        }
        Icon::Bookmark => r#"<path d="M6 3h12a1 1 0 0 1 1 1v17l-7-4-7 4V4a1 1 0 0 1 1-1z"/>"#,

        Icon::CloudDone => {
            r#"<path d="M7 18a4 4 0 0 1 0-8 5 5 0 0 1 9.6-1.5A3.5 3.5 0 0 1 18 18z"/><path d="m9 14 2 2 4-4"/>"#
        }
        Icon::CloudOff => {
            r#"<path d="M16 16.9a4 4 0 0 0-1.4-7.9 5 5 0 0 0-9.3-1.7"/><path d="M4.4 5 3 6.4 6.8 10.2A4 4 0 0 0 7 18h9a3.5 3.5 0 0 0 1.3-.3L19.6 20 21 18.6z"/>"#
        }
        Icon::CloudSync => {
            r#"<path d="M7 17a4 4 0 0 1 0-8 5 5 0 0 1 9.6-1.5A3.5 3.5 0 0 1 18 17"/><path d="M9 14.5 12 12l3 2.5M12 12v6"/>"#
        }
        Icon::ArrowForward => r#"<path d="M5 12h14M13 6l6 6-6 6"/>"#,
        Icon::ChevronRight => r#"<path d="m9 6 6 6-6 6"/>"#,
        Icon::ChevronDown => r#"<path d="m6 9 6 6 6-6"/>"#,
        Icon::Close => r#"<path d="M6 6l12 12M18 6 6 18"/>"#,
        Icon::Check => r#"<path d="m5 12 5 5 9-11"/>"#,
        Icon::OpenInNew => {
            r#"<path d="M14 5h5v5"/><path d="M19 5 10 14"/><path d="M19 14v4a1 1 0 0 1-1 1H6a1 1 0 0 1-1-1V6a1 1 0 0 1 1-1h4"/>"#
        }
        Icon::Radar => {
            r#"<circle cx="12" cy="12" r="9"/><path d="M12 12 20 8"/><path d="M12 4a8 8 0 1 0 8 8"/>"#
        }
        Icon::Merge => {
            r#"<path d="M6 3v6a6 6 0 0 0 6 6h6"/><path d="m15 12 3 3-3 3"/><path d="M18 3v3"/>"#
        }
        Icon::Group => {
            r#"<circle cx="9" cy="8" r="3"/><path d="M3 20a6 6 0 0 1 12 0"/><path d="M16 6a3 3 0 0 1 0 6"/><path d="M18 20a6 6 0 0 0-3-5"/>"#
        }
        Icon::History => {
            r#"<path d="M3 12a9 9 0 1 0 3-6.7L3 8"/><path d="M3 4v4h4"/><path d="M12 8v4l3 2"/>"#
        }
        Icon::ShieldLock => {
            r#"<path d="M12 3l8 3v5c0 5-3.5 8.5-8 10-4.5-1.5-8-5-8-10V6z"/><rect x="9.5" y="11" width="5" height="4" rx="1"/><path d="M10.5 11V9.5a1.5 1.5 0 0 1 3 0V11"/>"#
        }
        Icon::Code => r#"<path d="m8 8-4 4 4 4"/><path d="m16 8 4 4-4 4"/><path d="m13 5-2 14"/>"#,
        Icon::Fire => {
            r#"<path d="M12 3c1 3-1 4-1 6a3 3 0 0 0 6 0c0-1 0-2-.5-3 2 1.5 3.5 4 3.5 7a7 7 0 1 1-14 0c0-3.5 2.5-6 6-10z"/>"#
        }
        Icon::Schedule => r#"<circle cx="12" cy="12" r="9"/><path d="M12 7v5l3 2"/>"#,
        Icon::TaskAlt => r#"<path d="M22 11.1V12a10 10 0 1 1-5.9-9.1"/><path d="m9 11 3 3L22 4"/>"#,
        Icon::PauseCircle => r#"<circle cx="12" cy="12" r="9"/><path d="M10 9v6M14 9v6"/>"#,
        Icon::Cancel => r#"<circle cx="12" cy="12" r="9"/><path d="m15 9-6 6M9 9l6 6"/>"#,
        Icon::Add => r#"<path d="M12 5v14M5 12h14"/>"#,
        Icon::Remove => r#"<path d="M5 12h14"/>"#,
        Icon::Refresh => r#"<path d="M21 12a9 9 0 1 1-2.6-6.4"/><path d="M21 3v5h-5"/>"#,
        Icon::Public => {
            r#"<circle cx="12" cy="12" r="9"/><path d="M3 12h18M12 3a15 15 0 0 1 0 18M12 3a15 15 0 0 0 0 18"/>"#
        }
        Icon::Block => r#"<circle cx="12" cy="12" r="9"/><path d="m5.6 5.6 12.8 12.8"/>"#,

        Icon::Palette => {
            r#"<path d="M12 3a9 9 0 1 0 0 18c1.5 0 2-1 2-2 0-1.5 1-2 2-2h1a3 3 0 0 0 3-3 8 8 0 0 0-8-9z"/><circle cx="7.5" cy="10.5" r="1"/><circle cx="12" cy="7.5" r="1"/><circle cx="16.5" cy="10.5" r="1"/>"#
        }
        Icon::Mail => {
            r#"<rect x="3" y="5" width="18" height="14" rx="2"/><path d="m3 7 9 6 9-6"/>"#
        }
        Icon::Devices => {
            r#"<rect x="3" y="5" width="13" height="10" rx="1"/><path d="M2 19h13"/><rect x="17" y="9" width="5" height="10" rx="1"/>"#
        }
        Icon::Back => r#"<path d="M19 12H5M11 6l-6 6 6 6"/>"#,
        Icon::Download => r#"<path d="M12 4v11"/><path d="m7 11 5 5 5-5"/><path d="M4 20h16"/>"#,
        Icon::Delete => {
            r#"<path d="M4 7h16"/><path d="M9 7V5a1 1 0 0 1 1-1h4a1 1 0 0 1 1 1v2"/><path d="M6 7l1 12a1 1 0 0 0 1 1h8a1 1 0 0 0 1-1l1-12"/><path d="M10 11v6M14 11v6"/>"#
        }
        Icon::Flag => r#"<path d="M5 21V4"/><path d="M5 5h11l-2 3 2 3H5z"/>"#,
        Icon::Circle => r#"<circle cx="12" cy="12" r="4"/>"#,
    }
}
