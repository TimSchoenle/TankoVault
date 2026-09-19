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
//!
//! Nothing in the plan audit (`repo_query_plans`) can see this: the cascade queries are issued by
//! Postgres itself and never appear in the `.sqlx` cache.
//!
//! Gated behind the `integration` feature (requires Docker).
#![cfg(feature = "integration")]

use sqlx::Row as _;
use tankovault_test_support::TestDb;

/// Foreign keys onto the catalogue, as `table(columns) -> parent`, that no usable index leads with.
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
      AND c.confrelid IN ('series'::regclass, 'series_sources'::regclass) \
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

#[tokio::test]
async fn every_catalogue_foreign_key_has_a_usable_index() {
    let db = TestDb::spawn().await;
    let missing: Vec<String> = sqlx::query(UNINDEXED_FOREIGN_KEYS)
        .fetch_all(&db.pool)
        .await
        .expect("list unindexed foreign keys")
        .iter()
        .map(|row| row.get("fk"))
        .collect();
    assert!(
        missing.is_empty(),
        "foreign keys with no index leading on their columns; every parent delete scans the \
         referencing table once per row:\n  {}",
        missing.join("\n  ")
    );
}
