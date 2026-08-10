//! Content preferences (§9.4) — the reader's half of the adult-content gate.
//!
//! Two states, not one switch. Turning the preference on for the first time is a declaration of
//! age and asks for confirmation; every change after that is an ordinary setting, because the
//! attestation is kept server-side and re-asking would teach readers to click through the one
//! dialog that is meant to carry weight.
//!
//! Nothing here is optimistic. Every other panel flips its control locally and reconciles, which
//! is right for a display setting and wrong for this one: a control that shows "on" before the
//! server agreed would report an entitlement the reader may not have.

use crate::api;
use crate::components::{OutcomeLine, PanelCard, Section, SkeletonBlock};
use crate::hooks::use_outcome;
use crate::i18n::use_i18n;
use crate::icons::Icon;
use crate::wire::types::{ContentPrefsDto, ContentPrefsUpdate};
use dioxus::prelude::*;
use inkstone_ui::{Button, ToggleButton, Tone};
#[component]
pub(crate) fn ContentPanel() -> Element {
    let i18n = use_i18n();
    let api = api::use_api();
    let mut outcome = use_outcome();
    let mut prefs = use_signal(|| Option::<ContentPrefsDto>::None);
    // Whether the confirmation step is showing. Local, and never persisted: a reader who
    // reloads mid-decision has not decided.
    let mut confirming = use_signal(|| false);
    let mut saving = use_signal(|| false);

    use_effect(move || {
        let client = api.client();
        spawn(async move {
            match client.content_prefs().send().await {
                Ok(response) => prefs.set(Some(response.into_inner())),
                // Not defaulted on failure: presenting "off" as saved state would let a reader
                // believe they had opted out when nothing was read at all.
                Err(e) => outcome.set(Some(Err(api::friendly_error(i18n, e)))),
            }
        });
    });

    let Some(current) = prefs.read().clone() else {
        return rsx! {
            PanelCard { icon: Icon::ShieldLock, title: i18n.t("account.content.title"),
                SkeletonBlock { height: 140 }
            }
        };
    };

    let mut save = move |adult_opt_in: bool, confirm_age: bool| {
        outcome.set(None);
        saving.set(true);
        confirming.set(false);
        let client = api.client();
        spawn(async move {
            let body = ContentPrefsUpdate {
                adult_opt_in,
                confirm_age: Some(confirm_age),
            };
            match client.put_content_prefs().body(body).send().await {
                Ok(response) => {
                    prefs.set(Some(response.into_inner()));
                    outcome.set(Some(Ok(i18n.t("account.content.saved"))));
                }
                Err(e) => outcome.set(Some(Err(api::friendly_error(i18n, e)))),
            }
            saving.set(false);
        });
    };

    let busy = *saving.read();
    let opted_in = current.adult_opt_in;
    let attested = current.age_attested;
    let allowed = current.allowed_by_deployment;

    rsx! {
        PanelCard { icon: Icon::ShieldLock, title: i18n.t("account.content.title"),
            Section { label: i18n.t("account.content.section.adult"),
                p { class: "ik-muted", style: "font-size:13px;", {i18n.t("account.content.intro")} }

                // Shown whether or not the deployment allows it, and *before* the control, so a
                // reader who cannot understand why their shelf is unchanged has the reason in
                // front of them rather than concluding the setting failed to save.
                if !allowed {
                    div { class: "ik-note", {i18n.t("account.content.unavailable")} }
                }

                if *confirming.read() {
                    div { class: "ik-note",
                        div { {i18n.t("account.content.confirm.body")} }
                        div { class: "ik-row", style: "margin-top:8px; gap:8px;",
                            Button {
                                tone: Tone::Primary,
                                disabled: busy,
                                on_click: move |_| save(true, true),
                                {i18n.t("account.content.confirm.cta")}
                            }
                            Button {
                                disabled: busy,
                                on_click: move |_| confirming.set(false),
                                {i18n.t("common.cancel")}
                            }
                        }
                    }
                } else {
                    div { class: "ik-row",
                        div { class: "grow",
                            div { {i18n.t("account.content.adult.label")} }
                            div { class: "ik-muted", style: "font-size:12px;",
                                {i18n.t("account.content.adult.hint")}
                            }
                        }
                        ToggleButton {
                            on: opted_in,
                            disabled: busy || !allowed,
                            on_toggle: move |_| {
                                if opted_in {
                                    // Opting out never asks anything. Making the reader confirm
                                    // their way *out* of adult content would be a dark pattern.
                                    save(false, false);
                                } else if attested {
                                    save(true, false);
                                } else {
                                    confirming.set(true);
                                }
                            },
                            if opted_in {
                                {i18n.t("common.on")}
                            } else {
                                {i18n.t("common.off")}
                            }
                        }
                    }
                }
            }
            OutcomeLine { outcome: outcome.read().clone() }
        }
    }
}
