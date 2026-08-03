//! A production-shaped workload fixture, for suites that assert on *query plans* rather than on
//! rows.
//!
//! # Why this is not `seed`
//!
//! [`crate::seed`] builds the handful of entities a correctness test names. That is the wrong
//! fixture for a plan assertion, because a planner choice is a cost comparison: below a few
//! thousand rows a sequential scan is genuinely cheaper, so a "does not scan" assertion over a
//! seed-sized table would pass for a query that scans everything in production and fail for one
//! that does not. **Volume is the fixture here**, and a table left empty is a table whose plans
//! carry no information at all — not a table that passed.
//!
//! Row counts are production's, divided by the same factor throughout ([`SERIES`] against the
//! ~54 000 series the real catalogue holds), so the *ratios* the planner reasons about survive.
//! Two further properties are load-bearing and easy to lose:
//!
//! - **Trigram diversity.** Titles drawn from a small vocabulary share almost all their trigrams,
//!   so `%` matches most of the table and a sequential scan is again the correct plan. The
//!   generator below builds pseudo-words from syllables for exactly this reason; a "realistic"
//!   word list would silently break the suite.
//! - **Row width.** An index-versus-scan choice turns on how many *pages* a table occupies, not
//!   how many rows, so series carry a description of production-like length. Narrow rows push the
//!   crossover far above any row count a test would seed.
//!
//! The generator is deterministic (fixed seed, no `random()`), so an assertion cannot flake on a
//! fixture that happened to come out differently.

use sqlx::PgPool;

/// Series rows. Everything else is scaled against this; see the module docs.
pub(crate) const SERIES: usize = 20_000;
/// Alternative titles per series (production: 95 815 / 54 551).
const ALT_TITLES_PER_SERIES: usize = 2;
/// Chapters per source (production: 2 837 534 / 54 551). The single largest table, and the one
/// whose plans are worthless without volume.
const CHAPTERS_PER_SOURCE: i64 = 25;
/// Tag links per series (production: 170 753 / 54 551).
const TAGS_PER_SERIES: i64 = 3;
/// Distinct providers, matching the handful a deployment configures.
const PROVIDERS: i64 = 5;
/// Reader accounts, and the watchlist/progress/notification rows hung off each.
const USERS: i64 = 200;
const WATCHED_PER_USER: i64 = 50;
const NOTIFICATIONS_PER_USER: i64 = 100;
const AUDIT_ROWS: i64 = 5_000;

/// Fixed seed for [`Lcg`]. Any value works; it is pinned so every machine and every run builds a
/// byte-identical fixture.
const SEED: u64 = 0x5461_6E6B_6F56_6175;

/// A small linear congruential generator, so the fixture needs no `rand` dependency.
///
/// The constants are Knuth's MMIX values. Quality is irrelevant — the only requirement is that
/// the output be well spread and identical on every run.
struct Lcg(u64);

impl Lcg {
    fn new(seed: u64) -> Self {
        Self(seed)
    }

    /// The high bits of the next state, which are the well-mixed ones in an LCG.
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.0 >> 33
    }

    /// A value in `0..n`.
    fn below(&mut self, n: usize) -> usize {
        usize::try_from(self.next()).unwrap_or(usize::MAX) % n
    }
}

const ONSETS: &[u8] = b"bcdfghjklmnprstvwyz";
const VOWELS: &[u8] = b"aeiou";
const CODAS: &[u8] = b"nrslt";

/// A pronounceable pseudo-word of two to four syllables.
fn word(rng: &mut Lcg) -> String {
    let syllables = 2 + rng.below(3);
    let mut out = String::with_capacity(syllables * 3);
    for _ in 0..syllables {
        out.push(char::from(ONSETS[rng.below(ONSETS.len())]));
        out.push(char::from(VOWELS[rng.below(VOWELS.len())]));
        if rng.below(100) < 35 {
            out.push(char::from(CODAS[rng.below(CODAS.len())]));
        }
    }
    out
}

/// A title of two to four [`word`]s, in the shape `tankovault_domain::normalize` produces:
/// lowercase, single-space separated.
fn title(rng: &mut Lcg) -> String {
    let words = 2 + rng.below(3);
    let mut out = String::new();
    for i in 0..words {
        if i > 0 {
            out.push(' ');
        }
        out.push_str(&word(rng));
    }
    out
}

