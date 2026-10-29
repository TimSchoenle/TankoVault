# Replay fixtures

One exported `merge_decisions` row per line, in the shape of query A10 in
[`docs/MATCHING_OVERHAUL.md`](../../../../docs/MATCHING_OVERHAUL.md). `tests/replay.rs` and
`cargo run -p xtask -- matching-replay --input <file>` both read them. Titles and facts only: no
ids that identify users. `case` is a fixture-only field naming the production case a row stands
for; the export does not carry it and the reader ignores it when absent.

| File | Holds | Must replay as |
|---|---|---|
| `confirmed_same.jsonl` | merges known to be correct | `auto` when the stored signals carry a primary identity; never `distinct` |
| `confirmed_wrong.jsonl` | merges known to be wrong | never `auto` |

## Provenance

Both files are currently **hand-built** from the cases named in the overhaul plan, with facts
shaped like the production evidence (guards off, metadata agreeing). They are a seed, not the
corpus the plan specifies:

- `confirmed_same.jsonl` is meant to be the `live_agree` merges of query A9 (3,465 rows on
  2026-10-19). Export them by joining A9's `cls` (with `verdict = 'live_agree'`) to A10 on `id`,
  and replace the file.
- `confirmed_wrong.jsonl` is meant to grow with every wrong merge confirmed by hand during the
  undo campaign (WP8c). Append rows as they are confirmed; do not edit the existing ones.

A row's stored `signals` must be what the matcher of the time recorded: the replay test reads
them to decide which correct merges must stay automatic.
