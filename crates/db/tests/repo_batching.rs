//! Set-based rewrites of per-row loops, tested against a real Postgres for semantics a type
//! check can't catch — e.g. `ON CONFLICT DO UPDATE` aborting when one statement touches a row
//! twice.
//!
//! Gated behind the `integration` feature (requires Docker).
#![cfg(feature = "integration")]

use tankovault_config::MatchingConfig;
use tankovault_db::repo::catalog::register_source_stubs;
use tankovault_db::repo::tracking::NotificationFilter;
use tankovault_db::repo::{sync, tracking};
use tankovault_domain::{
    AccountStatus, ContentType, ProviderId, SeriesId, SeriesStatus, WatchStatus,
};
use tankovault_test_support::{TestDb, seed};
use time::OffsetDateTime;

async fn a_series(db: &TestDb, provider_id: ProviderId, title: &str, path: &str) -> SeriesId {
    // Type/status stay `Unknown`; these tests are about batched writes, not metadata.
    seed::series(db, provider_id, title)
        .source_path(path)
        .content_type(ContentType::Unknown)
        .status(SeriesStatus::Unknown)
        .create()
        .await
}

// Notifier fan-out

#[tokio::test]
async fn dedup_claim_many_returns_only_the_users_that_actually_claimed() {
    let db = TestDb::spawn().await;
    let provider = seed::provider(&db, "p1").create().await;
    let series = a_series(&db, provider, "Solo Leveling", "/s/solo").await;
    let a = db.seed_user("a", &[], AccountStatus::Active).await;
    let b = db.seed_user("b", &[], AccountStatus::Active).await;
    let c = db.seed_user("c", &[], AccountStatus::Active).await;

    let first = tracking::dedup_claim_many(&db.pool, &[a, b], series, 12.0)
        .await
        .expect("first claim");
    let mut first_sorted = first.clone();
    first_sorted.sort_by_key(|u| u.as_uuid());
    let mut expected = vec![a, b];
    expected.sort_by_key(|u| u.as_uuid());
    assert_eq!(first_sorted, expected, "both users are new to this chapter");

    // A rescan must not re-claim an already-notified user.
    let second = tracking::dedup_claim_many(&db.pool, &[a, b, c], series, 12.0)
        .await
        .expect("second claim");
    assert_eq!(second, vec![c], "already-notified users must not re-claim");

    // A different chapter is a different slot.
    let other = tracking::dedup_claim_many(&db.pool, &[a], series, 13.0)
        .await
        .expect("other chapter");
    assert_eq!(other, vec![a]);
}

#[tokio::test]
async fn dedup_claim_many_on_an_empty_list_is_a_no_op() {
    // Must not send `UNNEST` over an empty array.
    let db = TestDb::spawn().await;
    let provider = seed::provider(&db, "p1").create().await;
    let series = a_series(&db, provider, "Solo Leveling", "/s/solo").await;
    let claimed = tracking::dedup_claim_many(&db.pool, &[], series, 1.0)
        .await
        .expect("empty claim");
    assert!(claimed.is_empty());
}

#[tokio::test]
async fn notifications_upsert_many_writes_one_row_per_user_and_counts_group() {
    let db = TestDb::spawn().await;
    let a = db.seed_user("a", &[], AccountStatus::Active).await;
    let b = db.seed_user("b", &[], AccountStatus::Active).await;
    let payload = serde_json::json!({ "chapter_number": 4.0 });

    let created =
        tracking::notifications_upsert_many(&db.pool, &[a, b], "new_chapter", None, &payload)
            .await
            .expect("create notifications");
    assert_eq!(created.len(), 2);

    // The returned id must belong to its paired user (SSE addressing depends on it).
    for entry in &created {
        let page = tracking::notifications_page(
            &db.pool,
            entry.user_id,
            NotificationFilter::default(),
            10,
            0,
        )
        .await
        .expect("list notifications");
        assert_eq!(page.items.len(), 1);
        assert_eq!(page.items[0].id, entry.notification_id);
        assert_eq!(page.items[0].payload, payload);
    }

    let counts = tracking::notifications_unread_counts(&db.pool, &[a, b])
        .await
        .expect("unread counts");
    assert_eq!(counts.get(&a), Some(&1));
    assert_eq!(counts.get(&b), Some(&1));

    // Absent from the map, not zero — `GROUP BY` can't invent a row.
    let c = db.seed_user("c", &[], AccountStatus::Active).await;
    let counts = tracking::notifications_unread_counts(&db.pool, &[c])
        .await
        .expect("unread counts");
    assert!(!counts.contains_key(&c));
}

