//! The browse/discover read model (`crates/db/src/repo/catalog/browse.rs`, TEST F-05).
//!
//! This is the unauthenticated, highest-traffic route in the product, and PERF-2 left it with
//! **three copies of the same nine-predicate `WHERE` clause** — one in the recency page
//! statement, one in the sort-token page statement, one in the count. `sqlx`'s compile-time
//! macros need a string literal, so the duplication cannot be factored away; the only thing
//! that can catch a predicate drifting in one copy and not the others is a test that asserts
//! the three agree. That is what [`the_three_copies_of_the_where_clause_agree`] does, and it
//! is the reason this file exists.
//!
//! The failure it guards against is quiet: a filter that applies under four sort orders and
//! not the fifth returns a plausible page, and a total that disagrees with the page gives the
//! pager a page number that comes back empty.
//!
//! Opt-in: gated behind the `integration` feature because it requires Docker.
#![cfg(feature = "integration")]

use tankovault_config::MatchingConfig;
use tankovault_db::repo::catalog::{
    ChapterUpsert, ScannedSeries, SeriesFilter, SeriesSort, SeriesUpsert, ingest_series,
    list_series, list_series_authors, list_series_filtered, list_series_tags, list_series_titles,
    list_tags,
};
use tankovault_db::repo::providers::{self, NewProvider};
use tankovault_domain::{
    AdapterKind, ContentType, Politeness, ProviderId, SeriesId, SeriesStatus, normalize_title,
};
use tankovault_test_support::TestDb;

// ---------------------------------------------------------------------------
// Fixture
// ---------------------------------------------------------------------------

/// One series to ingest. Titles are deliberately unlike one another so the canonicalisation
/// pipeline cannot collapse two of them into one and quietly shrink the corpus.
struct Seed {
    title: &'static str,
    content_type: ContentType,
    status: SeriesStatus,
    release_year: Option<i32>,
    tags: &'static [&'static str],
    authors: &'static [&'static str],
    alt_titles: &'static [&'static str],
    chapters: usize,
}

/// The corpus every test in this file browses. Five series, chosen so that each filter has at
/// least one row on both sides of it and no two sort orders produce the same sequence.
const CORPUS: &[Seed] = &[
    Seed {
        title: "Berserk",
        content_type: ContentType::Manga,
        status: SeriesStatus::Completed,
        release_year: Some(1989),
        tags: &["Action", "Dark Fantasy"],
        authors: &["Kentaro Miura"],
        alt_titles: &[],
        chapters: 100,
    },
    Seed {
        title: "Solo Leveling",
        content_type: ContentType::Manhwa,
        status: SeriesStatus::Completed,
        release_year: Some(2018),
        tags: &["Action", "Dark Fantasy"],
        authors: &["Chugong"],
        alt_titles: &["Na Honjaman Level Up"],
        chapters: 200,
    },
    Seed {
        title: "Vinland Saga",
        content_type: ContentType::Manga,
        status: SeriesStatus::Ongoing,
        release_year: Some(2005),
        tags: &["Action", "Historical"],
        authors: &["Makoto Yukimura"],
        alt_titles: &[],
        chapters: 50,
    },
    Seed {
        title: "Frieren",
        content_type: ContentType::Manga,
        status: SeriesStatus::Ongoing,
        release_year: Some(2020),
        tags: &["Dark Fantasy"],
        authors: &["Kanehito Yamada"],
        alt_titles: &[],
        chapters: 10,
    },
    Seed {
        title: "Oyasumi Punpun",
        content_type: ContentType::Manga,
        status: SeriesStatus::Completed,
        release_year: None,
        tags: &[],
        authors: &[],
        alt_titles: &[],
        chapters: 5,
    },
];

/// Pin every series' `updated_at` to a known instant.
///
/// Two tests need this rather than the ingest order: the recency order must be predictable,
/// and `sources`/`year` order tie-breaks on `updated_at DESC` so the expected sequence is only
/// well defined once the timestamps are. Ingesting the same series from a second provider
/// re-touches its row, so the natural order is not the seeding order.
const PIN_UPDATED_AT: &str = "UPDATE series SET updated_at = timestamptz '2024-01-01 00:00:00Z' \
     + CASE canonical_title \
         WHEN 'Berserk' THEN interval '1 day' \
         WHEN 'Solo Leveling' THEN interval '2 days' \
         WHEN 'Vinland Saga' THEN interval '3 days' \
         WHEN 'Frieren' THEN interval '4 days' \
         ELSE interval '5 days' END";

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

