//! `matching-replay`: re-judge exported sweep decisions with the current matcher, offline.
//!
//! The input is one `merge_decisions` row per line, exported by query A10 of
//! `docs/MATCHING_OVERHAUL.md`. The report is the stored-verdict → replayed-verdict table; with
//! `--out`, every row whose verdict changed is written as one JSON object per line.
//!
//! ```text
//! cargo run -p xtask -- matching-replay --input decisions.jsonl [--out changed.jsonl]
//! ```

use std::fmt::Write as _;
use std::io::{BufRead as _, BufWriter, Write as _};
use std::path::{Path, PathBuf};

use anyhow::Context as _;
use serde_json::json;
use tankovault_matcher::replay::{DecisionRow, Replayed, Tally, parse_row, replay};

/// Parsed command line.
pub(crate) struct Args {
    input: PathBuf,
    out: Option<PathBuf>,
}

impl Args {
    /// Read `--input <path> [--out <path>]`.
    ///
    /// # Errors
    /// A missing `--input`, a flag without its value, or an unknown argument.
    pub(crate) fn parse(args: &[String]) -> anyhow::Result<Self> {
        let mut input = None;
        let mut out = None;
        let mut it = args.iter();
        while let Some(flag) = it.next() {
            let slot = match flag.as_str() {
                "--input" => &mut input,
                "--out" => &mut out,
                other => anyhow::bail!("unknown argument {other:?}; {USAGE}"),
            };
            let value = it
                .next()
                .ok_or_else(|| anyhow::anyhow!("{flag} needs a path; {USAGE}"))?;
            *slot = Some(PathBuf::from(value));
        }
        Ok(Self {
            input: input.ok_or_else(|| anyhow::anyhow!("--input is required; {USAGE}"))?,
            out,
        })
    }
}

const USAGE: &str =
    "usage: xtask matching-replay --input <decisions.jsonl> [--out <changed.jsonl>]";

/// Replay every row of `args.input` and print the verdict-change table.
///
/// # Errors
/// An unreadable input, a line that is not a decision row (named by line number), or an
/// unwritable `--out`.
pub(crate) fn run(args: &Args) -> anyhow::Result<()> {
    let input = std::fs::File::open(&args.input)
        .with_context(|| format!("opening {}", args.input.display()))?;
    let mut out = args.out.as_deref().map(create).transpose()?;
    let mut tally = Tally::default();

    for (index, line) in std::io::BufReader::new(input).lines().enumerate() {
        let line = line.with_context(|| format!("reading line {}", index + 1))?;
        if line.trim().is_empty() {
            continue;
        }
        let row = parse_row(&line).with_context(|| format!("line {}", index + 1))?;
        let replayed = replay(&row);
        tally.add(&row, &replayed);
        if let Some(out) = out.as_mut()
            && row.verdict != replayed.verdict()
        {
            serde_json::to_writer(&mut *out, &changed(&row, &replayed))?;
            out.write_all(b"\n")?;
        }
    }
    if let Some(mut out) = out {
        out.flush()?;
    }

    print!("{}", render(&tally));
    Ok(())
}

fn create(path: &Path) -> anyhow::Result<BufWriter<std::fs::File>> {
    let file =
        std::fs::File::create(path).with_context(|| format!("creating {}", path.display()))?;
    Ok(BufWriter::new(file))
}

/// One changed row, as `--out` writes it.
fn changed(row: &DecisionRow, replayed: &Replayed) -> serde_json::Value {
    json!({
        "id": row.id,
        "case": row.case,
        "left_title": row.evidence.left.title,
        "right_title": row.evidence.right.title,
        "old_verdict": row.verdict,
        "old_reason": row.reason,
        "old_signals": row.signals,
        "new_verdict": replayed.verdict(),
        "new_reason": replayed.adjudication.reason,
        "new_blocked_by": replayed.adjudication.blocked_by,
        "new_signals": replayed.reading.assessment.signals.labels(),
        "new_score": replayed.reading.assessment.score,
        "matched_query_title": replayed.reading.matched_query_title,
        "matched_candidate_title": replayed.reading.matched_candidate_title,
    })
}

/// The verdict-change table as Markdown, ready to paste into a pull request.
fn render(tally: &Tally) -> String {
    let mut table = String::from("| stored | replayed | rows |\n|---|---|---|\n");
    for (old, new, count) in tally.transitions() {
        let _ = writeln!(table, "| {old} | {new} | {count} |");
    }
    let total = tally.total();
    let unchanged = tally.unchanged();
    #[expect(
        clippy::cast_precision_loss,
        reason = "a percentage for a human, of row counts far below f64's exact range"
    )]
    let share = if total == 0 {
        100.0
    } else {
        unchanged as f64 * 100.0 / total as f64
    };
    let _ = writeln!(
        table,
        "\n{unchanged} of {total} rows reproduce their stored verdict ({share:.2}%)."
    );
    table
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_arguments_need_an_input_and_accept_an_output() {
        let args = |a: &[&str]| Args::parse(&a.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>());
        let parsed = args(&["--input", "a.jsonl", "--out", "b.jsonl"]).unwrap();
        assert_eq!(parsed.input, PathBuf::from("a.jsonl"));
        assert_eq!(parsed.out, Some(PathBuf::from("b.jsonl")));
        assert!(args(&[]).is_err());
        assert!(args(&["--input"]).is_err());
        assert!(args(&["--input", "a", "--bogus", "b"]).is_err());
    }
}
