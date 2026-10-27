//! The provider inspector's state and its writes: the edit fields, the dry run, saving, the
//! state and preset-lock toggles, and a manual scan.

use dioxus::prelude::*;
use progenitor_client::ResponseValue;

use super::politeness::{emulation_token, politeness_body};
use crate::api::{self, Api};
use crate::components::{use_step_up_gate, StepUpGate};
use crate::hooks::{use_busy, use_outcome, Busy, Outcome, Reload};
use crate::i18n::{use_i18n, Translator};
use crate::models::*;
use crate::state::capabilities::use_capabilities;
use crate::views::console::config_editor_text;
use crate::wire::types::Permission;

/// What the operator may do to the inspected provider. One control per capability: each
/// appears exactly when the server would accept the call.
#[derive(Clone, Copy)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "each flag is one independent capability the server checks on its own"
)]
pub(super) struct Rights {
    /// Edit the provider.
    pub(super) edit: bool,
    /// Clone it. A clone is a registration, so it needs the registration capability.
    pub(super) create: bool,
    /// Pause or enable it.
    pub(super) change_state: bool,
    /// Queue a scan of it.
    pub(super) scan: bool,
    /// Dry-run its adapter config.
    pub(super) test: bool,
    /// Delete it.
    pub(super) delete: bool,
    /// Edit the preset-owned fields: name, base URL and config. Everything preset-owned is
    /// read-only while the preset link is locked; politeness never is.
    pub(super) edit_owned: bool,
}

impl Rights {
    /// The signed-in operator's rights over `provider`.
    ///
    /// `edit_owned` mirrors the server: a PATCH touching a preset-owned field of a locked
    /// provider is refused with a `409`, so disabling the inputs is the courtesy, not the
    /// enforcement.
    pub(super) fn use_for(provider: &Provider) -> Self {
        let caps = use_capabilities();
        let edit = caps.can(Permission::ProvidersWrite);
        let locked = provider.preset.as_ref().is_some_and(|link| link.locked);
        Self {
            edit,
            create: caps.can(Permission::ProvidersCreate),
            change_state: caps.can(Permission::ProvidersState),
            scan: caps.can(Permission::ScansRun),
            test: caps.can(Permission::ProvidersTest),
            delete: caps.can(Permission::ProvidersDelete),
            edit_owned: edit && !locked,
        }
    }
}

/// The inspector's handles and edit fields. Every field is `Copy`, so each handler takes its
/// own.
#[derive(Clone, Copy)]
pub(super) struct Inspector {
    pub(super) api: Api,
    pub(super) i18n: Translator,
    pub(super) busy: Busy,
    pub(super) outcome: Signal<Outcome>,
    /// One gate for the inspector: every write in it is elevated, and it has one outcome line
    /// to answer through.
    pub(super) gate: StepUpGate,
    pub(super) reload: Reload,
    pub(super) name: Signal<String>,
    pub(super) base_url: Signal<String>,
    pub(super) config: Signal<String>,
    /// The dry-run gate: cleared by every config edit, re-armed by a passing dry run.
    pub(super) dry_run_passed: Signal<bool>,
    pub(super) dry_run: Signal<Option<Result<serde_json::Value, String>>>,
    pub(super) rps: Signal<f64>,
    pub(super) concurrency: Signal<f64>,
    pub(super) crawl_delay: Signal<f64>,
    pub(super) user_agent: Signal<String>,
    pub(super) emulation: Signal<String>,
    /// Fast checks deltas; full re-reads the catalogue for this provider only.
    pub(super) scan_mode: Signal<ScanMode>,
    /// Whether the re-lock confirmation is open.
    pub(super) relinking: Signal<bool>,
}

impl Inspector {
    /// The inspector's state, with the edit fields seeded from `provider`.
    pub(super) fn use_new(provider: &Provider, reload: Reload) -> Self {
        let politeness = &provider.politeness;
        Self {
            api: api::use_api(),
            i18n: use_i18n(),
            busy: use_busy(),
            outcome: use_outcome(),
            gate: use_step_up_gate(),
            reload,
            name: use_signal(|| provider.name.clone()),
            base_url: use_signal(|| provider.base_url.clone()),
            config: use_signal(|| config_editor_text(&provider.config)),
            dry_run_passed: use_signal(|| false),
            dry_run: use_signal(|| None),
            rps: use_signal(|| politeness.rps.unwrap_or(1.0)),
            concurrency: use_signal(|| f64::from(politeness.concurrency.unwrap_or(2))),
            crawl_delay: use_signal(|| delay_ms(politeness.crawl_delay_ms)),
            user_agent: use_signal(|| politeness.user_agent.clone().unwrap_or_default()),
            emulation: use_signal(|| emulation_token(politeness.emulation.as_ref())),
            scan_mode: use_signal(|| ScanMode::Fast),
            relinking: use_signal(|| false),
        }
    }