/// A group key coalesces into the reader's *open* row; a read one starts a fresh row.
///
/// Twelve chapters overnight used to be twelve rows and a bell reading `12`, none of which said
/// which series they were about. The merge is what makes the count mean "things to look at": it
/// sums `count`, widens `first_number`/`last_number`, and keeps `latest` on whichever side is
/// further along, so an event arriving out of order cannot walk the row backwards.
#[tokio::test]
async fn a_group_key_coalesces_into_the_open_row_only() {
    let db = TestDb::spawn().await;
    let reader = db.seed_user("grouped", &[], AccountStatus::Active).await;
    let group = "series:test";

    let event = |number: f64, title: &str| {
        serde_json::json!({
            "v": 2,
            "series_title": "Blame!",
            "count": 1,
            "first_number": number,
            "last_number": number,
            "latest": { "number": number, "title": title },
        })
    };

    for (number, title) in [(7.0, "seven"), (9.0, "nine"), (8.0, "eight")] {
        tracking::notifications_upsert_many(
            &db.pool,
            &[reader],
            "new_chapter",
            Some(group),
            &event(number, title),
        )
        .await
        .expect("upsert");
    }

    let page = tracking::notifications_page(&db.pool, reader, NotificationFilter::default(), 10, 0)
        .await
        .expect("list");
    assert_eq!(page.items.len(), 1, "three chapters coalesced into one row");
    assert_eq!(page.unread, 1, "and into one unread count");
    let merged = &page.items[0].payload;
    assert_eq!(merged["count"], serde_json::json!(3));
    // Read as `f64`, not compared against a JSON literal: a whole `float8` round-trips through
    // `jsonb` as `7`, not `7.0`, and the API reads these with `as_f64` for exactly that reason.
    assert_eq!(merged["first_number"].as_f64(), Some(7.0));
    assert_eq!(merged["last_number"].as_f64(), Some(9.0));
    assert_eq!(
        merged["latest"]["title"],
        serde_json::json!("nine"),
        "the out-of-order chapter 8 must not overwrite the newer chapter 9"
    );

    tracking::notifications_mark_all_read(&db.pool, reader)
        .await
        .expect("mark read");
    tracking::notifications_upsert_many(
        &db.pool,
        &[reader],
        "new_chapter",
        Some(group),
        &event(10.0, "ten"),
    )
    .await
    .expect("upsert after read");

    let page = tracking::notifications_page(&db.pool, reader, NotificationFilter::default(), 10, 0)
        .await
        .expect("list");
    assert_eq!(page.items.len(), 2, "a read row is not reopened");
    assert_eq!(page.items[0].payload["count"], serde_json::json!(1));
}

/// The window is a window, and the two totals count the inbox behind it.
///
/// `GET /v1/me/notifications` used to answer with a single hard-capped batch of 100 rows and no
/// counts at all, so a reader with more than that saw exactly 100 rows, a notification bell stuck
/// at 100, and no way to reach the rest. `total` and `unread` must therefore be counted from the
/// table rather than from the page — deriving them from `items` reproduces the cap exactly.
///
/// The filter is server-side for the same reason: the tabs used to filter the one page the client
/// had loaded, so "Unread" showed nothing whenever the unread rows sat on page two.
#[tokio::test]
async fn notifications_page_windows_the_inbox_and_counts_all_of_it() {
    let db = TestDb::spawn().await;
    let reader = db.seed_user("reader", &[], AccountStatus::Active).await;
    let other = db.seed_user("other", &[], AccountStatus::Active).await;

    for n in 0..5 {
        let payload = serde_json::json!({ "chapter_number": f64::from(n) });
        tracking::notifications_upsert_many(
            &db.pool,
            &[reader, other],
            "new_chapter",
            None,
            &payload,
        )
        .await
        .expect("create notifications");
    }

    let mut seen = std::collections::HashSet::new();
    for offset in [0, 2, 4] {
        let page = tracking::notifications_page(
            &db.pool,
            reader,
            NotificationFilter::default(),
            2,
            offset,
        )
        .await
        .expect("page notifications");
        // Both counts describe the inbox, so they do not move as the window does.
        assert_eq!(page.total, 5);
        assert_eq!(page.unread, 5);
        assert_eq!(page.items.len(), if offset == 4 { 1 } else { 2 });
        seen.extend(page.items.iter().map(|n| n.id));
    }
    // Walking the offsets reaches every row exactly once — the point of paging over truncating.
    assert_eq!(seen.len(), 5);

    let filtered = tracking::notifications_page(
        &db.pool,
        reader,
        NotificationFilter {
            unread_only: false,
            kind: Some("series_completed"),
        },
        10,
        0,
    )
    .await
    .expect("page notifications");
    assert!(filtered.items.is_empty());
    assert_eq!(
        filtered.total, 0,
        "`total` follows the filter — it is the pager's denominator"
    );
    assert_eq!(filtered.unread, 5, "`unread` does not — it is the bell");

    let marked = tracking::notifications_mark_all_read(&db.pool, reader)
        .await
        .expect("mark all read");
    assert_eq!(marked, 5);

    let page = tracking::notifications_page(&db.pool, reader, NotificationFilter::default(), 2, 0)
        .await
        .expect("page notifications");
    assert_eq!(page.unread, 0);
    assert_eq!(page.total, 5);

    let unread_only = tracking::notifications_page(
        &db.pool,
        reader,
        NotificationFilter {
            unread_only: true,
            kind: None,
        },
        10,
        0,
    )
    .await
    .expect("page notifications");
    assert!(unread_only.items.is_empty());
    assert_eq!(unread_only.total, 0);

    // Scoped to its owner: "mark all read" must not reach across accounts.
    let theirs = tracking::notifications_page(&db.pool, other, NotificationFilter::default(), 2, 0)
        .await
        .expect("page notifications");
    assert_eq!(theirs.unread, 5);
}

