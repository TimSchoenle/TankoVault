//! Registering a new provider.

use crate::api;
use crate::components::{use_step_up_gate, OutcomeLine, StepUpGuard};
use crate::hooks::{use_busy, use_outcome, Reload};
use crate::i18n::use_i18n;
use crate::models::*;
use crate::views::console::{adapter_label_key, ADAPTER_KINDS};
use dioxus::prelude::*;
use inkstone_ui::{Button, Size, Tone};
/// Why the form would not submit, as the operator has to be told.
enum Rejected {
    /// The catalogue key of a self-contained message.
    Message(&'static str),
    /// The adapter config did not parse; the serde message names the offending position.
    BadConfig(String),
}

impl Rejected {
    fn wording(self, i18n: crate::i18n::Translator) -> String {
        match self {
            Self::Message(key) => i18n.t(key),
            Self::BadConfig(detail) => {
                i18n.args("console.providers.badConfig", &[("message", &detail)])
            }
        }
    }
}

/// Assemble the registration body from the form's raw fields.
///
/// # Errors
///
/// [`Rejected`] when a required field is blank, the config is not JSON, or the adapter token
/// is one this build does not know.
fn registration(
    slug: &str,
    name: &str,
    base_url: &str,
    adapter: &str,
    config: &str,
) -> Result<CreateProvider, Rejected> {
    let parsed = serde_json::from_str::<serde_json::Value>(config)
        .map_err(|e| Rejected::BadConfig(e.to_string()))?;
    let (slug, name, base_url) = (slug.trim(), name.trim(), base_url.trim());
    if slug.is_empty() || name.is_empty() || base_url.is_empty() {
        return Err(Rejected::Message("console.providers.missingFields"));
    }
    // The generated `FromStr`, not a local `match` with a `_ => Custom` arm: the token can only
    // have come from the picker below, so an unparseable one is a bug in this file rather than
    // a user's mistake — and defaulting to `Custom` is how that bug used to reach the database
    // as a silently mis-registered provider (FRONTEND F10).
    let adapter = adapter
        .parse::<AdapterKind>()
        .map_err(|_| Rejected::Message("console.providers.missingFields"))?;
    Ok(CreateProvider {
        slug: slug.to_owned(),
        name: name.to_owned(),
        base_url: base_url.to_owned(),
        adapter,
        config: Some(parsed),
        politeness: None,
    })
}

/// The fields a clone starts from: everything the form can carry, taken off the source
/// provider.
///
/// Politeness is deliberately not among them. A clone exists to point at a *different* site —
/// that is what the operator changes the base URL to — and a crawl budget tuned for one host is
/// a guess about the new one. The server's polite defaults apply, and the inspector tunes them.
#[derive(Clone, PartialEq)]
pub(super) struct CloneSeed {
    pub slug: String,
    pub name: String,
    pub base_url: String,
    pub adapter: String,
    pub config: String,
}

/// Register a provider. Politeness is left at the polite server defaults and tuned afterwards
/// from the provider's own inspector.
///
/// With a `seed`, this is the console's "clone": the same registration, opened with another
/// provider's fields filled in. A clone is therefore never preset-managed — it goes through the
/// same create endpoint as any hand-registered provider.
#[component]
pub(super) fn CreateProviderForm(
    reload: Reload,
    #[props(default)] seed: Option<CloneSeed>,
    on_done: EventHandler<()>,
) -> Element {
    let api = api::use_api();
    let i18n = use_i18n();
    let busy = use_busy();
    let mut outcome = use_outcome();
    // Elevated: registering a provider is a mutating operator capability, and the API answers
    // `403 step_up_required` until a second factor has been presented.
    let gate = use_step_up_gate();
    let cloning = seed.is_some();
    let mut slug = use_signal(|| seed.as_ref().map(|s| s.slug.clone()).unwrap_or_default());
    let mut name = use_signal(|| seed.as_ref().map(|s| s.name.clone()).unwrap_or_default());
    let mut base_url = use_signal(|| {
        seed.as_ref()
            .map(|s| s.base_url.clone())
            .unwrap_or_default()
    });
    let mut adapter = use_signal(|| {
        seed.as_ref()
            .map_or_else(|| "generic_config".to_owned(), |s| s.adapter.clone())
    });
    let mut config = use_signal(|| {
        seed.as_ref()
            .map_or_else(|| "{}".to_owned(), |s| s.config.clone())
    });

    let submit = move |_| {
        gate.attempt(move || {
            if !busy.claim() {
                return;
            }
            outcome.set(None);
            let body = match registration(
                &slug.peek(),
                &name.peek(),
                &base_url.peek(),
                &adapter.peek(),
                &config.peek(),
            ) {
                Ok(body) => body,
                Err(rejected) => {
                    outcome.set(Some(Err(rejected.wording(i18n))));
                    busy.release();
                    return;
                }
            };
            let client = gate.client(api);
            spawn(async move {
                match client.create_provider().body(body).send().await {
                    Ok(_) => {
                        slug.set(String::new());
                        name.set(String::new());
                        base_url.set(String::new());
                        config.set("{}".to_owned());
                        reload.bump();
                        on_done.call(());
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

    rsx! {
        div { style: "max-width:620px;",
            h2 { class: "ik-insp-title", style: "margin-bottom:16px;",
                if cloning {
                {i18n.t("console.providers.cloneTitle")}
                } else {
                {i18n.t("console.providers.add")}
                }
            }
            if cloning {
                p { class: "ik-muted", style: "font-size:11.5px;line-height:1.5;margin:-8px 0 14px;",
                    {i18n.t("console.providers.cloneIntro")}
                }
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
                StepUpGuard { gate, intro: Some(i18n.t("console.stepUp.intro")) }
                OutcomeLine { outcome: outcome.read().clone() }
                div { class: "ik-flex", style: "gap:8px;",
                    Button {
                        size: Size::Sm,
                        tone: Tone::Primary,
                        disabled: busy.is_busy(),
                        on_click: submit,
                        {i18n.t("console.providers.create")}
                    }
                    Button {
                        size: Size::Sm,
                        on_click: move |_| on_done.call(()),
                        {i18n.t("common.cancel")}
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(adapter: &str) -> Result<CreateProvider, Rejected> {
        registration(
            "  acme-scans ",
            " Acme Scans ",
            " https://acmescans.example ",
            adapter,
            r#"{"list":"/series"}"#,
        )
    }

    /// An adapter token the picker cannot have produced must refuse the form.
    ///
    /// The defect this closes (FRONTEND F10): the token was parsed with a `_ =>
    /// AdapterKind::Custom` arm, so one wrong character registered the provider as `Custom` —
    /// a form that looked like it worked, producing a provider that scans nothing.
    #[test]
    fn an_unknown_adapter_token_is_refused_rather_than_registered_as_custom() {
        assert!(matches!(ok("madarra"), Err(Rejected::Message(_))));
        assert!(matches!(ok(""), Err(Rejected::Message(_))));
        assert_eq!(
            ok("madara").ok().map(|body| body.adapter),
            Some(AdapterKind::Madara)
        );
    }

    /// The three identity fields are trimmed before they are checked *and* before they are
    /// sent: a slug that is only spaces is missing, and a slug with a trailing space is a
    /// different provider from the one the operator meant to type.
    #[test]
    fn the_identity_fields_are_trimmed_and_a_blank_one_is_missing() {
        let body = ok("generic_config").ok().expect("the form is complete");
        assert_eq!(body.slug, "acme-scans");
        assert_eq!(body.name, "Acme Scans");
        assert_eq!(body.base_url, "https://acmescans.example");

        for blank in ["", "   "] {
            assert!(matches!(
                registration(blank, "n", "https://b", "generic_config", "{}"),
                Err(Rejected::Message(_))
            ));
            assert!(matches!(
                registration("s", blank, "https://b", "generic_config", "{}"),
                Err(Rejected::Message(_))
            ));
            assert!(matches!(
                registration("s", "n", blank, "generic_config", "{}"),
                Err(Rejected::Message(_))
            ));
        }
    }

    /// A config that is not JSON reports the parser's own position, which is the only thing
    /// that makes a mistyped adapter config findable in a full page of it.
    #[test]
    fn an_unparseable_config_carries_the_parser_message() {
        let Err(Rejected::BadConfig(detail)) =
            registration("s", "n", "https://b", "generic_config", "{ nope }")
        else {
            panic!("a malformed config must be refused as a config error");
        };
        assert!(!detail.is_empty());
    }

    /// Politeness is never guessed here: the server's own defaults are the polite ones, and
    /// sending a client-side guess would register a provider crawling at whatever this form
    /// happened to seed.
    #[test]
    fn registration_leaves_politeness_to_the_server() {
        let body = ok("generic_config").ok().expect("the form is complete");
        assert!(body.politeness.is_none());
    }
}
