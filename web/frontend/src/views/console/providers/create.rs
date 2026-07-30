//! Registering a new provider.

use crate::api;
use crate::components::OutcomeLine;
use crate::hooks::{use_busy, use_outcome, Reload};
use crate::i18n::use_i18n;
use crate::models::*;
use crate::views::console::{adapter_label_key, ADAPTER_KINDS};
use dioxus::prelude::*;

/// Register a provider. Politeness is left at the polite server defaults and tuned afterwards
/// from the provider's own inspector.
#[component]
pub(super) fn CreateProviderForm(reload: Reload, on_done: EventHandler<()>) -> Element {
    let api = api::use_api();
    let i18n = use_i18n();
    let busy = use_busy();
    let mut outcome = use_outcome();
    let mut slug = use_signal(String::new);
    let mut name = use_signal(String::new);
    let mut base_url = use_signal(String::new);
    let mut adapter = use_signal(|| "generic_config".to_owned());
    let mut config = use_signal(|| "{}".to_owned());

    let submit = move |_| {
        if !busy.claim() {
            return;
        }
        outcome.set(None);
        let parsed = match serde_json::from_str::<serde_json::Value>(&config.peek()) {
            Ok(value) => value,
            Err(e) => {
                outcome.set(Some(Err(i18n.args(
                    "console.providers.badConfig",
                    &[("message", &e.to_string())],
                ))));
                busy.release();
                return;
            }
        };
        let (s, n, b) = (
            slug.peek().trim().to_owned(),
            name.peek().trim().to_owned(),
            base_url.peek().trim().to_owned(),
        );
        if s.is_empty() || n.is_empty() || b.is_empty() {
            outcome.set(Some(Err(i18n.t("console.providers.missingFields"))));
            busy.release();
            return;
        }
        // The generated `FromStr`, not a local `match` with a `_ => Custom` arm: the token can
        // only have come from the picker below, so an unparseable one is a bug in this file
        // rather than a user's mistake — and defaulting to `Custom` is how that bug used to
        // reach the database as a silently mis-registered provider (FRONTEND F10).
        let Ok(kind) = adapter.peek().parse::<AdapterKind>() else {
            outcome.set(Some(Err(i18n.t("console.providers.missingFields"))));
            busy.release();
            return;
        };
        let client = api.client();
        spawn(async move {
            let body = CreateProvider {
                slug: s,
                name: n,
                base_url: b,
                adapter: kind,
                config: Some(parsed),
                politeness: None,
            };
            match client.create_provider().body(body).send().await {
                Ok(_) => {
                    slug.set(String::new());
                    name.set(String::new());
                    base_url.set(String::new());
                    config.set("{}".to_owned());
                    reload.bump();
                    on_done.call(());
                }
                Err(e) => outcome.set(Some(Err(api::friendly_error(i18n, e)))),
            }
            busy.release();
        });
    };

    rsx! {
        div { style: "max-width:620px;",
            h2 { class: "ik-insp-title", style: "margin-bottom:16px;",
                {i18n.t("console.providers.add")}
            }
            div { style: "display:flex;flex-direction:column;gap:10px;",
                div { class: "ik-kv",
                    label { class: "k", r#for: "tv-new-slug", {i18n.t("console.providers.field.slug")} }
                    input {
                        id: "tv-new-slug",
                        class: "ik-input ik-mono",
                        // An illustrative slug, not copy: a slug is `[a-z0-9-]` in every
                        // locale, and so is the example URL two fields down. The display-name
                        // example beside them *is* copy, so it comes from the catalogue.
                        placeholder: "acme-scans",
                        value: "{slug}",
                        oninput: move |e| slug.set(e.value()),
                    }
                }
                div { class: "ik-kv",
                    label { class: "k", r#for: "tv-new-name", {i18n.t("console.providers.field.name")} }
                    input {
                        id: "tv-new-name",
                        class: "ik-input",
                        placeholder: i18n.t("console.providers.namePlaceholder"),
                        value: "{name}",
                        oninput: move |e| name.set(e.value()),
                    }
                }
                div { class: "ik-kv",
                    label { class: "k", r#for: "tv-new-base", {i18n.t("console.providers.field.baseUrl")} }
                    input {
                        id: "tv-new-base",
                        class: "ik-input ik-mono",
                        placeholder: "https://acmescans.example",
                        value: "{base_url}",
                        oninput: move |e| base_url.set(e.value()),
                    }
                }
                div { class: "ik-kv",
                    label { class: "k", r#for: "tv-new-adapter", {i18n.t("console.providers.field.adapter")} }
                    select {
                        id: "tv-new-adapter",
                        class: "ik-select",
                        value: "{adapter}",
                        onchange: move |e| adapter.set(e.value()),
                        for kind in ADAPTER_KINDS.iter().copied() {
                            option { key: "{kind}", value: "{kind}", {i18n.t(adapter_label_key(kind))} }
                        }
                    }
                }
                div {
                    div { class: "ik-sec-lbl", style: "margin-bottom:8px;",
                        {i18n.t("console.providers.adapterConfig")}
                    }
                    textarea {
                        class: "ik-jsonblock",
                        spellcheck: "false",
                        "aria-label": i18n.t("console.providers.adapterConfig"),
                        value: "{config}",
                        oninput: move |e| config.set(e.value()),
                    }
                }
                OutcomeLine { outcome: outcome.read().clone() }
                div { class: "ik-flex", style: "gap:8px;",
                    button {
                        class: "ik-btn sm primary",
                        disabled: busy.is_busy(),
                        onclick: submit,
                        {i18n.t("console.providers.create")}
                    }
                    button {
                        class: "ik-btn sm",
                        onclick: move |_| on_done.call(()),
                        {i18n.t("common.cancel")}
                    }
                }
            }
        }
    }
}
