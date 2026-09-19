//! Every foreign key has an index its referential action can use.
//!
//! # The bug this exists to stop
//!
//! Postgres runs a foreign key's `ON DELETE` action once per deleted parent row, as
//! `… WHERE fk_column = $1` against the referencing table. Without an index leading on that
//! column, each run is a sequential scan of the whole table. The console's catalogue purge
//! deletes 500 series per batch, and twelve tables referenced `series`/`series_sources` on
//! unindexed columns — `series_cooccurrence.other_id` and `merge_candidates.candidate_id` among
//! them — so one batch took longer than the 30 s request timeout, was cancelled and rolled
//! back, and "Wipe the entire catalogue" failed with a `408` on every attempt. Raising the
//! statement timeout could not help: the request, not the statement, was what expired.
//! Account erasure and provider deletion had the same shape against `users` and `providers`.
//!
//! Nothing in the plan audit (`repo_query_plans`) can see this: the cascade queries are issued by
//! Postgres itself and never appear in the `.sqlx` cache.
//!
//! Gated behind the `integration` feature (requires Docker).
#![cfg(feature = "integration")]

use sqlx::Row as _;
use tankovault_test_support::TestDb;

/// Foreign keys, as `table(columns) -> parent`, that no usable index leads with.
///
/// An index qualifies when its first `n` key columns are the FK's `n` columns in any order (the
/// action's predicate is a conjunction of equalities), and it is either total or restricted only
/// to `col IS NOT NULL`, which that equality implies.
const UNINDEXED_FOREIGN_KEYS: &str = "\
    SELECT format('%s(%s) -> %s', c.conrelid::regclass, \
                  (SELECT string_agg(a.attname, ', ' ORDER BY k.ord) \
                   FROM unnest(c.conkey) WITH ORDINALITY AS k(attnum, ord) \
                   JOIN pg_attribute a ON a.attrelid = c.conrelid AND a.attnum = k.attnum), \
                  c.confrelid::regclass) AS fk \
    FROM pg_constraint c \
    JOIN pg_namespace n ON n.oid = c.connamespace \
    WHERE c.contype = 'f' AND n.nspname = 'public' \
      AND NOT EXISTS ( \
        SELECT 1 FROM pg_index i \
        CROSS JOIN LATERAL ( \
          SELECT (string_to_array(i.indkey::text, ' ')::int2[])[1:cardinality(c.conkey)] AS lead \
        ) l \
        WHERE i.indrelid = c.conrelid AND i.indisvalid \
          AND (i.indpred IS NULL \
               OR (cardinality(c.conkey) = 1 \
                   AND pg_get_expr(i.indpred, i.indrelid) = format('(%I IS NOT NULL)', \
                       (SELECT attname FROM pg_attribute \
                        WHERE attrelid = c.conrelid AND attnum = c.conkey[1])))) \
          AND l.lead @> c.conkey AND l.lead <@ c.conkey) \
    ORDER BY 1";

/// Foreign keys left unindexed on purpose, each with why its parent delete stays cheap.
///
/// Only a table whose size is bounded independently of users and the catalogue belongs here.
const EXEMPT: &[(&str, &str)] = &[
    (
        "feature_flag_overrides(updated_by) -> users",
        "one row per feature key defined in code",
    ),
    (
        "tunable_overrides(updated_by) -> users",
        "one row per tunable key defined in code",
    ),
    (
        "mfa_challenges(user_id) -> users",
        "rows live until a short expiry and are swept; bounded by in-flight sign-ins",
    ),
    (
        "step_up_grants(user_id) -> users",
        "rows live until a short expiry and are swept; bounded by in-flight step-ups",
    ),
    (
        "webauthn_ceremonies(user_id) -> users",
        "rows live until a short expiry and are swept; bounded by in-flight ceremonies",
    ),
];

#[tokio::test]
async fn every_foreign_key_has_a_usable_index() {
    let db = TestDb::spawn().await;
    let unindexed: Vec<String> = sqlx::query(UNINDEXED_FOREIGN_KEYS)
        .fetch_all(&db.pool)
        .await
        .expect("list unindexed foreign keys")
        .iter()
        .map(|row| row.get("fk"))
        .collect();

    let missing: Vec<&str> = unindexed
        .iter()
        .map(String::as_str)
        .filter(|fk| !EXEMPT.iter().any(|(exempt, _)| exempt == fk))
        .collect();
    assert!(
        missing.is_empty(),
        "foreign keys with no index leading on their columns; every parent delete scans the \
         referencing table once per row. Index them, or exempt a table bounded independently of \
         users and the catalogue:\n  {}",
        missing.join("\n  ")
    );

    let stale: Vec<&str> = EXEMPT
        .iter()
        .map(|(fk, _)| *fk)
        .filter(|fk| !unindexed.iter().any(|u| u == fk))
        .collect();
    assert!(
        stale.is_empty(),
        "exempted foreign keys that are now indexed or gone; remove them from EXEMPT:\n  {}",
        stale.join("\n  ")
    );
}
