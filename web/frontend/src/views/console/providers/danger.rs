//! The Danger tab: irreversible and near-irreversible operations, each stating its blast
//! radius with real counts rather than asking "are you sure".

use crate::api;
use crate::components::{
    use_step_up_gate, InlineConfirm, OutcomeLine, Section, StepUpGuard, TypeToConfirm,
};
use crate::hooks::{use_busy, use_outcome, Reload};
use crate::i18n::use_i18n;
use crate::models::{Provider, ProviderStat, ProviderState, SetProviderStateBody};
use crate::util::thousands;
use dioxus::prelude::*;

/// Two tiers, as designed: blocklisting is reversible and acts inline; deleting is not and is
/// gated on typing the slug.
#[component]
pub(super) fn DangerTab(
    provider: Provider,
    stat: Option<ProviderStat>,
    can_change_state: bool,
    can_delete: bool,
    reload: Reload,
    on_deleted: EventHandler<()>,
) -> Element {
    let api = api::use_api();
    let i18n = use_i18n();
    let busy = use_busy();
    let mut outcome = use_outcome();
    // Elevated: blocklisting and deleting a provider are mutating operator capabilities, and
    // the API answers `403 step_up_required` until a second factor has been presented.
    let gate = use_step_up_gate();
    let mut confirming_block = use_signal(|| false);
    let id = provider.id;
    let blocked = provider.state == ProviderState::Blocked;

    let set_blocked = use_callback(move |target: ProviderState| {
        if !busy.claim() {
            return;
        }
        outcome.set(None);
        let client = gate.client(api);
        spawn(async move {
            match client
                .set_provider_state()
                .id(id)
                .body(SetProviderStateBody { state: target })
                .send()
                .await
            {
                Ok(_) => {
                    confirming_block.set(false);
                    reload.bump();
                }
                Err(e) => {
                    if !gate.refused(api::Refusal::of(&e)) {
                        outcome.set(Some(Err(api::guarded_error(i18n, e))));
                    }
                }
            }
            busy.release();
        });
    });

    let delete = move |()| {
        gate.attempt(move || {
            if !busy.claim() {
                return;
            }
            outcome.set(None);
            let client = gate.client(api);
            spawn(async move {
                match client.delete_provider().id(id).send().await {
                    Ok(_) => {
                        on_deleted.call(());
                        reload.bump();
                    }
                    Err(e) => {
                        if !gate.refused(api::Refusal::of(&e)) {
                            outcome.set(Some(Err(api::guarded_error(i18n, e))));
                        }
                    }
                }
                busy.release();
            });
        });
    };

    // The blast radius, in the numbers the operator can check against the Coverage tab.
    let radius = i18n.args(
        "console.providers.deleteRadius",
        &[
            (
                "sources",
                &stat
                    .as_ref()
                    .map_or_else(|| "—".to_owned(), |s| thousands(s.source_count)),
            ),
            (
                "chapters",
                &stat
                    .as_ref()
                    .map_or_else(|| "—".to_owned(), |s| thousands(s.chapter_count)),
            ),
        ],
    );

    rsx! {
        Section { label: i18n.t("console.providers.tab.danger"),
            div { class: "ik-danger",
                if can_change_state {
                    if *confirming_block.read() {
                        InlineConfirm {
                            title: i18n.t("console.providers.blocklist"),
                            body: i18n.t("console.providers.blocklistWhy"),
                            cta: i18n.t("console.providers.blocklistCta"),
                            busy: busy.is_busy(),
                            on_cancel: move |()| confirming_block.set(false),
                            on_confirm: move |()| gate.attempt(move || set_blocked.call(ProviderState::Blocked)),
                        }
                    } else {
                        div { class: "ik-flex", style: "padding:10px 12px;gap:10px;",
                            div { style: "min-width:0;",
                                div { class: "ttl", {i18n.t("console.providers.blocklist")} }
                                div { class: "why", {i18n.t("console.providers.blocklistWhy")} }
                            }
                            button {
                                class: "ik-btn xs",
                                style: "margin-left:auto;flex:none;",
                                disabled: busy.is_busy(),
                                onclick: move |_| {
                                    if blocked {
                                        gate.attempt(move || set_blocked.call(ProviderState::Active));
                                    } else {
                                        confirming_block.set(true);
                                    }
                                },
                                if blocked {
                                    {i18n.t("console.providers.unblock")}
                                } else {
                                    {i18n.t("console.providers.blocklistCta")}
                                }
                            }
                        }
                    }
                }
                if can_delete {
                    TypeToConfirm {
                        title: i18n.t("console.providers.delete"),
                        body: radius,
                        expect: provider.slug.clone(),
                        cta: i18n.t("console.providers.deleteCta"),
                        busy: busy.is_busy(),
                        on_confirm: delete,
                    }
                }
            }
            StepUpGuard { gate, intro: Some(i18n.t("console.stepUp.intro")) }
            OutcomeLine { outcome: outcome.read().clone() }
        }
    }
}