async fn ingest(db: &TestDb, provider_id: ProviderId, seed: &Seed, chapters: usize) -> SeriesId {
    ingest_series(
        &db.pool,
        &ScannedSeries {
            provider_id,
            source_path: format!("/s/{}", normalize_title(seed.title).replace(' ', "-")),
            provider_title: Some(seed.title.to_owned()),
            meta: SeriesUpsert {
                canonical_title: seed.title.to_owned(),
                normalized_title: normalize_title(seed.title),
                description: None,
                cover_url: None,
                content_type: seed.content_type,
                status: seed.status,
                release_year: seed.release_year,
            },
            alt_titles: seed
                .alt_titles
                .iter()
                .map(|t| ((*t).to_owned(), normalize_title(t)))
                .collect(),
            tags: seed.tags.iter().map(|t| (*t).to_owned()).collect(),
            authors: seed.authors.iter().map(|a| (*a).to_owned()).collect(),
            chapters: (1..=chapters)
                .map(|n| ChapterUpsert {
                    #[expect(
                        clippy::cast_precision_loss,
                        reason = "a fixture index, far below f64's exact-integer range"
                    )]
                    number: n as f64,
                    volume: None,
                    title: None,
                    path: format!("/c/{n}"),
                    published_at: None,
                })
                .collect(),
            content_hash: vec![1],
        },
        &MatchingConfig::default(),
    )
    .await
    .expect("ingest series")
    .series_id
}

/// Seed the whole corpus on provider `alpha`, plus a second source for `Solo Leveling` on
/// `beta` so `source_count`, the `sources` order and the `provider_slug` filter all have
/// something to distinguish.
async fn seed_corpus(db: &TestDb) {
    let alpha = a_provider(db, "alpha").await;
    let beta = a_provider(db, "beta").await;
    for seed in CORPUS {
        ingest(db, alpha, seed, seed.chapters).await;
    }
    let solo = &CORPUS[1];
    ingest(db, beta, solo, 20).await;
    db.execute(PIN_UPDATED_AT).await;
}

/// A filter with `limit` large enough to hold the whole corpus, so a page is the match set.
fn all_of(filter: SeriesFilter) -> SeriesFilter {
    SeriesFilter {
        limit: 100,
        ..filter
    }
}

fn titles(page: &tankovault_db::repo::catalog::SeriesPage) -> Vec<&str> {
    page.items
        .iter()
        .map(|i| i.series.canonical_title.as_str())
        .collect()
}

fn sorted_titles(page: &tankovault_db::repo::catalog::SeriesPage) -> Vec<&str> {
    let mut t = titles(page);
    t.sort_unstable();
    t
}

/// Every filter shape the API can construct, named so a failure says which one drifted.
fn filter_matrix() -> Vec<(&'static str, SeriesFilter)> {
    vec![
        ("unfiltered", SeriesFilter::default()),
        (
            "query",
            SeriesFilter {
                query: Some("berserk".to_owned()),
                ..SeriesFilter::default()
            },
        ),
        (
            "content_type",
            SeriesFilter {
                content_type: Some(ContentType::Manhwa),
                ..SeriesFilter::default()
            },
        ),
        (
            "status",
            SeriesFilter {
                status: Some(SeriesStatus::Completed),
                ..SeriesFilter::default()
            },
        ),
        (
            "provider_slug",
            SeriesFilter {
                provider_slug: Some("beta".to_owned()),
                ..SeriesFilter::default()
            },
        ),
        (
            "year_min",
            SeriesFilter {
                year_min: Some(2015),
                ..SeriesFilter::default()
            },
        ),
        (
            "year_max",
            SeriesFilter {
                year_max: Some(2000),
                ..SeriesFilter::default()
            },
        ),
        (
            "min_chapters",
            SeriesFilter {
                min_chapters: Some(50),
                ..SeriesFilter::default()
            },
        ),
        (
            "tags",
            SeriesFilter {
                tags: vec!["action".to_owned()],
                ..SeriesFilter::default()
            },
        ),
        (
            "tags_all",
            SeriesFilter {
                tags: vec!["action".to_owned(), "dark-fantasy".to_owned()],
                ..SeriesFilter::default()
            },
        ),
        (
            "exclude_tags",
            SeriesFilter {
                exclude_tags: vec!["dark-fantasy".to_owned()],
                ..SeriesFilter::default()
            },
        ),
        (
            "combined",
            SeriesFilter {
                status: Some(SeriesStatus::Completed),
                year_min: Some(1980),
                min_chapters: Some(50),
                tags: vec!["action".to_owned()],
                exclude_tags: vec!["historical".to_owned()],
                ..SeriesFilter::default()
            },
        ),
    ]
}

