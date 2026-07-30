# `cargo-fuzz` targets

Coverage-guided fuzzing of the parsers that read **provider-controlled bytes**: the text and URL
helpers in `crates/adapters/src/html.rs`, the JSON-in-markup recovery path in
`crates/adapters/src/json.rs`, and the selector-driven extraction in
`crates/adapters/src/generic.rs`.

The value here is demonstrated rather than argued. The 2026-07 audit found two verified defects
in exactly this code — a panic on a non-ASCII chapter title (`TEST F-01`) and a quadratic DoS on
a hostile JSON body (`TEST F-02`) — and both are what these targets exist to catch. A third, an
*infinite* chapter number out of `parse_number`, was found while writing the oracle for
`adapters_generic_series_page`; it is fixed, with the story in
`crates/adapters/src/html.rs::tests::an_unrepresentable_digit_run_is_no_number_at_all`.

## Why this is its own workspace

`libfuzzer-sys` compiles the crate under test with `-Z sanitizer=address`, which is **nightly
only**. The host workspace pins `1.94.0` in `rust-toolchain.toml` and its `msrv` CI job builds at
`1.85`.

As a workspace member, this crate would put nightly and `libfuzzer-sys`/`arbitrary` into the path
of `cargo fmt --all`, `cargo clippy --workspace --all-targets`, `cargo deny check bans` (whose
`multiple-versions` is `deny` against a dated skip list) and the `--locked` resolution the `msrv`
and `supply-chain` jobs measure — for a crate that no gate runs. So it is listed under the root
`Cargo.toml`'s `exclude`, alongside `web/frontend`, which is outside for the same kind of reason
(it targets `wasm32`). It carries its own `rust-toolchain.toml` and its own, uncommitted lockfile.

**Nothing in `.github/workflows/ci.yml` runs these targets, deliberately.** A nightly toolchain in
a required check is a gate that breaks on somebody else's schedule, and a fuzz run's outcome
depends on how long it ran — neither is a property a blocking gate should have. If a job is ever
added it must be `continue-on-error: true` and time-boxed (`-max_total_time=60` per target for a
pull request, a `schedule:`d soak with the corpus persisted via `actions/cache` for the real
campaign), and it must report rather than fail.

## Running them

`cargo fuzz` locates `./fuzz` from the **repository root**, and the root `rust-toolchain.toml`
wins there, so the `+nightly` is not optional:

```bash
rustup toolchain install nightly

# 1. The text/URL helpers. Cheap and fast — expect >300k execs/minute.
cargo +nightly fuzz run adapters_html_parsers \
  fuzz/corpus/adapters_html_parsers fuzz/seeds/adapters_html_parsers \
  -- -max_total_time=60 -timeout=2 -rss_limit_mb=512 -dict=fuzz/dictionaries/html.dict

# 2. The JSON-in-markup recovery path. `-timeout` is the oracle, not a convenience: F-02 was a
#    body that parsed correctly and took ~30 s doing it, which no assertion can express.
cargo +nightly fuzz run adapters_json_body \
  fuzz/corpus/adapters_json_body fuzz/seeds/adapters_json_body \
  -- -max_total_time=60 -timeout=2 -rss_limit_mb=512 -dict=fuzz/dictionaries/json.dict

# 3. Selector extraction end to end through the shipped `manhuaus` preset. Two orders of
#    magnitude slower per exec — html5ever builds a document tree every iteration — so it gets a
#    looser timeout and more time.
cargo +nightly fuzz run adapters_generic_series_page \
  fuzz/corpus/adapters_generic_series_page fuzz/seeds/adapters_generic_series_page \
  crates/adapters/fixtures/manhuaus \
  -- -max_total_time=300 -timeout=5 -rss_limit_mb=1024 -dict=fuzz/dictionaries/html.dict
```

`cargo +nightly fuzz build` compiles all three without running them, which is the cheap check
that a change to `crates/adapters` has not broken a target.

