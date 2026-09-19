//! The catalogue purge's claim: one live run at a time, a lease that frees a dead run's claim,
//! and a cancel the run sees at its next batch.
//!
//! Gated behind the `integration` feature (requires Docker).
#![cfg(feature = "integration")]

use tankovault_db::repo::catalog::maintenance::DeletionReport;
use tankovault_db::repo::catalog::purge::{
    PurgeStop, PurgeTarget, advance_purge, claim_purge, finish_purge, read_purge_state,
    request_purge_cancel,
};
use tankovault_test_support::TestDb;
use uuid::Uuid;

fn removed(series: i64) -> DeletionReport {
    DeletionReport {
        series,
        ..DeletionReport::default()
    }
}

#[tokio::test]
async fn a_live_claim_refuses_a_second_run_until_it_is_released() {
    let db = TestDb::spawn().await;
    let actor = Uuid::new_v4();

    let first = claim_purge(&db.pool, PurgeTarget::Everything, actor)
        .await
        .unwrap()
        .expect("an idle purge is claimable");
    assert!(
        claim_purge(&db.pool, PurgeTarget::Chapters, actor)
            .await
            .unwrap()
            .is_none()
    );

    finish_purge(&db.pool, first, removed(7), Some(0), PurgeStop::Done, None)
        .await
        .unwrap();
    let state = read_purge_state(&db.pool).await.unwrap();
    assert!(!state.running);
    assert_eq!(state.stopped, Some(PurgeStop::Done));
    assert_eq!(state.scope, Some(PurgeTarget::Everything));
    assert_eq!(state.removed.series, 7);
    assert_eq!(state.remaining, Some(0));

    assert!(
        claim_purge(&db.pool, PurgeTarget::Chapters, actor)
            .await
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn a_cancel_stops_the_run_at_its_next_advance() {
    let db = TestDb::spawn().await;
    assert!(
        !request_purge_cancel(&db.pool).await.unwrap(),
        "no run is live to cancel"
    );

    let claim = claim_purge(&db.pool, PurgeTarget::Everything, Uuid::new_v4())
        .await
        .unwrap()
        .unwrap();
    assert!(
        advance_purge(&db.pool, claim, removed(10), 90)
            .await
            .unwrap()
    );
    assert!(request_purge_cancel(&db.pool).await.unwrap());
    assert!(read_purge_state(&db.pool).await.unwrap().cancel_requested);
    assert!(
        !advance_purge(&db.pool, claim, removed(20), 80)
            .await
            .unwrap()
    );
}

/// A run killed with its process never releases the claim. Without the lease the purge could
/// never be started again; without the claim fence the dead run, if it woke, would keep writing
/// over the run that replaced it.
#[tokio::test]
async fn a_lapsed_lease_reads_interrupted_and_fences_out_the_old_run() {
    let db = TestDb::spawn().await;
    let actor = Uuid::new_v4();
    let dead = claim_purge(&db.pool, PurgeTarget::Everything, actor)
        .await
        .unwrap()
        .unwrap();
    advance_purge(&db.pool, dead, removed(5), 95).await.unwrap();
    sqlx::query("UPDATE catalogue_purge_state SET heartbeat_at = now() - interval '1 hour'")
        .execute(&db.pool)
        .await
        .unwrap();

    let state = read_purge_state(&db.pool).await.unwrap();
    assert!(!state.running);
    assert_eq!(state.stopped, Some(PurgeStop::Interrupted));
    assert_eq!(state.removed.series, 5);
    assert!(!request_purge_cancel(&db.pool).await.unwrap());

    let next = claim_purge(&db.pool, PurgeTarget::Everything, actor)
        .await
        .unwrap()
        .expect("a lapsed lease is re-granted");
    assert_ne!(next, dead);
    assert!(
        !advance_purge(&db.pool, dead, removed(50), 50)
            .await
            .unwrap()
    );
    finish_purge(
        &db.pool,
        dead,
        removed(50),
        Some(50),
        PurgeStop::Failed,
        Some("late"),
    )
    .await
    .unwrap();

    let state = read_purge_state(&db.pool).await.unwrap();
    assert!(
        state.running,
        "the old run's release must not end the new one"
    );
    assert_eq!(state.removed.series, 0);
    assert_eq!(state.error, None);
}
