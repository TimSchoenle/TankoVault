//! The replay corpus: journalled sweep decisions re-judged by the current matcher.
//!
//! `fixtures/confirmed_same.jsonl` holds merges known to be correct, `confirmed_wrong.jsonl`
//! merges known to be wrong. Both are in the export shape of query A10 in
//! `docs/MATCHING_OVERHAUL.md`, so `cargo run -p xtask -- matching-replay` reads them unchanged.
//! See `fixtures/README.md` for where the rows come from.

use tankovault_matcher::replay::{DecisionRow, Replayed, parse_row, replay};

fn corpus(name: &str) -> Vec<(DecisionRow, Replayed)> {
    let path = format!("{}/tests/fixtures/{name}.jsonl", env!("CARGO_MANIFEST_DIR"));
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
    let rows: Vec<_> = text
        .lines()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty())
        .map(|(i, line)| {
            let row = parse_row(line).unwrap_or_else(|e| panic!("{path}:{}: {e}", i + 1));
            let replayed = replay(&row);
            (row, replayed)
        })
        .collect();
    assert!(!rows.is_empty(), "{path} holds no rows");
    rows
}

fn describe(row: &DecisionRow, replayed: &Replayed) -> String {
    format!(
        "{}: replayed {} ({}), signals {:?}",
        row.case.as_deref().or(row.id.as_deref()).unwrap_or("?"),
        replayed.verdict(),
        replayed.adjudication.reason,
        replayed.reading.assessment.signals.labels(),
    )
}

const PRIMARY_IDENTITY: [&str; 3] = ["exact_title", "compact_identity", "source_title_identity"];

/// Correct merges stay automatic where they rested on a primary identity, and are never called
/// distinct.
///
/// A merge that rested on an alternative title alone is held for review until external-id or
/// live-timing corroboration exists for it (WP4/WP5 of `docs/MATCHING_OVERHAUL.md`). That is the
/// price of no longer merging on an alias alone, and it is a review row, not a lost merge.
#[test]
fn confirmed_correct_merges_are_kept_or_reviewed() {
    for (row, replayed) in corpus("confirmed_same") {
        let stored_primary = row
            .signals
            .iter()
            .any(|s| PRIMARY_IDENTITY.contains(&s.as_str()));
        if stored_primary {
            assert_eq!(replayed.verdict(), "auto", "{}", describe(&row, &replayed));
        } else {
            assert_ne!(
                replayed.verdict(),
                "distinct",
                "{}",
                describe(&row, &replayed)
            );
        }
    }
}

/// **No merge confirmed wrong in production replays as automatic.**
///
/// Each row is a merge the sweep performed and a human confirmed wrong — a sequel absorbed
/// through an alias, an arc or edition absorbed through a shared english title, two works joined
/// by a junk or franchise alias. Every one of them was lifted over the merge threshold by an
/// agreeing medium, year or credit, with the metadata guards switched off.
#[test]
fn confirmed_wrong_merges_are_not_automatic() {
    for (row, replayed) in corpus("confirmed_wrong") {
        assert_ne!(replayed.verdict(), "auto", "{}", describe(&row, &replayed));
    }
}