/// Fill the schema with a production-shaped workload and `ANALYZE` it.
///
/// The `ANALYZE` is not optional: `CREATE DATABASE … TEMPLATE` copies statistics along with the
/// data, so analysing once here is what lets every cloned test database plan like production
/// without paying for it again. Without it the planner works from defaults and every assertion
/// built on this fixture becomes meaningless.
pub(crate) async fn seed(pool: &PgPool) {
    let mut rng = Lcg::new(SEED);
    seed_catalogue(pool, &mut rng).await;
    seed_sources_and_chapters(pool).await;
    seed_tags_and_authors(pool, &mut rng).await;
    seed_readers(pool).await;
    analyse(pool).await;
}

/// Series and their alternative titles: the trigram-searched half of the schema.
async fn seed_catalogue(pool: &PgPool, rng: &mut Lcg) {
    let titles: Vec<String> = (0..SERIES).map(|_| title(rng)).collect();
    // The description is derived server-side rather than sent: it exists only to make the row
    // production-width, and repeating the title is enough for that.
    sqlx::query(
        "INSERT INTO series (canonical_title, normalized_title, description) \
         SELECT t, t, repeat(t || ' ', 12) FROM UNNEST($1::text[]) AS u(t)",
    )
    .bind(&titles)
    .execute(pool)
    .await
    .expect("seed series");

    let alts: Vec<String> = (0..SERIES * ALT_TITLES_PER_SERIES)
        .map(|_| title(rng))
        .collect();
    // `WITH ORDINALITY` against `row_number()` pairs the nth alternative title with a series
    // without a second round trip or a per-row id lookup. `ON CONFLICT` because the primary key
    // is (series_id, normalized) and the generator may repeat a short title.
    sqlx::query(
        "INSERT INTO series_titles (series_id, title, normalized) \
         SELECT s.id, a.t, a.t \
         FROM UNNEST($1::text[]) WITH ORDINALITY AS a(t, rn) \
         JOIN (SELECT id, row_number() OVER (ORDER BY id) AS rn FROM series) s \
           ON s.rn = ((a.rn - 1) % $2::bigint) + 1 \
         ON CONFLICT DO NOTHING",
    )
    .bind(&alts)
    .bind(i64::try_from(SERIES).unwrap_or(i64::MAX))
    .execute(pool)
    .await
    .expect("seed alternative titles");
}

/// Providers, one source per series, and the chapters under them — by far the largest table, and
/// generated entirely server-side because sending a million rows over the wire is not worth it.
async fn seed_sources_and_chapters(pool: &PgPool) {
    sqlx::query(
        "INSERT INTO providers (slug, name, base_url, adapter) \
         SELECT 'p' || g, 'Provider ' || g, 'https://example.test/' || g, 'generic_config' \
         FROM generate_series(1, $1) g",
    )
    .bind(PROVIDERS)
    .execute(pool)
    .await
    .expect("seed providers");

    sqlx::query(
        "INSERT INTO series_sources (series_id, provider_id, source_path, chapter_count) \
         SELECT s.id, p.id, 'series/' || s.rn, $1 \
         FROM (SELECT id, row_number() OVER (ORDER BY id) - 1 AS rn FROM series) s \
         JOIN (SELECT id, row_number() OVER (ORDER BY slug) - 1 AS rn FROM providers) p \
           ON p.rn = s.rn % $2::bigint",
    )
    .bind(i32::try_from(CHAPTERS_PER_SOURCE).unwrap_or(i32::MAX))
    .bind(PROVIDERS)
    .execute(pool)
    .await
    .expect("seed series sources");

    // Chapter numbers ascend per source and `published_at` descends with them, so the ordering
    // the tracking queries page by is the ordering the data actually has.
    sqlx::query(
        "INSERT INTO chapters (series_source_id, number, path, published_at, discovered_at) \
         SELECT ss.id, g::numeric, 'chapter/' || g, \
                now() - (g || ' hours')::interval, now() - (g || ' hours')::interval \
         FROM series_sources ss CROSS JOIN generate_series(1, $1) g",
    )
    .bind(CHAPTERS_PER_SOURCE)
    .execute(pool)
    .await
    .expect("seed chapters");
}

