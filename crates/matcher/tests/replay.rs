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

/// The harness reproduces the verdict each row was stored with.
#[test]
fn every_row_reproduces_its_stored_verdict() {
    for name in ["confirmed_same", "confirmed_wrong"] {
        for (row, replayed) in corpus(name) {
            assert_eq!(
                replayed.verdict(),
                row.verdict,
                "{}",
                describe(&row, &replayed)
            );
        }
    }
}
