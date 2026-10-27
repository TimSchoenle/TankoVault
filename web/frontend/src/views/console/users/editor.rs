//! The account inspector's state and its writes: the edit fields, saving them, signing the
//! account out everywhere and erasing it.

use std::collections::BTreeSet;

use dioxus::prelude::*;
use progenitor_client::Error as ApiOpError;

use super::known_permissions;
use crate::api::{self, Api};
use crate::components::{use_step_up_gate, StepUpGate};
use crate::hooks::{use_busy, use_outcome, Busy, Outcome, Reload};
use crate::i18n::{use_i18n, Translator};
use crate::state::capabilities::use_capabilities;
use crate::state::use_session;
use crate::wire::types::{
    AdminProfileUpdate, DeleteUser, Permission, ProblemDetails, SetPermissions, SetUserStatus,
    UserDetail, UserDetailResponse,
};

/// What the operator may do to the inspected account.
#[derive(Clone, Copy)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "each flag is one independent capability the server checks on its own"
)]
pub(super) struct Rights {
    /// Edit the identity and the status.
    pub(super) write: bool,
    /// Change the permission grants.
    pub(super) grant: bool,
    /// Sign the account out of its sessions.
    pub(super) sessions: bool,
    /// Erase the account.
    pub(super) delete: bool,
    /// Act on the account's external tracker links.
    pub(super) sync: bool,
    /// The account is the operator's own. The server refuses an administrator acting on their
    /// own account, so the inspector says so instead of looking broken.
    pub(super) is_self: bool,
}

impl Rights {
    /// The signed-in operator's rights over `user`.
    pub(super) fn use_for(user: &UserDetail) -> Self {
        let caps = use_capabilities();
        let session = use_session();
        Self {
            write: caps.can(Permission::UsersWrite),
            grant: caps.can(Permission::UsersPermissions),
            sessions: caps.can(Permission::UsersSessions),
            delete: caps.can(Permission::UsersDelete),
            sync: caps.can(Permission::SyncAdminWrite),
            is_self: session.username().is_some_and(|name| name == user.username),
        }
    }
}

/// Which parts of the account differ from the server's copy and may be written.
#[derive(Clone, Copy)]
pub(super) struct Changes {
    pub(super) identity: bool,
    pub(super) status: bool,
    pub(super) grants: bool,
}

impl Changes {
    /// Whether there is anything to save.
    pub(super) fn any(self) -> bool {
        self.identity || self.status || self.grants
    }
}

/// The inspector's handles and edit fields. Every field is `Copy`, so each handler takes its
/// own.
#[derive(Clone, Copy)]
pub(super) struct Editor {
    pub(super) api: Api,
    pub(super) i18n: Translator,
    pub(super) busy: Busy,
    pub(super) outcome: Signal<Outcome>,
    /// One gate for the whole inspector: every write in it is elevated, and it has one outcome
    /// line to answer through.
    pub(super) gate: StepUpGate,
    /// Refetches the directory.
    pub(super) reload: Reload,
    /// Refetches the inspected account.
    pub(super) detail_reload: Reload,
    pub(super) name: Signal<String>,
    pub(super) email: Signal<String>,
    pub(super) status: Signal<crate::wire::types::AccountStatus>,
    pub(super) suspend_reason: Signal<String>,
    /// Recorded in the audit trail, the only place it survives the erasure.
    pub(super) erase_reason: Signal<String>,
    /// The grants as edited, seeded with the recognised tokens the server returned.
    pub(super) chosen: Signal<BTreeSet<Permission>>,
}

/// Why the save chain stopped before its last call.
enum Halt {
    /// The API demanded a step-up. The prompt is open and the gate replays the save once it is
    /// answered, so there is nothing to report.
    Prompted,
    /// A call failed, with the message to show.
    Failed(String),
}

impl Editor {
    /// The inspector's state, with the edit fields seeded from `data`.
    pub(super) fn use_new(
        data: &UserDetailResponse,
        reload: Reload,
        detail_reload: Reload,
    ) -> Self {
        let user = &data.user;
        Self {
            api: api::use_api(),
            i18n: use_i18n(),
            busy: use_busy(),
            outcome: use_outcome(),
            gate: use_step_up_gate(),
            reload,
            detail_reload,
            name: use_signal(|| user.username.clone()),
            email: use_signal(|| user.email.clone()),
            status: use_signal(|| user.status),
            suspend_reason: use_signal(String::new),
            erase_reason: use_signal(String::new),
            chosen: use_signal(|| known_permissions(&data.permissions)),
        }
    }

