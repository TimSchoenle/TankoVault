//! The admin Sync console read models (`crates/db/src/repo/sync/admin_views.rs`, TEST F-05).
//!
//! Eight statements that had no test at any level. They are all *reads*, so a wrong answer
//! here is never an error — it is an operator queue that quietly omits work, a suggestion list
//! ordered by the wrong column, or one user's data appearing under another's name. Each test
//! below is written so that dropping the scoping predicate it names fails it and nothing else.
//!
//! The one that found a defect is [`pending_conflicts_is_scoped_to_the_account_not_the_user`]:
//! the count subquery joined on `user_id` alone while the row it decorates is keyed by
//! `(user, provider)`, so a second linked provider made both rows report the union.
//!
//! Opt-in: gated behind the `integration` feature because it requires Docker.
#![cfg(feature = "integration")]

use tankovault_config::MatchingConfig;
use tankovault_db::repo::catalog::{ScannedSeries, SeriesUpsert, ingest_series};
use tankovault_db::repo::providers::{self, NewProvider};
use tankovault_db::repo::sync::{
    self, FetchedRemoteEntry, NewConflict, admin_list_accounts, admin_list_mappings,
    admin_list_mappings_for_series, admin_list_unmapped, admin_list_unmatched_remote,
    get_remote_entry, mark_remote_entry_matched, suggest_series_candidates,
};
use tankovault_domain::{
    AccountStatus, AdapterKind, ContentType, Politeness, ProviderId, SeriesId, SeriesStatus,
    UserId, normalize_title,
};
use tankovault_test_support::TestDb;
use time::OffsetDateTime;
use time::ext::NumericalDuration as _;

// ---------------------------------------------------------------------------
// Fixture
// ---------------------------------------------------------------------------

async fn a_provider(db: &TestDb, slug: &str) -> ProviderId {
    providers::create(
        &db.pool,
        NewProvider {
            slug: slug.to_owned(),
            name: slug.to_owned(),
            base_url: format!("https://{slug}.invalid"),
            adapter: AdapterKind::GenericConfig,
            config: serde_json::json!({}),
            politeness: Politeness::default(),
        },
    )
    .await
    .expect("create provider")
    .id
}

/// Ingest one canonical series with `sources` distinct local providers behind it, so the
/// assign queue's `source_count` ordering has something to order by.
async fn a_series(db: &TestDb, title: &str, sources: usize, alt_titles: &[&str]) -> SeriesId {
    let mut series_id = None;
    for n in 0..sources {
        let provider = a_provider(
            db,
            &format!("{}-{n}", normalize_title(title).replace(' ', "")),
        )
        .await;
        let outcome = ingest_series(
            &db.pool,
            &ScannedSeries {
                provider_id: provider,
                source_path: format!("/s/{n}"),
                provider_title: Some(title.to_owned()),
                meta: SeriesUpsert {
                    canonical_title: title.to_owned(),
                    normalized_title: normalize_title(title),
                    description: None,
                    cover_url: None,
                    content_type: ContentType::Manga,
                    status: SeriesStatus::Ongoing,
                    release_year: Some(2000),
                },
                alt_titles: alt_titles
                    .iter()
                    .map(|t| ((*t).to_owned(), normalize_title(t)))
                    .collect(),
                tags: Vec::new(),
                authors: Vec::new(),
                chapters: Vec::new(),
                content_hash: vec![1],
            },
            &MatchingConfig::default(),
        )
        .await
        .expect("ingest series");
        series_id = Some(outcome.series_id);
    }
    series_id.expect("at least one source")
}

/// Link `user` to `provider`. The token bytes are opaque here — nothing in these read models
/// decrypts them.
async fn link(db: &TestDb, user: UserId, provider: &str) {
    sync::upsert_account(&db.pool, user, provider, b"token", None, None)
        .await
        .expect("link account");
}