// ---------------------------------------------------------------------------
// The differential test — the reason this file exists
// ---------------------------------------------------------------------------

/// The recency page, the sort-token page and the count must select the same rows.
///
/// The `WHERE` clause is written out three times because `sqlx::query_as!` needs a string
/// literal and will not expand `concat!`. Nothing but this test notices when one copy gains a
/// predicate the others do not: the page still renders, the pager still counts, and a filter
/// silently applies under some sort orders and not others.
///
/// Asserted as a set, not a sequence — ordering is [`every_sort_order_orders_by_its_own_key`]'s
/// job, and mixing the two would make an ordering bug look like a filtering bug.
#[tokio::test]
async fn the_three_copies_of_the_where_clause_agree() {
    let db = TestDb::spawn().await;
    seed_corpus(&db).await;

    for (name, filter) in filter_matrix() {
        let recency = list_series_filtered(
            &db.pool,
            &all_of(SeriesFilter {
                sort: SeriesSort::Updated,
                ..filter.clone()
            }),
        )
        .await
        .unwrap_or_else(|e| panic!("{name}: recency page: {e}"));

        let by_token = list_series_filtered(
            &db.pool,
            &all_of(SeriesFilter {
                sort: SeriesSort::Title,
                ..filter.clone()
            }),
        )
        .await
        .unwrap_or_else(|e| panic!("{name}: sort-token page: {e}"));

        assert_eq!(
            sorted_titles(&recency),
            sorted_titles(&by_token),
            "{name}: the recency and sort-token statements select different rows"
        );
        assert_eq!(
            i64::try_from(recency.items.len()).unwrap(),
            recency.total,
            "{name}: the count disagrees with the page it is paging"
        );
        assert_eq!(
            recency.total, by_token.total,
            "{name}: the count depends on the sort order, which it must not"
        );
    }
}

// ---------------------------------------------------------------------------
// Filtering
// ---------------------------------------------------------------------------

/// Each filter narrows to exactly the rows it names.
///
/// The differential test above only proves the three statements agree — they could agree on
/// the wrong answer, which is what a dropped `AND` looks like. These expectations are written
/// out so a predicate that stops constraining anything fails here.
#[tokio::test]
async fn each_filter_narrows_to_exactly_its_rows() {
    let db = TestDb::spawn().await;
    seed_corpus(&db).await;

    let expected: &[(&str, &[&str])] = &[
        (
            "unfiltered",
            &[
                "Berserk",
                "Frieren",
                "Oyasumi Punpun",
                "Solo Leveling",
                "Vinland Saga",
            ],
        ),
        ("query", &["Berserk"]),
        ("content_type", &["Solo Leveling"]),
        ("status", &["Berserk", "Oyasumi Punpun", "Solo Leveling"]),
        // Only Solo Leveling has a second source, so this is also the assertion that
        // provider scoping reaches through `series_sources` rather than matching the series.
        ("provider_slug", &["Solo Leveling"]),
        ("year_min", &["Frieren", "Solo Leveling"]),
        ("year_max", &["Berserk"]),
        (
            "min_chapters",
            &["Berserk", "Solo Leveling", "Vinland Saga"],
        ),
        ("tags", &["Berserk", "Solo Leveling", "Vinland Saga"]),
        ("tags_all", &["Berserk", "Solo Leveling"]),
        ("exclude_tags", &["Oyasumi Punpun", "Vinland Saga"]),
        ("combined", &["Berserk", "Solo Leveling"]),
    ];

    let matrix = filter_matrix();
    for (name, want) in expected {
        let filter = matrix
            .iter()
            .find(|(n, _)| n == name)
            .map_or_else(|| panic!("no filter named {name}"), |(_, f)| f.clone());
        let page = list_series_filtered(&db.pool, &all_of(filter))
            .await
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(sorted_titles(&page), *want, "{name}");
    }
}

