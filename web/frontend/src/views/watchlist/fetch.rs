//! Owns the watchlist's requests: the slice on screen, the sync account's status, and a two-way
//! sync.

use dioxus::prelude::*;
use progenitor_client::ResponseValue;

use super::{bite_size, watchlist_slice, Request, SYNC_PROVIDER};
use crate::api::{self, Api};
use crate::components::{unmeasured, GridFit};
use crate::hooks::{Busy, Outcome, Reload};
use crate::i18n::Translator;
use crate::models::*;
use crate::state::Session;

/// The slice of the watchlist that `request` asks for, paired with the request it answers.
pub(super) fn use_page(
    request: Signal<Request>,
    fit: GridFit,
    reload: Reload,
    api: Api,
    session: Session,
    i18n: Translator,
) -> Resource<(Request, Result<WatchlistView, String>)> {
    use_resource(move || {
        reload.track();
        let req = request.read().clone();
        // In the grid view the bite is whole rows of tiles, and the fetch waits for the first
        // measurement instead of firing a request the corrected one would immediately replace.
        let bite = bite_size(req.query.view, fit);
        let client = api.client();
        let authed = session.is_authenticated();
        async move {
            if !authed {
                return (req, Ok(signed_out()));
            }
            let Some(bite) = bite else {
                return unmeasured().await;
            };
            let outcome = watchlist_slice(&client, &req.query, req.limit(bite), req.skipped(bite))
                .send()
                .await
                .map(ResponseValue::into_inner)
                .map_err(|e| api::friendly_error(i18n, e));
            // The request travels with its own answer, so the chrome can tell a rendered window
            // from one still on the wire without a second flag to keep in step.
            (req, outcome)
        }
    })
}

/// The empty view a signed-out reader is shown.
fn signed_out() -> WatchlistView {
    WatchlistView {
        items: Vec::new(),
        counts: WatchlistCounts {
            reading: 0,
            planned: 0,
            paused: 0,
            completed: 0,
            dropped: 0,
            all: 0,
            source_issues: 0,
        },
        groups: Vec::new(),
        total: 0,
        // Offset paging, not keyset: this list refetches its whole window on every
        // page instead of appending, which is immune to the shifting-list defect the
        // cursor exists to fix and cannot use a cursor to express "rows 120 to 180".
        // The token is there for consumers that do append.
        next_cursor: None,
    }
}

/// The sync account's status. Best-effort: a down sync service must not take the watchlist with
/// it (same as Discover's facets).
pub(super) fn use_sync_status(
    reload: Reload,
    api: Api,
    session: Session,
) -> Resource<Option<SyncAccountStatus>> {
    use_resource(move || {
        reload.track();
        let client = api.client();
        let authed = session.is_authenticated();
        async move {
            if !authed {
                return None;
            }
            client
                .sync_status()
                .provider(SYNC_PROVIDER)
                .send()
                .await
                .ok()
                .map(ResponseValue::into_inner)
        }
    })
}

/// Pulls from the sync provider, then pushes to it, and reports the outcome.
pub(super) fn sync_both_ways(
    api: Api,
    i18n: Translator,
    reload: Reload,
    syncing: Busy,
    mut outcome: Signal<Outcome>,
) {
    if !syncing.claim() {
        return;
    }
    outcome.set(None);
    let client = api.client();
    spawn(async move {
        let opts = SyncOpts {
            policy: Some(ConflictPolicy::NewestWins),
        };
        // Pull before push, or a title just added on the other side gets overwritten.
        let result = match client
            .sync_pull()
            .provider(SYNC_PROVIDER)
            .body(opts.clone())
            .send()
            .await
        {
            Ok(_) => client
                .sync_push()
                .provider(SYNC_PROVIDER)
                .body(opts)
                .send()
                .await
                .map(|_| i18n.t("watchlist.synced"))
                .map_err(|e| api::friendly_error(i18n, e)),
            Err(e) => Err(api::friendly_error(i18n, e)),
        };
        if result.is_ok() {
            reload.bump();
        }
        outcome.set(Some(result));
        syncing.release();
    });
}
