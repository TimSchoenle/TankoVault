//! Set-based rewrites of the per-row loops the audit found (PERF-3, PERF-13, PERF-15).
//!
//! Each of these replaced an N-round-trip loop with one statement, and each carries a semantic
//! trap that only a real Postgres shows:
//!
//! - `ON CONFLICT DO UPDATE` **aborts** if one statement touches the same row twice, so every
//!   batch insert needs a `DISTINCT ON` whose tie-break reproduces what the sequential loop left
//!   behind. A wrong tie-break is not a crash; it silently persists the wrong row.
//! - `dedup_claim_many` decides whether a notification is sent at all. If it over-reports the
//!   claimed set, every rescan re-notifies; if it under-reports, a genuinely new chapter is
//!   announced to nobody.
//! - `sync_excluded_series` re-derives the §A.5 exclusion precedence that `is_sync_excluded`
//!   expresses in SQL. A divergence means a series the user opted out of gets synced anyway, so
//!   the two are asserted to agree rather than each asserted separately.
//!
//! Opt-in: gated behind the `integration` feature because it requires Docker.
#![cfg(feature = "integration")]

use tankovault_config::MatchingConfig;
use tankovault_db::repo::catalog::register_source_stubs;
use tankovault_db::repo::{sync, tracking};
use tankovault_domain::{
    AccountStatus, ContentType, ProviderId, SeriesId, SeriesStatus, WatchStatus,
};
use tankovault_test_support::{TestDb, seed};
use time::OffsetDateTime;

async fn a_series(db: &TestDb, provider_id: ProviderId, title: &str, path: &str) -> SeriesId {
    // Type and status stay `Unknown`: these tests are about batched writes, not about metadata,
    // and the builder's `Manga`/`Ongoing` defaults would be a claim this file does not make.
    seed::series(db, provider_id, title)
        .source_path(path)
        .content_type(ContentType::Unknown)
        .status(SeriesStatus::Unknown)
        .create()
        .await
}

// ---------------------------------------------------------------------------
// PERF-3 — the notifier fan-out
// ---------------------------------------------------------------------------

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

    // A rescan republishes the same event. Only the user who has never been notified may claim,
    // or every rescan re-fires the external channels.
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
    // The fan-out reaches this whenever every watcher has already read past the chapter. It must
    // not send an `UNNEST` over an empty array (or, worse, a bare statement).
    let db = TestDb::spawn().await;
    let provider = seed::provider(&db, "p1").create().await;
    let series = a_series(&db, provider, "Solo Leveling", "/s/solo").await;
    let claimed = tracking::dedup_claim_many(&db.pool, &[], series, 1.0)
        .await
        .expect("empty claim");
    assert!(claimed.is_empty());
}

#[tokio::test]
async fn notifications_create_many_writes_one_row_per_user_and_counts_group() {
    let db = TestDb::spawn().await;
    let a = db.seed_user("a", &[], AccountStatus::Active).await;
    let b = db.seed_user("b", &[], AccountStatus::Active).await;
    let payload = serde_json::json!({ "chapter_number": 4.0 });

    let created = tracking::notifications_create_many(&db.pool, &[a, b], "new_chapter", &payload)
        .await
        .expect("create notifications");
    assert_eq!(created.len(), 2);

    // Every returned id must belong to the user it was returned with — the pairing is what the
    // live push uses to address each SSE message.
    for (user, id) in &created {
        let rows = tracking::notifications_list(&db.pool, *user, 10)
            .await
            .expect("list notifications");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, *id);
        assert_eq!(rows[0].payload, payload);
    }

    let counts = tracking::notifications_unread_counts(&db.pool, &[a, b])
        .await
        .expect("unread counts");
    assert_eq!(counts.get(&a), Some(&1));
    assert_eq!(counts.get(&b), Some(&1));

    // A user with nothing unread is absent from the map rather than present as zero — the
    // grouped query cannot invent a row, and the caller has to treat a miss as 0.
    let c = db.seed_user("c", &[], AccountStatus::Active).await;
    let counts = tracking::notifications_unread_counts(&db.pool, &[c])
        .await
        .expect("unread counts");
    assert!(!counts.contains_key(&c));
}

// ---------------------------------------------------------------------------
// PERF-13 — the sync reconciliation prefetch and batch upserts
// ---------------------------------------------------------------------------

/// The prefetched exclusion set must agree with the single-series check on every combination of
/// (on watchlist?, blanket flag, per-provider override) — that is the §A.5 precedence, and a
/// divergence syncs a series the user opted out of.
#[tokio::test]
async fn sync_excluded_series_agrees_with_the_single_series_check() {
    let db = TestDb::spawn().await;
    let provider = seed::provider(&db, "p1").create().await;
    let user = db.seed_user("reader", &[], AccountStatus::Active).await;

    // Six series covering the precedence matrix.
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
            // Same action as "override-only"; the cases differ in whether the series is on the
            // watchlist at all, which is set above.
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
    // `sync_mappings` is keyed on `(series_id, provider)`, so two remote ids resolving to one
    // series is a same-row-twice conflict. The sequential loop left the *last* id in place;
    // without `DISTINCT ON … ORDER BY ord DESC` the statement would either abort or persist an
    // arbitrary one.
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
    // `start_year` and `series_id` travel as value + present-flag arrays because sqlx cannot bind
    // `Vec<Option<_>>` to a Postgres array. Getting that wrong would store 0 / the nil UUID
    // instead of NULL, which the admin console reads as a real year and a real series.
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

// ---------------------------------------------------------------------------
// PERF-15 — chunked catalogue stub registration
// ---------------------------------------------------------------------------

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

    // A re-scan must find everything known and register nothing — this is the path that used to
    // be the only cheap one, and it has to stay cheap *and* correct after the chunking change.
    let again = register_source_stubs(&db.pool, provider, &entries, &MatchingConfig::default())
        .await
        .expect("second registration");
    assert_eq!(again, 0);
}

#[tokio::test]
async fn stubs_sharing_a_normalized_title_attach_to_one_canonical_series() {
    // Canonicalisation inside a chunk must still see the series its predecessors created — that
    // is why the loop stays per-entry inside one transaction rather than being batched away.
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

    let series = tankovault_db::repo::catalog::list_series(&db.pool, None, 50)
        .await
        .expect("list series");
    assert_eq!(
        series.len(),
        1,
        "two spellings of one title must not create two canonical series"
    );
}