### Argument order matters

libFuzzer reads inputs from **every** directory listed and writes newly-discovered ones back into
the **first**. That is why the working corpus is named first and `seeds/` second: `seeds/` is
curated and committed, `corpus/` is machine output and git-ignored. The `manhuaus` fixture
directory is passed last in target 3 for the same reason — it feeds the fuzzer the real markup
without duplicating those bytes into this tree, and libFuzzer must never be given it first.

### On Windows

The targets link the ASan runtime dynamically, so `clang_rt.asan_dynamic-x86_64.dll` has to be on
`PATH` or the binary dies with `STATUS_DLL_NOT_FOUND` (`0xc0000135`) before `main`. It ships with
the MSVC build tools:

```powershell
$msvc = Get-ChildItem "C:\Program Files*\Microsoft Visual Studio\*\*\VC\Tools\MSVC\*\bin\Hostx64\x64" |
        Select-Object -Last 1
$env:PATH = "$($msvc.FullName);$env:PATH"
```

## The targets

| Target | Under test | Oracle |
| --- | --- | --- |
| `adapters_html_parsers` | `html::{parse_chapter_number, parse_number, parse_year, parse_ymd_date, map_status, unescape_entities, split_attr, relativize, absolutize}` over arbitrary UTF-8 | No panic. These are total functions returning `Option`/`String`/an enum, so any abort is a bug. |
| `adapters_json_body` | `json::parse_json_body`, reached through the `#[doc(hidden)] __fuzz` seam | No panic **and** completion inside `-timeout=2` at `-rss_limit_mb=512`. |
| `adapters_generic_series_page` | `GenericConfigAdapter`'s four entry points against the `manhuaus` preset, fed malformed HTML through a fake `Fetcher` | `Ok` or a typed `AdapterError`; never a panic, never a hang. |

`parse_selector` is deliberately not fuzzed even though it is `pub` and takes provider-supplied
text: it writes into a process-wide bounded memo (PERF-5), so consecutive iterations would stop
being independent and a reproducer would depend on execution order. Its bound is pinned by a unit
test instead.

The remaining targets the audit designed — `solver_challenge_detection`, `auth_jwt_verify`,
`config_env_load` — are **not** implemented. The first is partly covered on stable already, by the
differential property in `crates/solver/tests/prop_detection.rs`; the other two are the audit's own
priority 3.

## Seeds, corpus, artifacts

- `seeds/` — committed, read-only starting inputs. Distilled rather than copied: the chapter
  labels, status captions, entity strings and href shapes that the extractors branch on, the three
  wrapped-JSON shapes `json.rs`'s module doc describes, and the degenerate markup no fixture
  contains (misnested anchors, 500-deep nesting, a brace storm). A seed going stale costs nothing
  — it is a starting point for mutation, not an assertion.
- `corpus/` — git-ignored working corpus; only the per-target directories are tracked, via
  `.gitkeep`, because libFuzzer errors out on a corpus directory that does not exist.
- `dictionaries/` — token lists, so the mutator does not have to assemble `</pre>` or
  `wp-manga-chapter` one byte at a time. `-dict=` takes one file; concatenate `json.dict` and
  `html.dict` when fuzzing the wrapped-JSON shapes hard.
- `artifacts/` — git-ignored crash reproducers.

## When a target finds something

Reproduce it, then **retire the artifact into a named regression test in the crate that owns the
bug** — not into this directory. That is what F-01, F-02 and the `parse_number` infinity each got:
a test whose doc comment says what the bug was and what it broke downstream, living next to the
code a future reader would otherwise "simplify". A committed crash file says only that something
was once wrong.

```bash
cargo +nightly fuzz run <target> fuzz/artifacts/<target>/<crash-file>   # replay one input
cargo +nightly fuzz tmin <target> fuzz/artifacts/<target>/<crash-file>  # shrink it first
```