/// `year_max` must not swallow the series that has no year at all.
///
/// `release_year <= $5` is NULL for `Oyasumi Punpun`, so it drops out — correct, and worth
/// pinning because the obvious "fix" of `COALESCE(release_year, 0)` would make an unknown year
/// sort and filter as 1 BC.
#[tokio::test]
async fn a_series_without_a_year_is_outside_every_year_bound() {
    let db = TestDb::spawn().await;
    seed_corpus(&db).await;

    for filter in [
        SeriesFilter {
            year_min: Some(1),
            ..SeriesFilter::default()
        },
        SeriesFilter {
            year_max: Some(9999),
            ..SeriesFilter::default()
        },
    ] {
        let page = list_series_filtered(&db.pool, &all_of(filter))
            .await
            .expect("year bound");
        assert!(
            !titles(&page).contains(&"Oyasumi Punpun"),
            "a NULL release_year must not satisfy a year bound"
        );
        assert_eq!(page.total, 4);
    }
}

/// `tags` is an AND over slugs and `exclude_tags` is an OR — the two are written with
/// different SQL shapes (`EXCEPT` versus `= ANY`) and are easy to transpose.
#[tokio::test]
async fn tags_require_all_and_exclude_tags_remove_any() {
    let db = TestDb::spawn().await;
    seed_corpus(&db).await;

    // A slug no series carries must exclude everything, not be ignored.
    let none = list_series_filtered(
        &db.pool,
        &all_of(SeriesFilter {
            tags: vec!["action".to_owned(), "isekai".to_owned()],
            ..SeriesFilter::default()
        }),
    )
    .await
    .expect("tags all");
    assert!(
        none.items.is_empty() && none.total == 0,
        "requiring an absent tag must match nothing, got {:?}",
        titles(&none)
    );

    // Excluding two slugs removes a series carrying either, not only one carrying both.
    let excluded = list_series_filtered(
        &db.pool,
        &all_of(SeriesFilter {
            exclude_tags: vec!["historical".to_owned(), "dark-fantasy".to_owned()],
            ..SeriesFilter::default()
        }),
    )
    .await
    .expect("exclude tags");
    assert_eq!(sorted_titles(&excluded), vec!["Oyasumi Punpun"]);
}

// ---------------------------------------------------------------------------
// Ordering
// ---------------------------------------------------------------------------

