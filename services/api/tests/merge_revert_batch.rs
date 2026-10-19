//! The batch merge revert unwinds a chain newest first and reports each decision on its own.
//!
//! A merge whose survivor a later merge absorbed can only be reverted after that later one. A
//! batch handled in the order given would refuse the earlier merge whenever an operator listed
//! it first, which a list sorted by series or by title does about half the time.
//!
//! Gated behind the `integration` feature (requires Docker).
#![cfg(feature = "integration")]

use axum::http::StatusCode;
use serde_json::json;
use tankovault_api_test_support::{TestApp, TestConfig};
use tankovault_db::repo::matching::{
    MergeUndo, NewMergeDecision, merge_series, record_merge_decisions,
};
use tankovault_domain::{AccountStatus, Permission, SeriesId};
use tankovault_test_support::seed;

const BATCH: &str = "/v1/admin/merge-decisions/revert-batch";

async fn journal(app: &TestApp, keep: SeriesId, drop: SeriesId, undo: &MergeUndo) -> uuid::Uuid {
    let empty = json!({});
    record_merge_decisions(
        &app.db.pool,
        &[NewMergeDecision {
            sweep_id: None,
            trigger: "sweep_new",
            actor: None,
            pair: (keep, drop),
            titles: ("keep", "drop"),
            verdict: "auto",
            reason: "structural_identity_above_threshold",
            blocked_by: &[],
            outcome: "merged",
            survivor_id: Some(keep),
            absorbed_id: Some(drop),
            score: 1.0,
            base_score: 1.0,
            signals: &["alias_identity"],
            terms: &empty,
            evidence: &empty,
            policy: &empty,
            undo: Some(undo),
        }],
    )
    .await
    .expect("journal the merge")[0]
}

#[tokio::test]
async fn a_chain_listed_oldest_first_is_reverted_newest_first() {
    let app = TestApp::spawn_with(TestConfig::new().without_rate_limiting()).await;
    let operator = app
        .seed_user(
            "reverter",
            &[Permission::MergeRevert],
            AccountStatus::Active,
        )
        .await;
    let (bearer, step_up) = (
        app.bearer(operator),
        app.enrolled_and_elevated(operator).await,
    );
    let provider = seed::provider(&app.db, "alpha").create().await;
    let a = seed::series(&app.db, provider, "Vinland Saga")
        .create()
        .await;
    let b = seed::series(&app.db, provider, "Berserk").create().await;
    let c = seed::series(&app.db, provider, "Monster").create().await;
    let a_into_b = merge_series(&app.db.pool, b, a, None, "auto_merged")
        .await
        .expect("merge a into b");
    let first = journal(&app, b, a, &a_into_b).await;
    let b_into_c = merge_series(&app.db.pool, c, b, None, "auto_merged")
        .await
        .expect("merge b into c");
    let second = journal(&app, c, b, &b_into_c).await;
    let unknown = uuid::Uuid::now_v7();

    let request = |dry_run: bool| {
        json!({
            "decision_ids": [first, second, unknown],
            "reason": "different works",
            "dry_run": dry_run,
        })
    };

    let (status, dry) = app
        .call_elevated(
            "POST",
            BATCH,
            Some(&bearer),
            Some(&step_up),
            Some(request(true)),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{dry}");
    assert_eq!(
        (dry["reverted"].as_i64(), dry["skipped"].as_i64()),
        (Some(2), Some(1)),
        "{dry}"
    );
    let live = tankovault_db::repo::catalog::maintenance::totals(&app.db.pool)
        .await
        .expect("catalogue totals")
        .series_total;
    assert_eq!(live, 1, "a dry run reverts nothing");

    let (status, run) = app
        .call_elevated(
            "POST",
            BATCH,
            Some(&bearer),
            Some(&step_up),
            Some(request(false)),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{run}");
    let order: Vec<(String, String)> = run["items"]
        .as_array()
        .expect("items")
        .iter()
        .map(|i| {
            (
                i["decision_id"].as_str().unwrap_or_default().to_owned(),
                i["outcome"].as_str().unwrap_or_default().to_owned(),
            )
        })
        .collect();
    assert_eq!(
        order,
        vec![
            (second.to_string(), "reverted".to_owned()),
            (first.to_string(), "reverted".to_owned()),
            (unknown.to_string(), "not_found".to_owned()),
        ],
        "{run}"
    );
    let live = tankovault_db::repo::catalog::maintenance::totals(&app.db.pool)
        .await
        .expect("catalogue totals")
        .series_total;
    assert_eq!(live, 3);
}
