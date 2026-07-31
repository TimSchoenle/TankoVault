# `cargo-fuzz` targets

Coverage-guided fuzzing of the code that reads **bytes somebody else chose**. Six targets, all
six of the ones the audit designed, in two groups:

- **Provider-controlled bytes** — the text and URL helpers in `crates/adapters/src/html.rs`, the
  JSON-in-markup recovery path in `crates/adapters/src/json.rs`, the selector-driven extraction
  in `crates/adapters/src/generic.rs`, and the challenge classifier in
  `crates/solver/src/detection.rs` that runs on every response before any of them.
- **Credential- and operator-controlled bytes** — `tankovault_auth::verify_access_token`, which
  is the whole of the API's authentication, and `tankovault_config::load`, the environment
  reader every service boots through.

The value here is demonstrated rather than argued. The 2026-07 audit found two verified defects
in exactly this code — a panic on a non-ASCII chapter title (`TEST F-01`) and a quadratic DoS on
a hostile JSON body (`TEST F-02`) — and both are what these targets exist to catch. A third, an
*infinite* chapter number out of `parse_number`, was found while writing the oracle for
`adapters_generic_series_page`; it is fixed, with the story in
`crates/adapters/src/html.rs::tests::an_unrepresentable_digit_run_is_no_number_at_all`.

Three of the six carry oracles beyond "no panic", and an oracle behind an `if` is an oracle that
can pass without ever running. Each was therefore **verified in the failing direction** — the
assertion inverted, one seed replayed, the panic observed — rather than trusted because a
campaign came back green:

| Target | Mutation | Seed that caught it |
| --- | --- | --- |
| `solver_challenge_detection` | differential flipped to `is_none()` | `turnstile-widget` |
| `auth_jwt_verify` | header pinned to `HS384` | `valid-hs256` |
| `config_env_load` | refresh clamp raised to an hour | `minimal-valid` |

`auth_jwt_verify`'s is the one worth reading: the inverted assertion failed with *"a token
verified while its own header declared HS256"*, which says in one line that a token really did
verify, that its header really was decoded, and that the comparison really ran.

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

# 4. The challenge classifier. The cheapest target here — two substring scans and a byte-boundary
#    walk — so it reaches millions of executions a minute and is the one to give a long soak.
cargo +nightly fuzz run solver_challenge_detection \
  fuzz/corpus/solver_challenge_detection fuzz/seeds/solver_challenge_detection \
  -- -max_total_time=60 -timeout=2 -rss_limit_mb=512 -dict=fuzz/dictionaries/solver.dict

# 5. Access-token verification.
cargo +nightly fuzz run auth_jwt_verify \
  fuzz/corpus/auth_jwt_verify fuzz/seeds/auth_jwt_verify \
  -- -max_total_time=60 -timeout=2 -rss_limit_mb=512

# 6. Environment loading. Two orders of magnitude slower than anything else here and the reason
#    is structural, not a bug to fix: each execution builds a `figment::Jail` — a temp directory,
#    a working-directory swap and a set/restore of every `TANKOVAULT_*` variable — so the harness
#    dominates. Budget minutes, not seconds, and do not read its exec count next to target 4's.
cargo +nightly fuzz run config_env_load \
  fuzz/corpus/config_env_load fuzz/seeds/config_env_load \
  -- -max_total_time=300 -timeout=5 -rss_limit_mb=512
```

`cargo +nightly fuzz build` compiles all six without running them, which is the cheap check that
a change to a crate under test has not broken a target.

### Argument order matters

libFuzzer reads inputs from **every** directory listed and writes newly-discovered ones back into
the **first**. That is why the working corpus is named first and `seeds/` second: `seeds/` is
curated and committed, `corpus/` is machine output and git-ignored. The `manhuaus` fixture
directory is passed last in target 3 for the same reason — it feeds the fuzzer the real markup
without duplicating those bytes into this tree, and libFuzzer must never be given it first.

### On Windows

Two things have to be on `PATH`, and neither failure names itself.

**`clang_rt.asan_dynamic-x86_64.dll`** — the targets link the ASan runtime dynamically, so
without it the binary dies with `STATUS_DLL_NOT_FOUND` (`0xc0000135`) before `main`.

**`cmake.exe`** — every target pays for it, including the three that touch no HTTP at all.
`tankovault-fetch` is a dependency of this *package*, and Cargo resolves dependencies per package
rather than per binary, so `--bin auth_jwt_verify` still builds `wreq` → `boring-sys2`, which
compiles BoringSSL from C and assembly through cmake. Without it the build fails with
`failed to execute command: program not found / is cmake not installed?` attributed to a build
script, several hundred lines into output that is otherwise about patching BoringSSL. It is not
a separate install: it ships with the build tools, one directory over from the ASan runtime, and
is simply not on `PATH` by default.

```powershell
$vc = Get-ChildItem "C:\Program Files*\Microsoft Visual Studio\*\*\VC\Tools\MSVC\*\bin\Hostx64\x64" |
      Select-Object -Last 1
$cmake = Get-ChildItem "C:\Program Files*\Microsoft Visual Studio\*\*\Common7\IDE\CommonExtensions\Microsoft\CMake\CMake\bin" |
         Select-Object -Last 1
