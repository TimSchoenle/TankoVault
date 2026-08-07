//! Discover/browse read-model tests: six statements — the recency page, the sort-token page and
//! the count, each in a search and a no-search form — must select the same rows for the same
//! filter. They share their filter predicate (one literal, spliced by `browse_statement!`), so
//! what can drift is the rest: the search branch's `matched` CTE and join, the ordering, and the
//! parameter numbering each statement chooses.
//!
//! Gated behind the `integration` feature (requires Docker).
#![cfg(feature = "integration")]

use tankovault_db::repo::catalog::{
    SeriesFilter, SeriesSort, list_series, list_series_authors, list_series_filtered,
    list_series_tags, list_series_titles, list_tags,
};
use tankovault_domain::{ContentType, ProviderId, SeriesId, SeriesStatus};
use tankovault_test_support::{TestDb, seed};

// Fixture

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

/// Pins each series' `updated_at` so recency order and `updated_at DESC` tie-breaks are
/// deterministic regardless of ingest order.
const PIN_UPDATED_AT: &str = "UPDATE series SET updated_at = timestamptz '2024-01-01 00:00:00Z' \
     + CASE canonical_title \
         WHEN 'Berserk' THEN interval '1 day' \
         WHEN 'Solo Leveling' THEN interval '2 days' \
         WHEN 'Vinland Saga' THEN interval '3 days' \
         WHEN 'Frieren' THEN interval '4 days' \
         ELSE interval '5 days' END";

/// Named `fixture`, not `seed`, to avoid shadowing the `seed` module.
async fn ingest(db: &TestDb, provider_id: ProviderId, fixture: &Seed, chapters: usize) -> SeriesId {
    let numbers: Vec<f64> = (1..=chapters)
        .map(|n| {
            #[expect(
                clippy::cast_precision_loss,
                reason = "a fixture index, far below f64's exact-integer range"
            )]
            let number = n as f64;
            number
        })
        .collect();
    seed::series(db, provider_id, fixture.title)
        .chapters(&numbers)
        .alt_titles(fixture.alt_titles)
        .tags(fixture.tags)
        .authors(fixture.authors)
        .content_type(fixture.content_type)
        .status(fixture.status)
        .release_year_opt(fixture.release_year)
        .create()
        .await
}