fn a_remote_entry(
    external_id: &str,
    title: &str,
    series_id: Option<SeriesId>,
) -> FetchedRemoteEntry {
    FetchedRemoteEntry {
        external_id: external_id.to_owned(),
        title: title.to_owned(),
        status: "reading".to_owned(),
        progress: 12.0,
        content_type: "manga".to_owned(),
        start_year: Some(1989),
        updated_at: OffsetDateTime::UNIX_EPOCH,
        series_id,
    }
}

// ---------------------------------------------------------------------------
// Linked accounts
// ---------------------------------------------------------------------------

/// Failing accounts sort to the top, then the most recently synced, then the never-synced.
///
/// This ordering is the whole point of the table: it is an operator's work queue, and
/// `NULLS LAST` is what keeps an account that has never synced from displacing one that is
/// actively failing. Dropping either clause still renders a table, just not a useful one.
#[tokio::test]
async fn admin_list_accounts_surfaces_failures_first_then_recency() {
    let db = TestDb::spawn().await;
    let alice = db.seed_user("alice", &[], AccountStatus::Active).await;
    let bob = db.seed_user("bob", &[], AccountStatus::Active).await;
    let now = OffsetDateTime::now_utc();

    link(&db, alice, "anilist").await;
    link(&db, alice, "mal").await;
    link(&db, bob, "anilist").await;
    link(&db, bob, "mal").await;

    sync::mark_synced(
        &db.pool,
        alice,
        "anilist",
        Some("alice-remote"),
        now - 1.days(),
    )
    .await
    .expect("mark synced");
    sync::mark_synced(&db.pool, bob, "anilist", None, now)
        .await
        .expect("mark synced");
    // Synced longest ago *and* failing: the error must outrank the recency.
    sync::mark_synced(&db.pool, bob, "mal", None, now - 10.days())
        .await
        .expect("mark synced");
    sync::record_sync_error(&db.pool, bob, "mal", "token expired")
        .await
        .expect("record error");
    // alice/mal is never synced: `last_synced_at` stays NULL and it sorts last.

    let rows = admin_list_accounts(&db.pool, 10).await.expect("list");
    assert_eq!(
        rows.iter()
            .map(|r| (r.username.as_str(), r.provider.as_str()))
            .collect::<Vec<_>>(),
        vec![
            ("bob", "mal"),
            ("bob", "anilist"),
            ("alice", "anilist"),
            ("alice", "mal"),
        ]
    );
    assert_eq!(rows[0].last_error.as_deref(), Some("token expired"));
    assert_eq!(rows[2].external_username.as_deref(), Some("alice-remote"));
    assert!(rows[3].last_synced_at.is_none());

    // The limit truncates the head of that order, not an arbitrary slice of it.
    let capped = admin_list_accounts(&db.pool, 2).await.expect("list capped");
    assert_eq!(capped.len(), 2);
    assert_eq!(capped[0].provider, "mal");
}