// Sync reconciliation prefetch and batch upserts

/// The prefetched exclusion set must agree with the single-series check on every combination of
/// watchlist membership, blanket flag and per-provider override.
#[tokio::test]
async fn sync_excluded_series_agrees_with_the_single_series_check() {
    let db = TestDb::spawn().await;
    let provider = seed::provider(&db, "p1").create().await;
    let user = db.seed_user("reader", &[], AccountStatus::Active).await;

    let mut cases: Vec<(SeriesId, &str)> = Vec::new();
    for (i, label) in [
        "not-on-watchlist",
        "watchlisted-included",
        "watchlisted-excluded",
        "override-excludes",
        "override-includes-over-blanket",
        "override-only",
    ]
    .into_iter()
    .enumerate()
    {
        let series = a_series(&db, provider, &format!("Series {i}"), &format!("/s/{i}")).await;
        cases.push((series, label));
    }

    for (series, label) in &cases {
        match *label {
            "not-on-watchlist" | "override-only" => {}
            _ => {
                tracking::watchlist_upsert(&db.pool, user, *series, WatchStatus::Reading, true)
                    .await
                    .expect("watchlist");
            }
        }
        match *label {
            "watchlisted-excluded" => {
                tracking::set_sync_excluded(&db.pool, user, *series, true)
                    .await
                    .expect("blanket exclude");
            }
            // Same action as "override-only"; differs only in watchlist membership, set above.
            "override-excludes" | "override-only" => {
                tracking::set_sync_override(&db.pool, user, *series, "p1", true)
                    .await
                    .expect("override exclude");
            }
            "override-includes-over-blanket" => {
                tracking::set_sync_excluded(&db.pool, user, *series, true)
                    .await
                    .expect("blanket exclude");
                tracking::set_sync_override(&db.pool, user, *series, "p1", false)
                    .await
                    .expect("override include");
            }
            _ => {}
        }
    }

    let batched = tracking::sync_excluded_series(&db.pool, user, "p1")
        .await
        .expect("batched exclusions");

    for (series, label) in &cases {
        let single = tracking::is_sync_excluded(&db.pool, user, *series, "p1")
            .await
            .expect("single check");
        assert_eq!(
            batched.contains(series),
            single,
            "prefetched set disagrees with is_sync_excluded for {label}"
        );
    }

    // A provider the override does not name must not inherit it.
    let other = tracking::sync_excluded_series(&db.pool, user, "p2")
        .await
        .expect("other provider");
    let (override_only, _) = cases
        .iter()
        .find(|(_, l)| *l == "override-only")
        .expect("case present");
    assert!(
        !other.contains(override_only),
        "a per-provider override must not leak to another provider"
    );
}

#[tokio::test]
async fn progress_and_status_prefetches_match_the_per_series_reads() {
    let db = TestDb::spawn().await;
    let provider = seed::provider(&db, "p1").create().await;
    let user = db.seed_user("reader", &[], AccountStatus::Active).await;
    let one = a_series(&db, provider, "One", "/s/one").await;
    let two = a_series(&db, provider, "Two", "/s/two").await;

    tracking::watchlist_upsert(&db.pool, user, one, WatchStatus::Reading, true)
        .await
        .expect("watchlist one");
    tracking::progress_set(&db.pool, user, one, 17.0)
        .await
        .expect("progress one");
    tracking::watchlist_upsert(&db.pool, user, two, WatchStatus::Paused, true)
        .await
        .expect("watchlist two");

    let progress = tracking::progress_states_for_user(&db.pool, user)
        .await
        .expect("progress prefetch");
    let statuses = tracking::watchlist_statuses_for_user(&db.pool, user)
        .await
        .expect("status prefetch");

    assert_eq!(progress.get(&one).map(|(p, _)| *p), Some(17.0));
    assert!(
        !progress.contains_key(&two),
        "a series with no read_progress row must be absent, not zero"
    );
    assert_eq!(statuses.get(&one), Some(&WatchStatus::Reading));
    assert_eq!(statuses.get(&two), Some(&WatchStatus::Paused));
}

