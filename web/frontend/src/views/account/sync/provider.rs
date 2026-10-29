//! Owns one provider card's requests: its link status and automatic-sync settings, linking and
//! unlinking the account, changing the settings, and a manual pull or push.

use dioxus::prelude::*;
use progenitor_client::ResponseValue;

use crate::api::{self, Api};
use crate::components::{use_step_up_gate, StepUpGate};
use crate::hooks::{use_busy, use_outcome, use_reload, Busy, Outcome, Reload};
use crate::i18n::{use_i18n, Translator};
use crate::models::*;
use crate::state::{use_session, Session};
use crate::wire::types::AccountSettings;

/// Which way a manual sync moves the watchlist.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Direction {
    /// From the provider into the watchlist.
    Pull,
    /// From the watchlist to the provider.
    Push,
}

/// One provider card's handles. Every field is `Copy`, so each button's handler takes its own.
#[derive(Clone, Copy)]
pub(super) struct ProviderSync {
    pub(super) session: Session,
    pub(super) i18n: Translator,
    pub(super) api: Api,
    pub(super) gate: StepUpGate,
    pub(super) reload: Reload,
    pub(super) busy: Busy,
    pub(super) outcome: Signal<Outcome>,
}

impl ProviderSync {
    /// The card's handles, with no outcome reported yet.
    pub(super) fn use_new() -> Self {
        Self {
            session: use_session(),
            i18n: use_i18n(),
            api: api::use_api(),
            gate: use_step_up_gate(),
            reload: use_reload(),
            busy: use_busy(),
            outcome: use_outcome(),
        }
    }

    /// Whether the account is linked to the provider `slug`, and as whom.
    pub(super) fn use_status(self, slug: &str) -> Resource<Result<SyncAccountStatus, String>> {
        let Self {
            session,
            i18n,
            api,
            reload,
            ..
        } = self;
        use_resource({
            let slug = slug.to_owned();
            move || {
                reload.track();
                let slug = slug.clone();
                let client = api.client();
                let authed = session.is_authenticated();
                async move {
                    if !authed {
                        return Ok(SyncAccountStatus {
                            linked: false,
                            username: None,
                            last_synced_at: None,
                        });
                    }
                    client
                        .sync_status()
                        .provider(slug)
                        .send()
                        .await
                        .map(ResponseValue::into_inner)
                        .map_err(|e| api::friendly_error(i18n, e))
                }
            }
        })
    }

    /// The account's automatic-sync settings for the provider `slug`, or `None` when signed
    /// out or the request fails.
    pub(super) fn use_settings(self, slug: &str) -> Resource<Option<AccountSettings>> {
        let Self {
            session,
            api,
            reload,
            ..
        } = self;
        use_resource({
            let slug = slug.to_owned();
            move || {
                reload.track();
                let slug = slug.clone();
                let client = api.client();
                let authed = session.is_authenticated();
                async move {
                    if !authed {
                        return None;
                    }
                    client
                        .sync_settings()
                        .provider(slug)
                        .send()
                        .await
                        .ok()
                        .map(ResponseValue::into_inner)
                }
            }
        })
    }

    /// Saves a change to the provider's automatic-sync settings, then refetches.
    pub(super) fn patch_settings(self, slug: String, patch: SyncSettingsPatch) {
        let Self { api, reload, .. } = self;
        let client = api.client();
        spawn(async move {
            let _ = client
                .sync_settings_patch()
                .provider(slug)
                .body(patch)
                .send()
                .await;
            reload.bump();
        });
    }

    /// Links the account: fetches the provider's consent URL and leaves for it.
    pub(super) fn connect(self, slug: String) {
        let Self {
            i18n,
            api,
            gate,
            mut outcome,
            ..
        } = self;
        gate.attempt(move || {
            outcome.set(None);
            let slug = slug.clone();
            // Elevated, exactly like the unlink below: following the URL this returns grants
            // a third party a standing OAuth token against the caller's account there, so the
            // API answers `403` until a second factor has been presented.
            let client = gate.client(api);
            spawn(async move {
                match client.sync_authorize_url().provider(slug).send().await {
                    Ok(response) => {
                        // A full-page navigation, not a router push: the consent screen lives
                        // on the provider's origin.
                        crate::platform::navigate_to(&response.into_inner().url);
                    }
                    Err(e) => {
                        if !gate.refused(api::Refusal::of(&e)) {
                            outcome.set(Some(Err(api::friendly_error(i18n, e))));
                        }
                    }
                }
            });
        });
    }

    /// Unlinks the account from the provider `name`.
    pub(super) fn disconnect(self, slug: String, name: String) {
        let Self {
            i18n,
            api,
            gate,
            reload,
            busy,
            mut outcome,
            ..
        } = self;
        gate.attempt(move || {
            if !busy.claim() {
                return;
            }
            outcome.set(None);
            let (slug, name) = (slug.clone(), name.clone());
            // Elevated: unlinking a tracker is an account change, so the API demands a second
            // factor and answers `403` until it has one.
            let client = gate.client(api);
            spawn(async move {
                match client.sync_disconnect().provider(slug).send().await {
                    Ok(_) => {
                        outcome.set(Some(Ok(
                            i18n.args("account.sync.disconnected", &[("provider", &name)])
                        )));
                        reload.bump();
                    }
                    Err(e) => {
                        if !gate.refused(api::Refusal::of(&e)) {
                            outcome.set(Some(Err(api::friendly_error(i18n, e))));
                        }
                    }
                }
                busy.release();
            });
        });
    }

    /// Starts a manual sync in `direction`, resolving conflicts by `policy`.
    pub(super) fn transfer(self, slug: String, direction: Direction, policy: ConflictPolicy) {
        let Self {
            i18n,
            api,
            reload,
            busy,
            mut outcome,
            ..
        } = self;
        if !busy.claim() {
            return;
        }
        outcome.set(None);
        let client = api.client();
        spawn(async move {
            let opts = SyncOpts {
                policy: Some(policy),
            };
            let (sent, started) = match direction {
                Direction::Pull => (
                    client
                        .sync_pull()
                        .provider(slug)
                        .body(opts)
                        .send()
                        .await
                        .map(drop),
                    "account.sync.pullStarted",
                ),
                Direction::Push => (
                    client
                        .sync_push()
                        .provider(slug)
                        .body(opts)
                        .send()
                        .await
                        .map(drop),
                    "account.sync.pushStarted",
                ),
            };
            match sent {
                Ok(()) => {
                    outcome.set(Some(Ok(i18n.t(started))));
                    reload.bump();
                }
                Err(e) => outcome.set(Some(Err(api::friendly_error(i18n, e)))),
            }
            busy.release();
        });
    }
}