/// The pending-conflict count belongs to the account row, not to the user.
///
/// The bug this pins: the correlated subquery matched on `sc.user_id = ea.user_id` only, while
/// the row it decorates is one per `(user, provider)`. A user with two linked providers saw
/// each provider's row claim the other's conflicts — and since the console offers a per-row
/// "resolve" affordance, an operator would have been sent to the wrong provider's queue.
/// Invisible today only because one provider ships.
#[tokio::test]
async fn pending_conflicts_is_scoped_to_the_account_not_the_user() {
    let db = TestDb::spawn().await;
    let alice = db.seed_user("alice", &[], AccountStatus::Active).await;
    let berserk = a_series(&db, "Berserk", 1, &[]).await;
    let vinland = a_series(&db, "Vinland Saga", 1, &[]).await;

    link(&db, alice, "anilist").await;
    link(&db, alice, "mal").await;

    for (series, provider, field) in [
        (berserk, "anilist", "progress"),
        (vinland, "anilist", "progress"),
        (berserk, "mal", "status"),
    ] {
        sync::insert_conflict(
            &db.pool,
            &NewConflict {
                user_id: alice,
                series_id: series,
                provider,
                field,
                local_value: "10",
                remote_value: "12",
            },
        )
        .await
        .expect("insert conflict");
    }

    // A resolved conflict is not pending and must leave the badge alone.
    sync::insert_conflict(
        &db.pool,
        &NewConflict {
            user_id: alice,
            series_id: vinland,
            provider: "anilist",
            field: "status",
            local_value: "reading",
            remote_value: "completed",
        },
    )
    .await
    .expect("insert conflict");
    let pending = sync::list_pending_conflicts(&db.pool, alice)
        .await
        .expect("list pending");
    let resolvable = pending
        .iter()
        .find(|c| c.field == "status" && c.provider == "anilist")
        .expect("the status conflict is pending");
    assert!(
        sync::resolve_conflict(&db.pool, alice, resolvable.id, "local")
            .await
            .expect("resolve")
    );

    let rows = admin_list_accounts(&db.pool, 10).await.expect("list");
    let count_for = |provider: &str| {
        rows.iter()
            .find(|r| r.provider == provider)
            .map(|r| r.pending_conflicts)
            .expect("account row")
    };
    assert_eq!(count_for("anilist"), 2, "two pending, one resolved");
    assert_eq!(count_for("mal"), 1, "mal must not inherit anilist's queue");

    // The user-wide count is a different question and still answers 3.
    assert_eq!(
        sync::count_pending_conflicts(&db.pool, alice)
            .await
            .expect("user-wide count"),
        3
    );
}

/// The policy columns are read straight through, so the console cannot show a stale default
/// after a user changes their settings.
#[tokio::test]
async fn admin_list_accounts_reports_the_users_current_sync_policy() {
    let db = TestDb::spawn().await;
    let alice = db.seed_user("alice", &[], AccountStatus::Active).await;
    link(&db, alice, "anilist").await;

    // The column defaults (`0014_progress_sync_v2.sql`): linking opts a user in, and
    // disagreements are decided by whichever side moved last unless they say otherwise.
    let before = admin_list_accounts(&db.pool, 10).await.expect("list");
    assert!(before[0].auto_sync_enabled);
    assert_eq!(before[0].conflict_policy, "newest_wins");

    sync::update_account_settings(&db.pool, alice, "anilist", Some(false), Some("ask_me"))
        .await
        .expect("update settings");

    let after = admin_list_accounts(&db.pool, 10).await.expect("list");
    assert!(!after[0].auto_sync_enabled);
    assert_eq!(after[0].conflict_policy, "ask_me");
}

// ---------------------------------------------------------------------------
// Mappings
// ---------------------------------------------------------------------------

/// The global mapping table joins the canonical title and orders by recency; the per-series
/// one returns every provider for one series, ordered by provider.
///
/// Two statements, one row type, different `WHERE`/`ORDER BY`. Asserting them together is what
/// makes it obvious if one starts answering the other's question.
#[tokio::test]
async fn the_two_mapping_views_answer_their_own_questions() {
    let db = TestDb::spawn().await;
    let berserk = a_series(&db, "Berserk", 1, &[]).await;
    let vinland = a_series(&db, "Vinland Saga", 1, &[]).await;

    sync::upsert_mapping(&db.pool, berserk, "mal", "b-mal")
        .await
        .expect("map");
    sync::upsert_mapping(&db.pool, berserk, "anilist", "b-anilist")
        .await
        .expect("map");
    sync::upsert_mapping(&db.pool, vinland, "anilist", "v-anilist")
        .await
        .expect("map");

    let global = admin_list_mappings(&db.pool, 10).await.expect("mappings");
    assert_eq!(
        global
            .iter()
            .map(|m| (m.series_title.as_str(), m.external_id.as_str()))
            .collect::<Vec<_>>(),
        vec![
            ("Vinland Saga", "v-anilist"),
            ("Berserk", "b-anilist"),
            ("Berserk", "b-mal"),
        ],
        "most recently written first"
    );

    let for_series = admin_list_mappings_for_series(&db.pool, berserk)
        .await
        .expect("per-series mappings");
    assert_eq!(
        for_series
            .iter()
            .map(|m| (m.provider.as_str(), m.external_id.as_str()))
            .collect::<Vec<_>>(),
        vec![("anilist", "b-anilist"), ("mal", "b-mal")],
        "one row per provider, ordered by provider"
    );
    assert!(for_series.iter().all(|m| m.series_id == berserk.as_uuid()));

    // Re-mapping replaces in place and bumps the row to the head of the global list.
    sync::upsert_mapping(&db.pool, berserk, "mal", "b-mal-2")
        .await
        .expect("remap");
    let after = admin_list_mappings(&db.pool, 10).await.expect("mappings");
    assert_eq!(after.len(), 3, "a re-map replaces rather than appends");
    assert_eq!(after[0].external_id, "b-mal-2");
}