    /// What differs from `data` and `rights` allows writing.
    ///
    /// The grants are compared against a set built from `data` on every render, not a memo:
    /// `data` changes when the account refetches, and a memo would freeze the comparison.
    pub(super) fn changes(self, data: &UserDetailResponse, rights: Rights) -> Changes {
        let user = &data.user;
        Changes {
            identity: rights.write
                && (*self.name.read() != user.username || *self.email.read() != user.email),
            status: rights.write && *self.status.read() != user.status,
            grants: rights.grant && *self.chosen.read() != known_permissions(&data.permissions),
        }
    }

    /// Save `changes` to `user` in up to three calls, in order, stopping at the first failure,
    /// so a rejected rename never applies a grant change beside it.
    pub(super) fn save(self, user: &UserDetail, changes: Changes) {
        let Self {
            api,
            i18n,
            busy,
            mut outcome,
            gate,
            reload,
            detail_reload,
            name,
            email,
            status,
            mut suspend_reason,
            chosen,
            ..
        } = self;
        if !busy.claim() {
            return;
        }
        outcome.set(None);
        let id = user.id.clone();
        let identity = changes.identity.then(|| AdminProfileUpdate {
            username: Some(name.peek().trim().to_owned()).filter(|v| *v != user.username),
            email: Some(email.peek().trim().to_owned()).filter(|v| *v != user.email),
        });
        let status = changes.status.then(|| SetUserStatus {
            status: *status.peek(),
            reason: Some(suspend_reason.peek().trim().to_owned()).filter(|v| !v.is_empty()),
            // A suspension that leaves the account working until its access token expires is not
            // what anyone means by suspending it.
            revoke_sessions: Some(true),
        });
        let grants = changes.grants.then(|| SetPermissions {
            permissions: chosen.peek().iter().copied().collect(),
        });
        let client = gate.client(api);
        let halt = move |e: ApiOpError<ProblemDetails>| {
            if gate.refused(api::Refusal::of(&e)) {
                Halt::Prompted
            } else {
                Halt::Failed(api::guarded_error(i18n, e))
            }
        };
        spawn(async move {
            let saved: Result<(), Halt> = async {
                if let Some(body) = identity {
                    let call = client.update_user().id(id.clone()).body(body);
                    call.send().await.map_err(halt)?;
                }
                if let Some(body) = status {
                    let call = client.set_user_status().id(id.clone()).body(body);
                    call.send().await.map_err(halt)?;
                }
                if let Some(body) = grants {
                    let call = client.set_user_permissions().id(id).body(body);
                    call.send().await.map_err(halt)?;
                }
                Ok(())
            }
            .await;
            match saved {
                Ok(()) => {
                    outcome.set(Some(Ok(i18n.t("console.users.saved"))));
                    suspend_reason.set(String::new());
                    detail_reload.bump();
                    reload.bump();
                }
                Err(Halt::Failed(message)) => outcome.set(Some(Err(message))),
                Err(Halt::Prompted) => {}
            }
            busy.release();
        });
    }

    /// Sign the account `user_id` out of every device.
    pub(super) fn revoke_sessions(self, user_id: String) {
        let Self {
            api,
            i18n,
            busy,
            mut outcome,
            gate,
            detail_reload,
            ..
        } = self;
        if !busy.claim() {
            return;
        }
        outcome.set(None);
        let client = gate.client(api);
        spawn(async move {
            match client.revoke_user_sessions().id(user_id).send().await {
                Ok(_) => {
                    outcome.set(Some(Ok(i18n.t("console.users.signedOutEverywhere"))));
                    detail_reload.bump();
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

    /// Erase the account `user_id`, recording `reason`, then call `on_erased`.
    pub(super) fn erase(
        self,
        user_id: String,
        username: String,
        reason: String,
        on_erased: EventHandler<()>,
    ) {
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
            let body = DeleteUser {
                // The server re-checks this against the account it is about to erase;
                // `TypeToConfirm` has already proved the operator typed it.
                confirm_username: username,
                reason: (!reason.is_empty()).then_some(reason),
            };
            match client.delete_user().id(user_id).body(body).send().await {
                Ok(_) => {
                    on_erased.call(());
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
}
