//! The recommendation build's claim (`crates/db/src/repo/recsys/build.rs`).
//!
//! Gated behind the `integration` feature (requires Docker).
#![cfg(feature = "integration")]

use tankovault_db::repo::recsys;
use tankovault_test_support::TestDb;

/// Long enough that nothing in a test expires on its own; every expiry here is forced.
const LEASE: f64 = 3_600.0;

async fn stage_now(db: &TestDb) -> String {
    sqlx::query_scalar("SELECT stage FROM rec_build_state WHERE id")
        .fetch_one(&db.pool)
        .await
        .expect("read stage")
}

/// Age the current claim's heartbeat past any plausible lease.
async fn expire_the_lease(db: &TestDb) {
    sqlx::query("UPDATE rec_build_state SET heartbeat_at = now() - interval '1 day' WHERE id")
        .execute(&db.pool)
        .await
        .expect("expire the lease");
}

/// **A build that dies without releasing its claim must not block every later one.**
///
/// The bug: the claim was `stage <> 'idle'` and nothing but `finish_build` ever cleared it. The
/// control plane awaited a full build inside its `/internal/recsys-build` handler, the 30-second
/// `TimeoutLayer` dropped that future mid-extraction, and the release never ran. Production sat
/// on `full:features, 0 series` for six hours: every scheduled run afterwards was refused the
/// claim and logged it at debug, so the recommender stopped updating and reported nothing wrong.
///
/// The lease is what makes that self-healing. A claim is held only while something is alive to
/// keep stamping it.
#[tokio::test]
async fn a_claim_whose_heartbeat_went_stale_is_reclaimed() {
    let db = TestDb::spawn().await;

    let first = recsys::start_build(&db.pool, true, LEASE)
        .await
        .expect("claim the build")
        .expect("an idle build is claimable");

    assert!(
        recsys::start_build(&db.pool, true, LEASE)
            .await
            .expect("second claim")
            .is_none(),
        "a live claim is still the mutual exclusion"
    );

    expire_the_lease(&db).await;

    let second = recsys::start_build(&db.pool, true, LEASE)
        .await
        .expect("reclaim the build")
        .expect("an expired lease is breakable");
    assert_ne!(second.claim_id, first.claim_id);
    assert_eq!(
        second.generation,
        first.generation + 1,
        "a full build takes the next generation, reclaim or not"
    );
}

/// **A claim taken before the lease column existed is breakable.**
///
/// The row this migration lands on may already be wedged — that is the reason the migration was
/// written — and it carries no heartbeat to expire. A NULL therefore has to count as expired, or
/// the deployment that ships the fix leaves the stuck build stuck.
#[tokio::test]
async fn a_claim_with_no_heartbeat_at_all_is_reclaimed() {
    let db = TestDb::spawn().await;

    recsys::start_build(&db.pool, true, LEASE)
        .await
        .expect("claim the build")
        .expect("an idle build is claimable");
    sqlx::query("UPDATE rec_build_state SET heartbeat_at = NULL WHERE id")
        .execute(&db.pool)
        .await
        .expect("strip the heartbeat");

    assert!(
        recsys::start_build(&db.pool, true, LEASE)
            .await
            .expect("reclaim the build")
            .is_some(),
        "a claim with no heartbeat predates the lease and must not hold forever"
    );
}

/// **A heartbeat holds the claim, and only its own.**
///
/// The first half is what keeps a long build from having its claim stolen mid-run. The second is
/// what stops a superseded build from keeping a claim alive that it no longer owns — without the
/// `claim_id` fence, the zombie's heartbeat would extend the *new* holder's lease and the two
/// would take turns writing under different generations indefinitely.
#[tokio::test]
async fn only_the_current_holder_can_stamp_the_lease() {
    let db = TestDb::spawn().await;

    let first = recsys::start_build(&db.pool, true, LEASE)
        .await
        .expect("claim the build")
        .expect("an idle build is claimable");

    expire_the_lease(&db).await;
    recsys::touch_build(&db.pool, first)
        .await
        .expect("heartbeat");
    assert!(
        recsys::start_build(&db.pool, true, LEASE)
            .await
            .expect("second claim")
            .is_none(),
        "a stamped lease is a live claim again"
    );

    expire_the_lease(&db).await;
    let second = recsys::start_build(&db.pool, true, LEASE)
        .await
        .expect("reclaim the build")
        .expect("an expired lease is breakable");

    expire_the_lease(&db).await;
    recsys::touch_build(&db.pool, first)
        .await
        .expect("zombie heartbeat");
    assert!(
        recsys::start_build(&db.pool, true, LEASE)
            .await
            .expect("third claim")
            .is_some(),
        "the superseded build's heartbeat must not hold the claim it lost"
    );
    assert_ne!(second.claim_id, first.claim_id);
}

/// **A superseded build cannot write over the run that replaced it.**
///
/// Breaking a lease makes two builds briefly concurrent: a cancelled request handler is dropped
/// but a spawned task is not, and a partitioned replica still believes it owns the build. Without
/// the `claim_id` fence the zombie's progress writes would land on the new run — the console
/// showing one run's stage against another's count — and its `finish_build` would release a claim
/// it does not hold, freeing the state of a build still in progress for a *third* one to claim.
#[tokio::test]
async fn a_superseded_build_cannot_advance_or_release_the_new_claim() {
    let db = TestDb::spawn().await;

    let zombie = recsys::start_build(&db.pool, true, LEASE)
        .await
        .expect("claim the build")
        .expect("an idle build is claimable");
    expire_the_lease(&db).await;
    let holder = recsys::start_build(&db.pool, true, LEASE)
        .await
        .expect("reclaim the build")
        .expect("an expired lease is breakable");

    recsys::update_build_stage(&db.pool, zombie, "full:priors", 99, 99)
        .await
        .expect("zombie progress write");
    assert_eq!(
        stage_now(&db).await,
        "full:features",
        "the zombie's stage must not land on the run that replaced it"
    );

    recsys::finish_build(&db.pool, zombie, 0, 0, 0, Some("zombie"))
        .await
        .expect("zombie release");
    assert_eq!(
        stage_now(&db).await,
        "full:features",
        "the zombie must not release a claim it no longer holds"
    );

    recsys::finish_build(&db.pool, holder, 1, 1, 1, None)
        .await
        .expect("release");
    assert_eq!(stage_now(&db).await, "idle", "the holder releases its own");
}