// ---------------------------------------------------------------------------
// The assign queue
// ---------------------------------------------------------------------------

/// A series missing a mapping for *this* provider is in the queue even if it is mapped
/// elsewhere, richest first.
///
/// `NOT EXISTS (… AND sm.provider = $1)` is the whole predicate. Without the provider term,
/// mapping a series at `AniList` would also clear it from every other provider's queue —
/// silently dropping work that has to be done per provider.
#[tokio::test]
async fn the_assign_queue_is_per_provider_and_richest_first() {
    let db = TestDb::spawn().await;
    let berserk = a_series(&db, "Berserk", 3, &[]).await;
    let vinland = a_series(&db, "Vinland Saga", 1, &[]).await;
    let _frieren = a_series(&db, "Frieren", 1, &[]).await;

    sync::upsert_mapping(&db.pool, berserk, "mal", "b-mal")
        .await
        .expect("map");
    sync::upsert_mapping(&db.pool, vinland, "anilist", "v-anilist")
        .await
        .expect("map");

    let anilist = admin_list_unmapped(&db.pool, "anilist", None, 10)
        .await
        .expect("queue");
    assert_eq!(
        anilist
            .iter()
            .map(|r| (r.series_title.as_str(), r.source_count))
            .collect::<Vec<_>>(),
        vec![("Berserk", 3), ("Frieren", 1)],
        "Berserk is mapped at mal, which says nothing about anilist"
    );

    let mal = admin_list_unmapped(&db.pool, "mal", None, 10)
        .await
        .expect("queue");
    assert_eq!(
        mal.iter()
            .map(|r| r.series_title.as_str())
            .collect::<Vec<_>>(),
        vec!["Frieren", "Vinland Saga"],
        "equal source counts fall back to title order"
    );
}

/// The queue's title search is an emptiness check, not a minimum length.
///
/// The guard reads `format!("%{q}%").len() > 2`, which looks like "ignore queries under three
/// characters" and is not: the two `%` already make the string two characters long, so the
/// rule is exactly "the trimmed query is non-empty". Pinned as written, because the next
/// reader will otherwise either "fix" the off-by-two or assume a minimum that is not enforced.
#[tokio::test]
async fn the_assign_queues_search_ignores_only_a_blank_query() {
    let db = TestDb::spawn().await;
    a_series(&db, "Berserk", 1, &[]).await;
    a_series(&db, "Vinland Saga", 1, &[]).await;

    let matched = |q: Option<&str>| {
        let pool = db.pool.clone();
        let q = q.map(str::to_owned);
        async move {
            admin_list_unmapped(&pool, "anilist", q.as_deref(), 10)
                .await
                .expect("queue")
                .into_iter()
                .map(|r| r.series_title)
                .collect::<Vec<_>>()
        }
    };

    assert_eq!(matched(Some("vinland")).await, vec!["Vinland Saga"]);
    // Case-insensitive, and a substring rather than a prefix.
    assert_eq!(matched(Some("SAGA")).await, vec!["Vinland Saga"]);
    // One character is still a filter.
    assert_eq!(matched(Some("k")).await, vec!["Berserk"]);
    // Blank and whitespace-only are not.
    for blank in ["", "   "] {
        assert_eq!(matched(Some(blank)).await.len(), 2, "query={blank:?}");
    }
    assert_eq!(matched(None).await.len(), 2);
}