/// Seeds the corpus on provider `alpha`, plus a second `beta` source for Solo Leveling so
/// `source_count`, `sources` order and `provider_slug` all have something to distinguish.
async fn seed_corpus(db: &TestDb) {
    let alpha = seed::provider(db, "alpha").create().await;
    let beta = seed::provider(db, "beta").create().await;
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

/// Filter shapes with no search term, named so a failure says which one drifted.
///
/// Split from [`search_filter_matrix`] because the statements now branch on whether a term is
/// present: these exercise the arm with the search disjunction removed.
fn plain_filter_matrix() -> Vec<(&'static str, SeriesFilter)> {
    vec![
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

/// Filter shapes carrying a search term — the arm that unions the trigram and FTS index scans.
fn search_filter_matrix() -> Vec<(&'static str, SeriesFilter)> {
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
            "query_alt_title",
            SeriesFilter {
                query: Some("na honjaman level up".to_owned()),
                ..SeriesFilter::default()
            },
        ),
        (
            "query_combined",
            SeriesFilter {
                query: Some("berserk".to_owned()),
                status: Some(SeriesStatus::Completed),
                year_min: Some(1980),
                min_chapters: Some(50),
                tags: vec!["action".to_owned()],
                exclude_tags: vec!["historical".to_owned()],
                ..SeriesFilter::default()
            },
        ),
        (
            "query_matches_nothing",
            SeriesFilter {
                query: Some("zzzzqqqwxyv".to_owned()),
                ..SeriesFilter::default()
            },
        ),
    ]
}

/// Every filter shape the API can construct, both arms.
fn filter_matrix() -> Vec<(&'static str, SeriesFilter)> {
    let mut all = plain_filter_matrix();
    all.extend(search_filter_matrix());
    all
}

// The differential test

/// Every page and count statement must select the same rows for the same filter — the recency
/// page, the sort-token page, the relevance page and the count, each in the search and no-search
/// form the filter's `query` picks. Asserted as a set, not a sequence — ordering is
/// [`every_sort_order_orders_by_its_own_key`]'s and
/// [`an_exact_title_match_leads_the_relevance_order`]'s job.
///
/// The count is the one that bites: it is a separate statement with its own parameter numbering
/// and its own search branch, and a page it disagrees with is a pager offering a page that comes
/// back empty.
#[tokio::test]
async fn every_page_and_count_statement_selects_the_same_rows() {
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

        // The fourth statement, and the newest: it carries the shared filter predicate *and* two
        // more bound parameters of its own (the raw term and its normalized key), which is
        // exactly the shape that drifts. It is only reached with a search term; without one it
        // falls back to the recency statement, which this compares it against either way.
        let by_relevance = list_series_filtered(
            &db.pool,
            &all_of(SeriesFilter {
                sort: SeriesSort::Relevance,
                ..filter.clone()
            }),
        )
        .await
        .unwrap_or_else(|e| panic!("{name}: relevance page: {e}"));

        assert_eq!(
            sorted_titles(&recency),
            sorted_titles(&by_token),
            "{name}: the recency and sort-token statements select different rows"
        );
        assert_eq!(
            sorted_titles(&recency),
            sorted_titles(&by_relevance),
            "{name}: the relevance statement selects different rows from the recency one"
        );
        assert_eq!(
            recency.total, by_relevance.total,
            "{name}: the count disagrees with the relevance page it is paging"
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

// Filtering

/// Each filter narrows to exactly its rows — the differential test above only proves the
/// three statements agree, not that they're right.
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
        ("query_alt_title", &["Solo Leveling"]),
        ("query_combined", &["Berserk"]),
        ("query_matches_nothing", &[]),
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

/// A year bound must not swallow a series with no year: `release_year <= $5` is NULL for it,
/// so it drops out — `COALESCE(release_year, 0)` would wrongly make it sort as 1 BC.
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

// Ordering

/// Every sort order produces its own sequence; a renamed `CASE` token would silently fall
/// through to `updated_at DESC` instead of erroring.
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
            // No rating column yet; served by recency — diverging from Updated above means
            // `is_recency` no longer routes this here.
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

/// Adjacent pages must neither repeat nor skip a row when the leading sort key ties. Both page
/// statements end with `s.id DESC` for exactly this reason.
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

// Search

/// A work searched under a non-primary name must be found — only the `series_titles`
/// alternative-title branch can return this row, so dropping that branch fails only here.
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
    let listed = list_series(&db.pool, Some("na honjaman level up"), false, 10)
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

/// A title that *is* the query must come first, however stale it is.
///
/// This pins the defect the relevance order exists for. Search used to inherit the browse
/// grid's default, `updated`, so results came back in last-scanned order and the term played no
/// part in ranking at all: a reader typing an exact title got it wherever its most recent crawl
/// happened to put it — below every longer title that merely contains the word and was scanned
/// more recently. Trigram similarity alone does not fix that either, which is why the order has
/// explicit tiers: a short query is a large fraction of a short unrelated title, so
/// `similarity()` ranks "Berserk of Gluttony" above "Berserk" on its own.
///
/// The fixture is built so the two failure modes are distinguishable. Both series match the
/// term; the exact one is deliberately the *older* row, so an order that fell through to
/// `updated_at DESC` puts it second, and one that ranked by similarity alone puts it second too.
#[tokio::test]
async fn an_exact_title_match_leads_the_relevance_order() {
    let db = TestDb::spawn().await;
    let alpha = seed::provider(&db, "alpha").create().await;
    for title in ["Berserk", "Berserk of Gluttony", "Berserk: The Prototype"] {
        seed::series(&db, alpha, title)
            .chapters(&[1.0])
            .create()
            .await;
    }
    // The exact match is the least recently updated of the three.
    db.execute(
        "UPDATE series SET updated_at = timestamptz '2024-01-01 00:00:00Z' \
           + CASE canonical_title WHEN 'Berserk' THEN interval '1 day' ELSE interval '9 days' END",
    )
    .await;

    let relevance = list_series_filtered(
        &db.pool,
        &all_of(SeriesFilter {
            query: Some("Berserk".to_owned()),
            sort: SeriesSort::Relevance,
            ..SeriesFilter::default()
        }),
    )
    .await
    .expect("relevance page");
    assert_eq!(
        titles(&relevance).first().copied(),
        Some("Berserk"),
        "the exact title must lead; got {:?}",
        titles(&relevance)
    );

    // And the order this replaced still behaves as it did, so the fix is the new statement
    // rather than a change to what `updated` means.
    let recency = list_series_filtered(
        &db.pool,
        &all_of(SeriesFilter {
            query: Some("Berserk".to_owned()),
            sort: SeriesSort::Updated,
            ..SeriesFilter::default()
        }),
    )
    .await
    .expect("recency page");
    assert_eq!(
        titles(&recency).last().copied(),
        Some("Berserk"),
        "recency order is unchanged, and is exactly why search no longer defaults to it"
    );
}

/// An alternative title that is the query must lead too — a work is just as exactly named by a
/// synonym, and only the `series_titles` tier of the relevance order can put it first.
#[tokio::test]
async fn an_exact_alternative_title_leads_as_well() {
    let db = TestDb::spawn().await;
    seed_corpus(&db).await;
    let alpha = seed::provider(&db, "gamma").create().await;
    seed::series(&db, alpha, "Na Honjaman Level Up Redraw")
        .chapters(&[1.0])
        .create()
        .await;

    let page = list_series_filtered(
        &db.pool,
        &all_of(SeriesFilter {
            query: Some("Na Honjaman Level Up".to_owned()),
            sort: SeriesSort::Relevance,
            ..SeriesFilter::default()
        }),
    )
    .await
    .expect("relevance page");
    assert_eq!(
        titles(&page).first().copied(),
        Some("Solo Leveling"),
        "the series whose synonym is the query must lead; got {:?}",
        titles(&page)
    );
}

/// A blank/whitespace query is no search, not a search for nothing — binding `''` would match
/// no row via the trigram predicate and render Discover empty.
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

// Per-series reads

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

    let listed = list_series(&db.pool, None, false, 100).await.expect("list");
    for item in &listed {
        let want = i64::from(item.series.canonical_title == "Solo Leveling") + 1;
        assert_eq!(
            item.source_count, want,
            "{}: source_count",
            item.series.canonical_title
        );
    }
}
