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
/// live-timing corroboration exists for it. That costs a review row, not a lost merge.
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

/// No merge confirmed wrong replays as automatic.
///
/// Each row is a merge a human confirmed wrong: a sequel absorbed through an alias, an arc or
/// edition absorbed through a shared english title, or two works joined by a junk or franchise
/// alias.
#[test]
fn confirmed_wrong_merges_are_not_automatic() {
    for (row, replayed) in corpus("confirmed_wrong") {
        assert_ne!(replayed.verdict(), "auto", "{}", describe(&row, &replayed));
    }
}

/// The tally counts stored-to-replayed transitions, and a row is replayed under the policy it was
/// decided with: a stored policy with the author guard off lets through a merge that the guard,
/// or a row with no policy and so the defaults, holds for review.
#[test]
fn rows_replay_under_their_stored_policy_and_tally_by_transition() {
    use tankovault_matcher::replay::Tally;

    let mut row = parse_row(
        r#"{"verdict":"auto","reason":"structural_identity_above_threshold",
            "evidence":{"trigram_similarity":1.0,
              "left":{"normalized_title":"phoenix","authors":["osamu tezuka"]},
              "right":{"normalized_title":"phoenix","authors":["someone else"]}},
            "policy":{"high":0.85,"low":0.6,"auto_merge":0.97,
              "guards":{"numeric_conflict":true,"author_conflict":false,
                        "year_conflict":false,"type_conflict":false}}}"#,
    )
    .expect("a row");
    let lenient = replay(&row);
    assert_eq!(lenient.verdict(), "auto");

    let mut policy = row.policy.expect("a stored policy");
    policy.guards.author_conflict = true;
    row.policy = Some(policy);
    let strict = replay(&row);
    assert_eq!(strict.verdict(), "review");
    assert_eq!(strict.adjudication.reason, "author_conflict");

    row.policy = None;
    assert_eq!(
        replay(&row).verdict(),
        "review",
        "the defaults switch every guard on"
    );

    let mut tally = Tally::default();
    tally.add(&row, &lenient);
    tally.add(&row, &strict);
    tally.add(&row, &strict);
    assert_eq!(tally.total(), 3);
    assert_eq!(tally.unchanged(), 1);
    assert_eq!(
        tally.transitions().collect::<Vec<_>>(),
        vec![("auto", "auto", 1), ("auto", "review", 2)]
    );
    assert!(parse_row("not json").is_err());
}