// ---------------------------------------------------------------------------
// Unmatched remote entries
// ---------------------------------------------------------------------------

/// The unmatched queue shows entries with no series, for one provider, alphabetically — and
/// assigning one removes it.
///
/// Two predicates carry it: `series_id IS NULL` (an already-matched entry is finished work)
/// and `provider = $1`. The user join is what puts a name next to each row; getting it wrong
/// attributes one user's library to another in an operator-visible list.
#[tokio::test]
async fn the_unmatched_queue_shows_only_unassigned_entries_for_one_provider() {
    let db = TestDb::spawn().await;
    let alice = db.seed_user("alice", &[], AccountStatus::Active).await;
    let bob = db.seed_user("bob", &[], AccountStatus::Active).await;
    let berserk = a_series(&db, "Berserk", 1, &[]).await;

    sync::upsert_remote_entries(
        &db.pool,
        alice,
        "anilist",
        &[
            a_remote_entry("30002", "Vinland Saga", None),
            a_remote_entry("30001", "Berserk", Some(berserk)),
            a_remote_entry("30003", "Frieren", None),
        ],
    )
    .await
    .expect("seed entries");
    sync::upsert_remote_entries(
        &db.pool,
        alice,
        "mal",
        &[a_remote_entry("m-1", "Oyasumi Punpun", None)],
    )
    .await
    .expect("seed entries");
    sync::upsert_remote_entries(
        &db.pool,
        bob,
        "anilist",
        &[a_remote_entry("30004", "Chainsaw Man", None)],
    )
    .await
    .expect("seed entries");

    let queue = admin_list_unmatched_remote(&db.pool, "anilist", None, 10)
        .await
        .expect("queue");
    assert_eq!(
        queue
            .iter()
            .map(|r| (r.username.as_str(), r.title.as_str()))
            .collect::<Vec<_>>(),
        vec![
            ("bob", "Chainsaw Man"),
            ("alice", "Frieren"),
            ("alice", "Vinland Saga"),
        ],
        "alphabetical by title; the matched Berserk entry and mal's entry are excluded"
    );
    assert!(
        (queue[1].progress - 12.0).abs() < f64::EPSILON,
        "the snapshot's progress travels with it, got {}",
        queue[1].progress
    );
    assert_eq!(queue[1].start_year, Some(1989));

    // Narrowing works from the remote side too.
    let narrowed = admin_list_unmatched_remote(&db.pool, "anilist", Some("saga"), 10)
        .await
        .expect("queue");
    assert_eq!(narrowed.len(), 1);
    assert_eq!(narrowed[0].title, "Vinland Saga");

    // Assigning is what takes an entry out of the queue.
    mark_remote_entry_matched(&db.pool, alice, "anilist", "30003", berserk)
        .await
        .expect("assign");
    let after = admin_list_unmatched_remote(&db.pool, "anilist", None, 10)
        .await
        .expect("queue");
    assert_eq!(
        after.iter().map(|r| r.title.as_str()).collect::<Vec<_>>(),
        vec!["Chainsaw Man", "Vinland Saga"]
    );
}