/// Every sort order produces its own sequence.
///
/// Four of the six are bound `CASE` expressions keyed on the token string, so a renamed token
/// disables that order and falls through to the trailing `updated_at DESC` — a page that looks
/// sorted by *something* and is not sorted by what was asked for. The corpus is chosen so no
/// two of these expected sequences coincide, which is what makes the assertion discriminating.
#[tokio::test]
async fn every_sort_order_orders_by_its_own_key() {
    let db = TestDb::spawn().await;
    seed_corpus(&db).await;

    let cases: &[(SeriesSort, &[&str])] = &[
        (
            SeriesSort::Updated,
            &[
                "Oyasumi Punpun",
                "Frieren",
                "Vinland Saga",
                "Solo Leveling",
                "Berserk",
            ],
        ),
        (
            SeriesSort::Title,
            &[
                "Berserk",
                "Frieren",
                "Oyasumi Punpun",
                "Solo Leveling",
                "Vinland Saga",
            ],
        ),
        (
            SeriesSort::Year,
            &[
                "Frieren",
                "Solo Leveling",
                "Vinland Saga",
                "Berserk",
                // NULLS LAST: no year sorts after 1989, never before 2020.
                "Oyasumi Punpun",
            ],
        ),
        (
            SeriesSort::Chapters,
            &[
                "Solo Leveling", // 200 on alpha + 20 on beta
                "Berserk",
                "Vinland Saga",
                "Frieren",
                "Oyasumi Punpun",
            ],
        ),
        (
            SeriesSort::Sources,
            &[
                "Solo Leveling", // the only series with two providers
                // The remaining four tie at one source and fall through to updated_at DESC.
                "Oyasumi Punpun",
                "Frieren",
                "Vinland Saga",
                "Berserk",
            ],
        ),
        (
            // No rating column exists; the token is accepted and served by recency. If this
            // ever diverges from the `Updated` sequence above, someone added a column and
            // forgot that `is_recency` still routes this order to the recency statement.
            SeriesSort::Rating,
            &[
                "Oyasumi Punpun",
                "Frieren",
                "Vinland Saga",
                "Solo Leveling",
                "Berserk",
            ],
        ),
    ];

    for (sort, want) in cases {
        let page = list_series_filtered(
            &db.pool,
            &all_of(SeriesFilter {
                sort: *sort,
                ..SeriesFilter::default()
            }),
        )
        .await
        .expect("sorted page");
        assert_eq!(titles(&page), *want, "sort={}", sort.as_token());
    }
}

/// Adjacent pages must neither repeat a row nor skip one when the leading sort key ties.
///
/// Both page statements end with `s.id DESC` for exactly this reason. Without it, rows sharing
/// `updated_at` have no defined order between two `OFFSET` queries, so page 2 can re-serve a
/// row from page 1 and lose another entirely — and the pager's total still looks right, which
/// is why nobody notices.
#[tokio::test]
async fn paging_a_tied_sort_key_neither_repeats_nor_skips() {
    let db = TestDb::spawn().await;
    seed_corpus(&db).await;
    db.execute("UPDATE series SET updated_at = timestamptz '2024-06-01 00:00:00Z'")
        .await;

    for sort in [SeriesSort::Updated, SeriesSort::Rating] {
        let mut seen = Vec::new();
        for offset in (0..6).step_by(2) {
            let page = list_series_filtered(
                &db.pool,
                &SeriesFilter {
                    sort,
                    limit: 2,
                    offset,
                    ..SeriesFilter::default()
                },
            )
            .await
            .expect("page");
            assert_eq!(page.total, 5, "the total must not depend on the offset");
            seen.extend(titles(&page).into_iter().map(str::to_owned));
        }
        seen.sort();
        let unique = {
            let mut u = seen.clone();
            u.dedup();
            u
        };
        assert_eq!(
            seen,
            unique,
            "sort={}: a row was served twice",
            sort.as_token()
        );
        assert_eq!(seen.len(), 5, "sort={}: a row was skipped", sort.as_token());
    }
}

// ---------------------------------------------------------------------------
// Search
// ---------------------------------------------------------------------------

/// A work searched for under a non-primary name must be found.
///
/// The alternative-title branch (`EXISTS … series_titles st WHERE st.normalized % $1`) is the
/// only reason a Korean romanisation finds the English release. `Solo Leveling`'s own
/// normalized title is nothing like the query, so this row can only come back through
/// `series_titles` — dropping that branch fails here and nowhere else.
#[tokio::test]
async fn search_finds_a_series_by_its_alternative_title() {
    let db = TestDb::spawn().await;
    seed_corpus(&db).await;

    let page = list_series_filtered(
        &db.pool,
        &all_of(SeriesFilter {
            query: Some("na honjaman level up".to_owned()),
            ..SeriesFilter::default()
        }),
    )
    .await
    .expect("alt-title search");
    assert_eq!(titles(&page), vec!["Solo Leveling"]);

    // The unfiltered listing carries the same three-way predicate; both must agree.
    let listed = list_series(&db.pool, Some("na honjaman level up"), 10)
        .await
        .expect("list_series search");
    assert_eq!(
        listed
            .iter()
            .map(|i| i.series.canonical_title.as_str())
            .collect::<Vec<_>>(),
        vec!["Solo Leveling"]
    );
}

