//! OAuth link `state` persistence (`crates/db/src/repo/sync/link_states.rs`).
//!
//! The `state` is what stops a link-CSRF: an attacker completing their own consent and landing a
//! victim's browser on the callback with the attacker's `code` (RFC 6749 §10.12). Every predicate
//! in `consume_link_state` fails open if dropped — the callback still succeeds — so each is
//! pinned here.
//!
//! Gated behind the `integration` feature (requires Docker).
#![cfg(feature = "integration")]

use tankovault_db::repo::sync::{consume_link_state, insert_link_state, prune_expired_link_states};
use tankovault_domain::UserId;
use tankovault_test_support::{TestDb, seed};
use time::{Duration, OffsetDateTime};

const PROVIDER: &str = "anilist";

async fn a_state(db: &TestDb, user: UserId, provider: &str, hash: &str, ttl: Duration) {
    insert_link_state(
        &db.pool,
        user,
        provider,
        hash,
        OffsetDateTime::now_utc() + ttl,
    )
    .await
    .expect("insert link state");
}

/// A state is spent by its first use. Before the table existed the `state` was a stateless HMAC,
/// so a leaked callback URL could be replayed for its whole ten-minute lifetime.
#[tokio::test]
async fn a_link_state_is_consumed_exactly_once() {
    let db = TestDb::spawn().await;
    let user = seed::user(&db, "linker").create().await;
    a_state(&db, user, PROVIDER, "state-once", Duration::minutes(10)).await;

    assert!(
        consume_link_state(&db.pool, user, PROVIDER, "state-once")
            .await
            .expect("consume")
    );
    assert!(
        !consume_link_state(&db.pool, user, PROVIDER, "state-once")
            .await
            .expect("consume"),
        "a replayed callback must find nothing"
    );
}

/// The link-CSRF itself: another account's `state` must not complete a link, and must survive
/// the attempt so the session that started the link can still finish it.
#[tokio::test]
async fn a_link_state_is_bound_to_its_account_and_provider() {
    let db = TestDb::spawn().await;
    let owner = seed::user(&db, "owner").create().await;
    let other = seed::user(&db, "other").create().await;
    a_state(&db, owner, PROVIDER, "state-bound", Duration::minutes(10)).await;

    assert!(
        !consume_link_state(&db.pool, other, PROVIDER, "state-bound")
            .await
            .expect("consume"),
        "a state issued to one account must not be spent by another"
    );
    assert!(
        !consume_link_state(&db.pool, owner, "mal", "state-bound")
            .await
            .expect("consume"),
        "a state issued for one provider must not link another"
    );
    assert!(
        consume_link_state(&db.pool, owner, PROVIDER, "state-bound")
            .await
            .expect("consume"),
        "refused attempts must not burn the owner's state"
    );
}

/// An expired state is refused even before the sweep has deleted it.
#[tokio::test]
async fn an_expired_link_state_is_refused_and_pruned() {
    let db = TestDb::spawn().await;
    let user = seed::user(&db, "late").create().await;
    a_state(&db, user, PROVIDER, "state-stale", -Duration::seconds(1)).await;
    a_state(&db, user, PROVIDER, "state-live", Duration::minutes(10)).await;

    assert!(
        !consume_link_state(&db.pool, user, PROVIDER, "state-stale")
            .await
            .expect("consume")
    );
    assert_eq!(
        prune_expired_link_states(&db.pool).await.expect("prune"),
        1,
        "the sweep removes only the expired row"
    );
    assert!(
        consume_link_state(&db.pool, user, PROVIDER, "state-live")
            .await
            .expect("consume")
    );
}