#[tokio::test]
async fn upsert_mappings_lets_the_last_external_id_win() {
    // Two remote ids resolving to one `(series_id, provider)` row is a same-row-twice conflict;
    // `DISTINCT ON … ORDER BY ord DESC` must keep the last one.
    let db = TestDb::spawn().await;
    let provider = seed::provider(&db, "p1").create().await;
    let series = a_series(&db, provider, "Solo Leveling", "/s/solo").await;

    sync::upsert_mappings(
        &db.pool,
        "fake",
        &[
            (series, "first".to_owned()),
            (series, "second".to_owned()),
            (series, "third".to_owned()),
        ],
    )
    .await
    .expect("batched mappings");

    assert_eq!(
        sync::mapping_external_for_series(&db.pool, series, "fake")
            .await
            .expect("read mapping"),
        Some("third".to_owned())
    );
}

#[tokio::test]
async fn upsert_remote_entries_persists_nullable_columns_and_survives_a_duplicate_id() {
    // Nullable columns travel as value + present-flag arrays (sqlx can't bind `Vec<Option<_>>`);
    // getting this wrong stores 0 / the nil UUID instead of NULL.
    let db = TestDb::spawn().await;
    let provider = seed::provider(&db, "p1").create().await;
    let series = a_series(&db, provider, "Solo Leveling", "/s/solo").await;
    let user = db.seed_user("reader", &[], AccountStatus::Active).await;
    let updated = OffsetDateTime::from_unix_timestamp(1_600_000_000).expect("timestamp");

    let entry = |external_id: &str, year, series_id| sync::FetchedRemoteEntry {
        external_id: external_id.to_owned(),
        title: format!("title-{external_id}"),
        status: "reading".to_owned(),
        progress: 3.0,
        content_type: "unknown".to_owned(),
        start_year: year,
        updated_at: updated,
        series_id,
    };

    sync::upsert_remote_entries(
        &db.pool,
        user,
        "fake",
        &[
            entry("matched", Some(2018), Some(series)),
            entry("unmatched", None, None),
            // A duplicate id in one statement must not abort the batch.
            entry("unmatched", None, None),
        ],
    )
    .await
    .expect("batched remote entries");

    let matched = sync::get_remote_entry(&db.pool, user, "fake", "matched")
        .await
        .expect("read matched")
        .expect("matched present");
    assert_eq!(matched.title, "title-matched");

    let unmatched = sync::admin_list_unmatched_remote(&db.pool, "fake", None, 10)
        .await
        .expect("list unmatched");
    assert_eq!(
        unmatched.len(),
        1,
        "the unmatched entry is stored once, with a NULL series_id"
    );
    assert!(
        unmatched[0].start_year.is_none(),
        "an absent start_year must persist as NULL, not 0"
    );
}

// Chunked catalogue stub registration

#[tokio::test]
async fn register_source_stubs_registers_once_and_is_idempotent() {
    let db = TestDb::spawn().await;
    let provider = seed::provider(&db, "p1").create().await;

    let entries = [
        ("/s/a", "Alpha"),
        ("/s/b", "Beta"),
        ("/s/c", "Gamma"),
        // The same path twice on one page: the batch insert must not abort.
        ("/s/c", "Gamma"),
    ];
    let registered =
        register_source_stubs(&db.pool, provider, &entries, &MatchingConfig::default())
            .await
            .expect("first registration");
    assert_eq!(registered, 4, "every fresh entry is reported registered");

    // A re-scan must register nothing.
    let again = register_source_stubs(&db.pool, provider, &entries, &MatchingConfig::default())
        .await
        .expect("second registration");
    assert_eq!(again, 0);
}

#[tokio::test]
async fn stubs_sharing_a_normalized_title_attach_to_one_canonical_series() {
    // Canonicalisation inside a chunk must see series its predecessors just created.
    let db = TestDb::spawn().await;
    let provider = seed::provider(&db, "p1").create().await;

    register_source_stubs(
        &db.pool,
        provider,
        &[("/s/one", "Solo Leveling"), ("/s/two", "solo leveling!")],
        &MatchingConfig::default(),
    )
    .await
    .expect("registration");

    let series = tankovault_db::repo::catalog::list_series(&db.pool, None, false, 50)
        .await
        .expect("list series");
    assert_eq!(
        series.len(),
        1,
        "two spellings of one title must not create two canonical series"
    );
}