/// A blank or whitespace-only query is *no* search, not a search for nothing.
///
/// `list_series_filtered` trims and discards an empty query before binding it. If it bound
/// `''` instead, the trigram predicate would match no row and Discover would render empty for
/// a user who cleared the search box.
#[tokio::test]
async fn a_blank_query_is_not_a_filter() {
    let db = TestDb::spawn().await;
    seed_corpus(&db).await;

    for query in ["", "   "] {
        let page = list_series_filtered(
            &db.pool,
            &all_of(SeriesFilter {
                query: Some(query.to_owned()),
                ..SeriesFilter::default()
            }),
        )
        .await
        .expect("blank query");
        assert_eq!(page.total, 5, "query={query:?}");
    }
}

// ---------------------------------------------------------------------------
// Per-series reads
// ---------------------------------------------------------------------------

/// The per-series enrichment reads return what was ingested, alphabetically, and an empty
/// vector — never an error — for a series that has none.
#[tokio::test]
async fn the_per_series_reads_are_alphabetical_and_empty_when_unset() {
    let db = TestDb::spawn().await;
    seed_corpus(&db).await;

    let page = list_series_filtered(&db.pool, &all_of(SeriesFilter::default()))
        .await
        .expect("page");
    let id_of = |title: &str| {
        page.items
            .iter()
            .find(|i| i.series.canonical_title == title)
            .map_or_else(|| panic!("{title} not in the corpus"), |i| i.series.id)
    };

    let berserk = id_of("Berserk");
    assert_eq!(
        list_series_tags(&db.pool, berserk)
            .await
            .expect("tags")
            .into_iter()
            .map(|t| (t.slug, t.name))
            .collect::<Vec<_>>(),
        vec![
            ("action".to_owned(), "Action".to_owned()),
            ("dark-fantasy".to_owned(), "Dark Fantasy".to_owned()),
        ],
        "tags come back by name, and the slug is the lowercased, dashed form"
    );
    assert_eq!(
        list_series_authors(&db.pool, berserk)
            .await
            .expect("authors")
            .into_iter()
            .map(|a| a.name)
            .collect::<Vec<_>>(),
        vec!["Kentaro Miura".to_owned()]
    );

    let solo = id_of("Solo Leveling");
    assert_eq!(
        list_series_titles(&db.pool, solo).await.expect("titles"),
        vec!["Na Honjaman Level Up".to_owned()]
    );

    let punpun = id_of("Oyasumi Punpun");
    assert!(
        list_series_tags(&db.pool, punpun)
            .await
            .expect("tags")
            .is_empty()
    );
    assert!(
        list_series_authors(&db.pool, punpun)
            .await
            .expect("authors")
            .is_empty()
    );
    assert!(
        list_series_titles(&db.pool, punpun)
            .await
            .expect("titles")
            .is_empty()
    );

    // An unknown id is an empty list rather than an error, which is what lets the series
    // detail handler answer 404 from the series read alone.
    let missing = SeriesId::from_uuid(uuid::Uuid::nil());
    assert!(
        list_series_tags(&db.pool, missing)
            .await
            .expect("tags")
            .is_empty()
    );
}

/// The tag vocabulary is de-duplicated across series and returned by display name.
#[tokio::test]
async fn list_tags_returns_each_tag_once() {
    let db = TestDb::spawn().await;
    seed_corpus(&db).await;

    let tags = list_tags(&db.pool).await.expect("tags");
    assert_eq!(
        tags.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(),
        vec!["Action", "Dark Fantasy", "Historical"],
        "three tags across five series, each written once despite being ingested repeatedly"
    );
}

/// The plain listing counts *providers*, not source rows.
///
/// `count(DISTINCT ss.provider_id)` is what makes re-scanning one provider leave the badge at
/// 1. A plain `count(*)` passes every other test in this file.
#[tokio::test]
async fn source_count_counts_distinct_providers() {
    let db = TestDb::spawn().await;
    seed_corpus(&db).await;

    let listed = list_series(&db.pool, None, 100).await.expect("list");
    for item in &listed {
        let want = i64::from(item.series.canonical_title == "Solo Leveling") + 1;
        assert_eq!(
            item.source_count, want,
            "{}: source_count",
            item.series.canonical_title
        );
    }
}