/// Tag and author links, which the candidate queries aggregate per series.
async fn seed_tags_and_authors(pool: &PgPool, rng: &mut Lcg) {
    let tags: Vec<String> = (0..200).map(|_| word(rng)).collect();
    let authors: Vec<String> = (0..5_000).map(|_| title(rng)).collect();

    for (table, names) in [("tags", &tags), ("authors", &authors)] {
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "INSERT INTO {table} (slug, name) \
             SELECT n || '-' || rn, n FROM UNNEST($1::text[]) WITH ORDINALITY AS u(n, rn)"
        )))
        .bind(names)
        .execute(pool)
        .await
        .expect("seed tag/author names");
    }

    // The `generate_series` is what bounds the fan-out to [`TAGS_PER_SERIES`]: joining on a
    // shared modulus instead pairs every series with every tag in its residue class, which is
    // 66 tags each, not 3.
    sqlx::query(
        "INSERT INTO series_tags (series_id, tag_id) \
         SELECT s.id, t.id \
         FROM (SELECT id, row_number() OVER (ORDER BY id) - 1 AS rn FROM series) s \
         CROSS JOIN generate_series(0, $1 - 1) g \
         JOIN (SELECT id, row_number() OVER (ORDER BY slug) - 1 AS rn FROM tags) t \
           ON t.rn = (s.rn * $1 + g) % (SELECT count(*) FROM tags) \
         ON CONFLICT DO NOTHING",
    )
    .bind(TAGS_PER_SERIES)
    .execute(pool)
    .await
    .expect("seed series tags");

    sqlx::query(
        "INSERT INTO series_authors (series_id, author_id) \
         SELECT s.id, a.id \
         FROM (SELECT id, row_number() OVER (ORDER BY id) - 1 AS rn FROM series) s \
         JOIN (SELECT id, row_number() OVER (ORDER BY slug) - 1 AS rn FROM authors) a \
           ON a.rn = s.rn % 5000 \
         ON CONFLICT DO NOTHING",
    )
    .execute(pool)
    .await
    .expect("seed series authors");
}

/// Accounts and everything hung off them: the user-scoped half of the schema, whose queries are
/// the other place a missing index costs a page load.
async fn seed_readers(pool: &PgPool) {
    sqlx::query(
        "INSERT INTO users (email, username, password_hash, email_verified_at) \
         SELECT 'reader' || g || '@example.test', 'reader' || g, '$argon2id$fixture', now() \
         FROM generate_series(1, $1) g",
    )
    .bind(USERS)
    .execute(pool)
    .await
    .expect("seed users");

    // Each reader watches a contiguous slice of the catalogue, offset by their own index, so the
    // slices overlap the way real libraries do rather than partitioning the table.
    sqlx::query(
        "INSERT INTO watchlist_entries (user_id, series_id, status) \
         SELECT u.id, s.id, 'reading' \
         FROM (SELECT id, row_number() OVER (ORDER BY username) - 1 AS rn FROM users) u \
         CROSS JOIN generate_series(0, $1 - 1) g \
         JOIN (SELECT id, row_number() OVER (ORDER BY id) - 1 AS rn FROM series) s \
           ON s.rn = (u.rn * 7 + g) % $2::bigint \
         ON CONFLICT DO NOTHING",
    )
    .bind(WATCHED_PER_USER)
    .bind(i64::try_from(SERIES).unwrap_or(i64::MAX))
    .execute(pool)
    .await
    .expect("seed watchlist entries");

    sqlx::query(
        "INSERT INTO read_progress (user_id, series_id, last_read_whole_number) \
         SELECT w.user_id, w.series_id, 5 FROM watchlist_entries w ON CONFLICT DO NOTHING",
    )
    .execute(pool)
    .await
    .expect("seed read progress");

    sqlx::query(
        "INSERT INTO notifications (user_id, kind, payload, read_at, created_at) \
         SELECT u.id, 'new_chapter', '{}'::jsonb, \
                CASE WHEN g % 3 = 0 THEN now() ELSE NULL END, \
                now() - (g || ' minutes')::interval \
         FROM users u CROSS JOIN generate_series(1, $1) g",
    )
    .bind(NOTIFICATIONS_PER_USER)
    .execute(pool)
    .await
    .expect("seed notifications");

    sqlx::query(
        "INSERT INTO audit_log (actor_id, action, target, detail, outcome, created_at) \
         SELECT u.id, 'fixture.action', 'target/' || g, '{}'::jsonb, 'success', \
                now() - (g || ' seconds')::interval \
         FROM generate_series(1, $1) g \
         JOIN (SELECT id, row_number() OVER (ORDER BY username) - 1 AS rn FROM users) u \
           ON u.rn = g % $2::bigint",
    )
    .bind(AUDIT_ROWS)
    .bind(USERS)
    .execute(pool)
    .await
    .expect("seed audit log");
}