$env:PATH = "$($vc.FullName);$($cmake.FullName);$env:PATH"
```

The first BoringSSL build is a few minutes; after that it is cached in `fuzz/target/`. Note that
this crate's lockfile is **not** committed (see `.gitignore`), so adding a dependency here
re-resolves the whole graph and can invalidate that cache — which is exactly when somebody meets
the cmake error for the first time and reads it as a fuzzing problem.

## The targets

| Target | Under test | Oracle |
| --- | --- | --- |
| `adapters_html_parsers` | `html::{parse_chapter_number, parse_number, parse_year, parse_ymd_date, map_status, unescape_entities, split_attr, relativize, absolutize}` over arbitrary UTF-8 | No panic. These are total functions returning `Option`/`String`/an enum, so any abort is a bug. |
| `adapters_json_body` | `json::parse_json_body`, reached through the `#[doc(hidden)] __fuzz` seam | No panic **and** completion inside `-timeout=2` at `-rss_limit_mb=512`. |
| `adapters_generic_series_page` | `GenericConfigAdapter`'s four entry points against the `manhuaus` preset, fed malformed HTML through a fake `Fetcher` | `Ok` or a typed `AdapterError`; never a panic, never a hang. |
| `solver_challenge_detection` | `detection::{detect_challenge, detect_challenge_body, is_rate_limit_page}` over arbitrary UTF-8 | No panic, **plus** two differentials: the narrow body-only classifier never accepts what the envelope-aware one rejects, and a rate-limit notice never buys a solve. |
| `auth_jwt_verify` | `verify_access_token`, then `AccessClaims::user_id` | No panic, **plus** a token that verifies must carry `alg: HS256` in its own header and must not verify under a second secret. |
| `config_env_load` | `tankovault_config::load` over the union of every published config block, then the post-parse accessors | `Ok` or a `ConfigError`, never a panic, **plus** the internal-token length floor and the feature-refresh clamp hold over values that came through env parsing. |

`parse_selector` is deliberately not fuzzed even though it is `pub` and takes provider-supplied
text: it writes into a process-wide bounded memo (PERF-5), so consecutive iterations would stop
being independent and a reproducer would depend on execution order. Its bound is pinned by a unit
test instead.

Every target the audit designed is implemented. Two of them overlap deliberately with cheaper
gates, and the overlap is the point rather than a duplication to remove:

- `solver_challenge_detection` asserts the same two differentials as
  `crates/solver/tests/prop_detection.rs`, which runs on stable in the normal `test` job. What it
  adds is **reach**: proptest's `".*"` expands to roughly 32 characters, so the stable suite
  cannot generate a body long enough to exercise the 4096-byte `TITLE_SCAN_BYTES` cut at all and
  needs a hand-written `"€".repeat(1350)` case to touch it — pinning one length rather than the
  boundary. It also builds marker-bearing bodies from a **fixed fragment list**, so it can only
  find a disagreement assembled from fragments somebody already thought of. Because the
  assertions are the same, anything found here is reproducible there, which is where the
  regression test should land.
- `auth_jwt_verify` sits over `jsonwebtoken`, which is not our parser. It is not looking for a
  crash in that library; it is looking at the two things this repository adds around it — the
  pinned algorithm, and `Uuid::from_str` on a `sub` the token's holder chose.

Measured, on `nightly-x86_64-pc-windows-msvc` with `cargo-fuzz 0.13.2`, at the invocations above
(2026-07-31): **7,167,430** / **1,781,078** / **16,774** executions for targets 4, 5 and 6, and
**406,162** / **124,415** / **17,538** for 1, 2 and 3. No crash, no timeout, no artifact. The
three-order-of-magnitude spread is harness cost, not code under test — see the note on
`config_env_load` above.

## Seeds, corpus, artifacts

- `seeds/` — committed, read-only starting inputs. Distilled rather than copied: the chapter
  labels, status captions, entity strings and href shapes that the extractors branch on, the three
  wrapped-JSON shapes `json.rs`'s module doc describes, and the degenerate markup no fixture
  contains (misnested anchors, 500-deep nesting, a brace storm). A seed going stale costs nothing
  — it is a starting point for mutation, not an assertion.

  The three newer sets follow the same rule and each has one detail worth knowing.
  `solver_challenge_detection` carries every marker the classifier branches on plus the two cases
  the fragment list alone cannot express: a title *past* the 4096-byte scan cut, and three-byte
  characters positioned so the cut lands inside one. `auth_jwt_verify` holds tokens minted against
  the secret named in the target, mutated the ways that matter — `alg` swapped to `none`/`RS256`/
  `HS384`, signature stripped, payload edited without re-signing, `sub` made non-UUID. Their `exp`
  is deliberately in the year 2286: an expiry baked into a committed file would eventually pass,
  every "this one verifies" seed would silently become a rejection seed, and three of that
  target's four oracles would stop being reachable with nothing to say so. `config_env_load`'s are
  plain `KEY=VALUE` lines — the audit sketched `arbitrary::Arbitrary` over `Vec<(String, String)>`,
  which would have made every seed an opaque blob in a directory whose whole convention is that
  seeds are readable.
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
