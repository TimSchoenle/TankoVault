//! The native client, advertised from the browser it would replace.
//!
//! **Web only** — `AccountPanel::is_visible` gates it — because the one reader who does not
//! need this panel is the one already reading it in the desktop app.
//!
//! Every link here goes to the releases page rather than to a versioned asset, and that is a
//! constraint rather than a shortcut: nothing in the browser build knows which release is
//! current, and the only way to find out would be a request to github.com. The served
//! Content-Security-Policy does not reach it and **must not be widened to**, since that policy is
//! the ceiling on where an injected script could send the access token this page holds in memory.
//! A download link is not worth paying for with that.
//!
//! The server address is shown alongside, because it is the one thing the installed client asks
//! for on first run and the one thing a reader who has only ever used a bookmark does not know.

use crate::components::PanelCard;
use crate::i18n::use_i18n;
use crate::icons::{Ic, Icon};
use dioxus::prelude::*;
use inkstone_ui::{button_class, Size, Tone};
/// What the native client offers that a browser tab cannot. Catalogue keys, in the order they
/// are shown.
const ADVANTAGES: [&str; 4] = [
    "account.desktop.advantage.notifications",
    "account.desktop.advantage.tray",
    "account.desktop.advantage.startup",
    "account.desktop.advantage.updates",
];

#[component]
pub(crate) fn DesktopAppPanel() -> Element {
    let i18n = use_i18n();
    let origin = crate::platform::origin();

    rsx! {
        PanelCard { icon: Icon::Download, title: i18n.t("account.desktop.title"),
            p { class: "ik-muted", style: "font-size:13px;margin-top:0;",
                {i18n.t("account.desktop.intro")}
            }

            ul { class: "ik-bullets",
                for key in ADVANTAGES {
                    li { key: "{key}",
                        Ic { icon: Icon::Check, size: 14 }
                        span { {i18n.t(key)} }
                    }
                }
            }

            div { class: "ik-prefs-actions", style: "margin-top:16px;",
                a {
                    class: button_class(Tone::Primary, Size::Md, false),
                    href: crate::build_info::RELEASES_URL,
                    target: "_blank",
                    rel: "noopener noreferrer",
                    {i18n.t("account.desktop.download")}
                    Ic { icon: Icon::OpenInNew, size: 13 }
                }
            }
            p { class: "ik-muted", style: "font-size:12.5px;margin:10px 0 0;",
                {i18n.t("account.desktop.platforms")}
            }

            if !origin.is_empty() {
                div { class: "ik-note", style: "padding:12px;margin-top:16px;",
                    div { class: "ik-subhead", {i18n.t("account.desktop.serverAddress")} }
                    code { style: "font-size:12.5px;word-break:break-all;", "{origin}" }
                    p { class: "ik-muted", style: "font-size:12.5px;margin:6px 0 0;",
                        {i18n.t("account.desktop.serverAddressHint")}
                    }
                }
            }
        }
    }
}