    /// Save every field to provider `id`. A base URL that differs from `original_base` is a
    /// migration, and the outcome says so.
    pub(super) fn save(self, id: ProviderId, original_base: &str) {
        let Self {
            api,
            i18n,
            busy,
            mut outcome,
            gate,
            reload,
            name,
            base_url,
            config,
            mut dry_run_passed,
            rps,
            concurrency,
            crawl_delay,
            user_agent,
            emulation,
            ..
        } = self;
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
        let politeness = match politeness_body(
            &format!("{}", *rps.peek()),
            &format!("{:.0}", *concurrency.peek()),
            &format!("{:.0}", *crawl_delay.peek()),
            &user_agent.peek(),
            &emulation.peek(),
        ) {
            Ok(value) => value,
            Err(key) => {
                outcome.set(Some(Err(i18n.t(key))));
                busy.release();
                return;
            }
        };
        let migrating = *base_url.peek() != original_base;
        let body = UpdateProvider {
            name: name.peek().clone(),
            base_url: base_url.peek().clone(),
            config: Some(parsed),
            politeness: Some(politeness),
        };
        let client = gate.client(api);
        spawn(async move {
            match client.update_provider().id(id).body(body).send().await {
                Ok(_) => {
                    outcome.set(Some(Ok(i18n.t(if migrating {
                        "console.providers.savedMigrating"
                    } else {
                        "console.providers.saved"
                    }))));
                    dry_run_passed.set(false);
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
    }

    /// Move provider `id` to `target`.
    pub(super) fn set_state(self, id: ProviderId, target: ProviderState) {
        let Self {
            api,
            i18n,
            busy,
            mut outcome,
            gate,
            reload,
            ..
        } = self;
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
                Ok(_) => reload.bump(),
                Err(e) => {
                    if !gate.refused(api::Refusal::of(&e)) {
                        outcome.set(Some(Err(api::guarded_error(i18n, e))));
                    }
                }
            }
            busy.release();
        });
    }

    /// Queue a scan of provider `id` in the chosen mode.
    pub(super) fn scan(self, id: ProviderId) {
        let Self {
            api,
            i18n,
            mut outcome,
            gate,
            scan_mode,
            ..
        } = self;
        outcome.set(None);
        let client = gate.client(api);
        spawn(async move {
            let body = TriggerScan {
                mode: *scan_mode.peek(),
                provider_id: Some(id),
            };
            match client.trigger_scan().body(body).send().await {
                Ok(_) => outcome.set(Some(Ok(i18n.t("console.providers.scanQueued")))),
                Err(e) => {
                    if !gate.refused(api::Refusal::of(&e)) {
                        outcome.set(Some(Err(api::guarded_error(i18n, e))));
                    }
                }
            }
        });
    }

    /// Lock or unlock provider `id`'s preset link. Re-locking discards the local edits to the
    /// preset-owned fields, so the caller puts it behind a confirmation.
    pub(super) fn set_lock(self, id: ProviderId, lock: bool) {
        let Self {
            api,
            i18n,
            busy,
            mut outcome,
            gate,
            reload,
            ..
        } = self;
        if !busy.claim() {
            return;
        }
        outcome.set(None);
        let client = gate.client(api);
        spawn(async move {
            match client
                .set_provider_preset_lock()
                .id(id)
                .body(SetPresetLockBody { locked: lock })
                .send()
                .await
            {
                Ok(_) => {
                    outcome.set(Some(Ok(i18n.t(if lock {
                        "console.providers.preset.relinked"
                    } else {
                        "console.providers.preset.unlocked"
                    }))));
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
    }

    /// Dry-run provider `id`'s adapter, arming the save gate when it passes.
    pub(super) fn run_dry(self, id: ProviderId) {
        let Self {
            api,
            i18n,
            busy,
            mut dry_run_passed,
            mut dry_run,
            ..
        } = self;
        if !busy.claim() {
            return;
        }
        dry_run.set(None);
        let client = api.client();
        spawn(async move {
            let result = client
                .test_adapter()
                .id(id)
                .body(TestAdapterRequest { path: None })
                .send()
                .await
                .map(ResponseValue::into_inner)
                .map_err(|e| api::friendly_error(i18n, e));
            dry_run_passed.set(result.is_ok());
            dry_run.set(Some(result));
            busy.release();
        });
    }

    /// Reformat the config text, when it parses.
    pub(super) fn format_config(self) {
        let mut config = self.config;
        // Read the text out before writing back: `peek` holds a borrow for the whole `if let`.
        let text = config.peek().clone();
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) {
            config.set(config_editor_text(&value));
        }
    }

    /// Set the request budget to `preset`'s.
    pub(super) fn adopt_budget(self, preset: &PresetDefinition) {
        let Self {
            mut rps,
            mut concurrency,
            mut crawl_delay,
            ..
        } = self;
        let budget = &preset.politeness;
        rps.set(budget.rps.unwrap_or(1.0));
        concurrency.set(f64::from(budget.concurrency.unwrap_or(2)));
        crawl_delay.set(delay_ms(budget.crawl_delay_ms));
    }
}

/// A stored crawl delay as the slider's value; unset is no delay.
#[expect(
    clippy::cast_precision_loss,
    reason = "crawl delays are milliseconds, well inside f64's exact integer range"
)]
fn delay_ms(stored: Option<i64>) -> f64 {
    stored.unwrap_or(0) as f64
}
