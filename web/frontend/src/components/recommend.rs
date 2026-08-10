//! The recommendation card: a cover card that says *why* a series is on the shelf, and lets the
//! reader refuse it.
//!
//! Split from [`CoverCard`](super::CoverCard) rather than growing it: this card carries an
//! interactive control, so its root cannot be the `Link` a plain cover card is — a `<button>`
//! inside an `<a>` is not focusable in the order it appears, and clicking it navigates.

use crate::api;
use crate::components::{CardMeta, Cover};
use crate::hooks::{use_busy, Reload};
use crate::i18n::use_i18n;
use crate::icons::{Ic, Icon};
use crate::models::{because_series, FeedbackBody, Recommendation, SeriesId};
use crate::Route;
use dioxus::prelude::*;
use inkstone_ui::Button;
/// The verdicts `POST /v1/me/recommendations/{id}/feedback` accepts.
///
/// Wire tokens rather than an enum, because the endpoint publishes a free-form string and
/// validates it itself; the two are named here so a typo is one grep, not a silent 400.
const NOT_INTERESTED: &str = "not_interested";
const HIDE_FOREVER: &str = "hide_forever";

/// One recommendation: the cover card, the reason it is here, and the refusal.
///
/// `detailed` is the difference between a card in a rail and a card on the screen that exists to
/// explain itself: the same reason, given room and a label instead of a grey line under the meta.
#[component]
pub(crate) fn RecCard(item: Recommendation, reload: Reload, detailed: bool) -> Element {
    let i18n = use_i18n();
    let mut asking = use_signal(|| false);

    let seed = because_series(&item);
    // The score is deliberately not shown. It is a blended, rank-normalised number whose scale
    // is only meaningful against the other candidates in the same request — a reader comparing
    // two shelves would be comparing nothing.
    rsx! {
        div { class: "ik-card", style: "position:relative;",
            Link {
                to: Route::Series { id: item.id.to_string() },
                style: "display:block;color:inherit;",
                Cover { url: item.cover_url.clone(), title: item.title.clone() }
                div { class: "ik-card-body",
                    div { class: "ik-card-title", "{item.title}" }
                    CardMeta {
                        content_type: item.content_type,
                        status: item.status,
                        chapter_count: item.chapter_count,
                        release_year: item.release_year,
                        blurb: item.blurb.clone(),
                        tags: item.tags.clone(),
                    }
                }
            }
            if *asking.read() {
                Verdicts { series_id: item.id, reload, on_close: move |()| asking.set(false) }
            } else {
                Because {
                    title: item.because_title.clone(),
                    seed,
                    shared: item.shared.clone(),
                    detailed,
                }
                Button {
                    style: "position:absolute;top:8px;right:8px;width:30px;height:30px;padding:0;justify-content:center;background:color-mix(in srgb,var(--bg) 72%,transparent);backdrop-filter:blur(3px);",
                    title: i18n.args("home.recommendations.dismiss", &[("title", &item.title)]),
                    aria_label: i18n.args("home.recommendations.dismiss", &[("title", &item.title)]),
                    on_click: move |_| asking.set(true),
                    Ic { icon: Icon::Close, size: 14 }
                }
            }
        }
    }
}