/// Both single-entry statements are scoped to the owning user.
///
/// Two users can hold the same `external_id` — they are both tracking the same work at the
/// same provider, which is the normal case. `get_remote_entry` without `user_id = $1` returns
/// whichever row Postgres reaches first; `mark_remote_entry_matched` without it assigns every
/// user's copy from one operator click.
#[tokio::test]
async fn the_single_entry_reads_and_writes_are_scoped_to_one_user() {
    let db = TestDb::spawn().await;
    let alice = db.seed_user("alice", &[], AccountStatus::Active).await;
    let bob = db.seed_user("bob", &[], AccountStatus::Active).await;
    let berserk = a_series(&db, "Berserk", 1, &[]).await;

    let mut alices = a_remote_entry("30001", "Berserk", None);
    alices.progress = 42.0;
    let mut bobs = a_remote_entry("30001", "Berserk", None);
    bobs.progress = 7.0;
    sync::upsert_remote_entries(&db.pool, alice, "anilist", &[alices])
        .await
        .expect("seed");
    sync::upsert_remote_entries(&db.pool, bob, "anilist", &[bobs])
        .await
        .expect("seed");

    let got = get_remote_entry(&db.pool, alice, "anilist", "30001")
        .await
        .expect("read")
        .expect("alice's entry");
    assert!(
        (got.progress - 42.0).abs() < f64::EPSILON,
        "alice's snapshot, not bob's, got {}",
        got.progress
    );
    assert!(
        get_remote_entry(&db.pool, alice, "mal", "30001")
            .await
            .expect("read")
            .is_none(),
        "the provider is part of the key"
    );

    mark_remote_entry_matched(&db.pool, alice, "anilist", "30001", berserk)
        .await
        .expect("assign");
    let still_queued = admin_list_unmatched_remote(&db.pool, "anilist", None, 10)
        .await
        .expect("queue");
    assert_eq!(
        still_queued
            .iter()
            .map(|r| r.username.as_str())
            .collect::<Vec<_>>(),
        vec!["bob"],
        "assigning alice's entry must not assign bob's"
    );
}

// ---------------------------------------------------------------------------
// Candidate suggestions
// ---------------------------------------------------------------------------

/// Suggestions come back best-similarity first, and an alternative title counts as a match.
///
/// The `ORDER BY 7 DESC` is a positional reference to the seventh select item. Inserting a
/// column before it re-sorts the operator's suggestion list by something arbitrary — with no
/// error, and with the correct rows. This test is the only thing that notices, which is why
/// it asserts the sequence rather than the set.
#[tokio::test]
async fn suggestions_rank_by_the_best_similarity_across_every_title() {
    let db = TestDb::spawn().await;
    a_series(&db, "Solo Leveling", 1, &[]).await;
    a_series(&db, "Solo Camping", 1, &[]).await;
    // Nothing like the query on its canonical title; reachable only through the alias.
    a_series(&db, "Chainsaw Man", 1, &["Solo Leveling Ragnarok"]).await;
    a_series(&db, "Berserk", 1, &[]).await;

    let hits = suggest_series_candidates(&db.pool, &normalize_title("Solo Leveling"), 10)
        .await
        .expect("suggest");
    let titles = hits.iter().map(|c| c.title.as_str()).collect::<Vec<_>>();

    assert_eq!(titles[0], "Solo Leveling", "an exact match ranks first");
    assert!(
        titles.contains(&"Chainsaw Man"),
        "a series matched only by an alternative title must still be suggested, got {titles:?}"
    );
    assert!(
        !titles.contains(&"Berserk"),
        "an unrelated title must not be suggested, got {titles:?}"
    );
    assert!(
        hits.windows(2).all(|w| w[0].similarity >= w[1].similarity),
        "suggestions must be ordered by similarity, got {:?}",
        hits.iter()
            .map(|c| (c.title.as_str(), c.similarity))
            .collect::<Vec<_>>()
    );

    let exact = &hits[0];
    assert!(exact.similarity > 0.99);
    assert_eq!(exact.content_type, "manga");
    assert_eq!(exact.release_year, Some(2000));
    assert_eq!(
        exact.source_count, 1,
        "the display fields travel with the score"
    );

    // The limit truncates the ranking, not an arbitrary slice of it.
    let capped = suggest_series_candidates(&db.pool, &normalize_title("Solo Leveling"), 1)
        .await
        .expect("suggest capped");
    assert_eq!(capped.len(), 1);
    assert_eq!(capped[0].title, "Solo Leveling");
}