/// `VACUUM` then `ANALYZE` the whole database, so no table is left planning from defaults.
///
/// # The `VACUUM` is load-bearing, and its absence is a race
///
/// A GIN index accepts inserts into a *pending list* and folds them into the tree later
/// (`fastupdate`, on by default). `gincostestimate` reads the pending-list size from the index
/// metapage **at plan time** and charges a page fetch for every page of it, because a scan
/// really would have to read them all. Bulk-seeding this fixture leaves ~765 such pages, which
/// adds ~765 to the estimated cost of *every* trigram index scan — enough that the planner
/// prefers a sequential scan of `series_titles` (970) and the trigram assertions in
/// `crates/db/tests/repo_query_plans.rs` fail.
///
/// Nothing in the fixture flushed that list, so whether it was still there when the plan was
/// taken came down to whether autovacuum's 60-second naptime happened to fire first — which
/// made the suite pass on a slow machine (the build takes ~150 s) and fail on a fast CI runner
/// (~38 s). `VACUUM` flushes the list outright, which is also the state a production catalogue
/// is in, autovacuum having long since run.
///
/// `default_statistics_target = 1000` first, and it is doing real work rather than being
/// thorough for its own sake. `ANALYZE` reads a *sample* — 300 rows per unit of the target — and
/// a different sample means different estimates, so cost figures drift every time the fixture is
/// rebuilt and any assertion about them drifts with it. At 1000 the sample is 300 000 rows, more
/// than every table here except `chapters` holds, so all of those are read whole.
///
/// It reduces the drift rather than removing it: `chapters` is still sampled, and estimates for
/// the queries that touch it were still observed moving by ~12% between runs. Anything asserting
/// on a cost needs headroom for that — see the `Budget` ceilings in
/// `crates/db/tests/repo_query_plans.rs`.
///
/// All three statements go over one connection because the setting is per-session; on a pool
/// they would land on different ones and the `ANALYZE` would run at the default.
async fn analyse(pool: &PgPool) {
    let mut conn = pool
        .acquire()
        .await
        .expect("acquire a connection to analyse");
    for statement in ["SET default_statistics_target = 1000", "VACUUM", "ANALYZE"] {
        sqlx::query(sqlx::AssertSqlSafe(statement))
            .execute(&mut *conn)
            .await
            .expect("analyse the workload fixture");
    }
}

#[cfg(test)]
mod tests {
    use super::{Lcg, SEED, title};

    /// The fixture must be identical on every run: a plan assertion that flakes because the
    /// generator wandered is worse than no assertion, because it trains the suite to be ignored.
    #[test]
    fn generator_is_deterministic() {
        let first: Vec<String> = {
            let mut rng = Lcg::new(SEED);
            (0..64).map(|_| title(&mut rng)).collect()
        };
        let second: Vec<String> = {
            let mut rng = Lcg::new(SEED);
            (0..64).map(|_| title(&mut rng)).collect()
        };
        assert_eq!(first, second);
    }

    /// Trigram diversity is the property that makes the index worth using; near-duplicate titles
    /// would send the planner back to a sequential scan and quietly void every plan assertion.
    #[test]
    fn titles_are_diverse() {
        let mut rng = Lcg::new(SEED);
        let titles: Vec<String> = (0..2_000).map(|_| title(&mut rng)).collect();
        let distinct: std::collections::HashSet<&String> = titles.iter().collect();
        assert!(
            distinct.len() > 1_990,
            "generator produced {} distinct titles out of 2000",
            distinct.len()
        );
    }
}