/// Why this series is on the shelf: the seed that produced it, and what the two have in common.
///
/// Renders nothing at all when the server sent no seed — the profile, exact-feature and
/// popularity paths genuinely have no single title to name, and inventing one ("picked for you")
/// would dress a different kind of answer up as this one.
///
/// `detailed` gives the same two facts a heading and full-size type. On a rail the reason is a
/// footnote to the cover; on the screen the reader opened *to see the reasons*, a 11.5px grey
/// line is the wrong emphasis.
#[component]
fn Because(
    title: Option<String>,
    seed: Option<SeriesId>,
    shared: Vec<String>,
    detailed: bool,
) -> Element {
    let i18n = use_i18n();
    let Some(title) = title else {
        return rsx! {};
    };
    // One interpolated sentence rather than a prefix concatenated with a linked title: German
    // puts the title in the middle of the clause, and a prefix + `{title}` layout would build
    // that sentence in English word order whatever the catalogue says.
    let line = i18n.args("home.recommendations.because", &[("title", &title)]);
    let line_style = if detailed {
        "font-size:13px;color:var(--text-2);line-height:1.45;"
    } else {
        "font-size:11.5px;color:var(--muted);line-height:1.4;"
    };
    rsx! {
        div { class: "ik-rec-because", style: if detailed { "padding:0 12px 12px;border-top:1px solid var(--border-soft);margin-top:2px;padding-top:10px;" } else { "padding:0 12px 12px;" },
            if detailed {
                div { class: "ik-sec-lbl", style: "margin-bottom:5px;",
                    {i18n.t("recommendations.whyLabel")}
                }
            }
            div { style: "{line_style}",
                if let Some(seed) = seed {
                    Link {
                        to: Route::Series { id: seed.to_string() },
                        style: "color:inherit;text-decoration:underline;text-underline-offset:2px;",
                        "{line}"
                    }
                } else {
                    span { "{line}" }
                }
            }
            if !shared.is_empty() {
                if detailed {
                    div { class: "ik-muted", style: "font-size:11.5px;margin-top:8px;",
                        {i18n.t("recommendations.sharesLabel")}
                    }
                }
                div { class: "ik-flex", style: "flex-wrap:wrap;gap:5px;margin-top:5px;",
                    for feature in shared {
                        span {
                            key: "{feature}",
                            class: "ik-tagchip",
                            style: "cursor:default;font-size:11px;padding:2px 8px;",
                            "{feature}"
                        }
                    }
                }
            }
        }
    }
}

/// The two refusals, offered together because they are not the same promise: one expires and
/// one does not, and a single "hide" control would have to pick for the reader.
#[component]
fn Verdicts(series_id: SeriesId, reload: Reload, on_close: EventHandler<()>) -> Element {
    let i18n = use_i18n();
    let api = api::use_api();
    let busy = use_busy();
    let mut failed = use_signal(|| Option::<String>::None);

    let mut send = move |verdict: &'static str| {
        if !busy.claim() {
            return;
        }
        let client = api.client();
        failed.set(None);
        spawn(async move {
            let result = client
                .feedback()
                .series_id(series_id)
                .body(FeedbackBody {
                    verdict: verdict.to_owned(),
                })
                .send()
                .await;
            busy.release();
            match result {
                // The server marks the profile stale, so the refetch returns a shelf this
                // series is genuinely absent from rather than one that merely hides it here.
                Ok(_) => reload.bump(),
                // Said, not swallowed: closing the prompt on a failed write would leave the
                // reader believing they had refused a series that is still being suggested.
                Err(e) => failed.set(Some(api::friendly_error(i18n, e))),
            }
        });
    };

    rsx! {
        div { style: "padding:10px 12px 12px;",
            div { style: "font-size:11.5px;color:var(--muted);margin-bottom:8px;",
                {i18n.t("home.recommendations.askTitle")}
            }
            if let Some(message) = failed.read().clone() {
                p { style: "font-size:11.5px;color:var(--acc);margin:0 0 8px;", "{message}" }
            }
            div { class: "ik-flex", style: "flex-wrap:wrap;gap:6px;",
                Button {
                    style: "font-size:12px;padding:6px 10px;",
                    disabled: busy.is_busy(),
                    title: i18n.t("home.recommendations.notInterestedHint"),
                    on_click: move |_| send(NOT_INTERESTED),
                    {i18n.t("home.recommendations.notInterested")}
                }
                Button {
                    style: "font-size:12px;padding:6px 10px;",
                    disabled: busy.is_busy(),
                    title: i18n.t("home.recommendations.hideForeverHint"),
                    on_click: move |_| send(HIDE_FOREVER),
                    {i18n.t("home.recommendations.hideForever")}
                }
                Button {
                    style: "font-size:12px;padding:6px 10px;margin-left:auto;",
                    disabled: busy.is_busy(),
                    on_click: move |_| on_close.call(()),
                    {i18n.t("common.cancel")}
                }
            }
        }
    }
}
