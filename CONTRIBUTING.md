# Contributing

**[`docs/ENGINEERING_GUIDE.md`](docs/ENGINEERING_GUIDE.md) is the canonical guide to structure,
style and security** — every rule, and what enforces it. This file covers workflow: what to run,
how to set up, and what a change needs. The two do not overlap by design.

## Before you push

```
cargo run -p xtask -- ci
```

That runs every gate CI runs that needs no Docker, no network and no database — `fmt`, pedantic
`clippy` with all features, the offline tests, the doc tests, the OpenAPI drift check, and the
three `web/frontend` gates — in CI's order, stopping at the first failure. It exists because the
alternative is reading `ci.yml` and replicating twenty-one jobs by hand, which is how
`cargo fmt --all --check` came to be red on `main` and stay red.

What it does **not** cover, and CI still does: the integration suites (Docker), the `sqlx`
offline-cache check (a live migrated Postgres), coverage, `cargo deny`/`audit`/supply-chain
(network), the image builds, the Tailwind rebuild (Node), the Prometheus rule tests, and secret
scanning. `xtask/src/ci.rs` says why each is excluded rather than pretending the list is
complete.

Install the pre-commit hook once, which regenerates the OpenAPI artifacts when the schema
changes:

```
cargo run -p xtask -- install-hooks
```

## What CI runs on your pull request

Not all of it, and that is deliberate. A `changes` job diffs your branch against its base and
publishes a set of booleans; a job whose inputs provably did not change is **skipped**, not run to
a foregone conclusion. A documentation-only change does not build eight container images.

Three things about that are worth knowing before you read a run:

- **A skipped job is a pass, not a gap.** `.github/workflows/ci.yml` is an input to every filter,
  so touching the workflow re-runs everything — and a weekly scheduled run ignores the filters
  entirely, which is what keeps a gate from rotting because nothing happened to touch its paths.
- **`CI` is the only required status check.** It is the aggregate job at the bottom of `ci.yml`;
  it fails if anything failed and passes when things were skipped. Do not add the individual jobs
  to branch protection — a required check that never reports is pending forever, so every skipped
  job would block its own pull request.
- **Pull requests build two images, not eight.** Seven of the eight are the same Dockerfile path
  with a different `BIN`; `render` is the one with a different runtime stage. The full matrix
  builds on `main` and on the weekly run.

To force the whole suite from a branch, run the workflow manually with the `full` input set
(**Actions → CI → Run workflow**). `cargo mutants` runs only that way — it is advisory
(`continue-on-error`), so per-pull-request it was 45 minutes producing a report to triage later.

Dependency updates come from Renovate (`renovate.json`), which opens grouped pull requests that
go through the same gates as everyone else's.

## Local setup

```
docker compose -f deploy/docker-compose.yml up --build      # the whole stack, app on :3000
```

Secrets are required rather than defaulted — copy `deploy/local.env.example` to
`deploy/local.env`, fill it in, and pass `--env-file deploy/local.env`. The published
placeholders are refused at boot in every profile, deliberately: a working default here would
only move the failure later.

For a database without the stack:

```
export DATABASE_URL=postgres://tankovault:tankovault@localhost:5432/tankovault
cargo run -p xtask -- migrate
cargo run -p xtask -- seed
```

`cargo run -p xtask -- reset` drops and recreates the `public` schema and needs
`TANKOVAULT_CONFIRM_RESET=1`. It is local-development only, and the guard exists because a
mis-pointed `DATABASE_URL` is otherwise indistinguishable from an intentional wipe.

Integration suites need Docker:

```
cargo test -p tankovault-db -p tankovault-api -p tankovault-sync --features integration
```

They reuse one container named `tankovault-test-postgres` and sweep leftover `tv_test_*`
databases older than an hour. Each test binary pays ~85 seconds in migrations, so a full run is
around fifteen minutes — that is expected, not a hang. `docker rm -f tankovault-test-postgres`
starts clean.

## What changes need

**Generated artifacts.** `openapi.json` and `crates/api-client/src/lib.rs` are committed and
generated. Change a handler's `#[utoipa::path]` or a `crates/contracts` type and run
`cargo run -p xtask -- openapi`; `--check` is a CI gate. Never edit either by hand.

**Configuration.** `docs/CONFIGURATION.md` is the surface — every `TANKOVAULT_*` key, its
default, and which services read it. Add, rename or retire a config field and the document has
to follow: a test derives the surface from the config structs and the `std::env::var` call sites
and fails on a disagreement in either direction. An unknown key is *ignored* at boot rather than
rejected, so without that test a stale row costs an operator a silent no-op. Keys are read from
the leftmost cell of a table row only; `cargo run -p xtask -- config-docs` prints the derived
list.

**SQL.** The `query!` macros are checked against the committed `.sqlx/` cache. Change any query
*text* and run `cargo run -p xtask -- sqlx-prepare` against a migrated Postgres 17. Moving a
query between files needs nothing — the cache is keyed on the text, not the location. The whole
workspace must compile with `DATABASE_URL` unset; that is what proves the cache is complete.

**Suppressions.** `#[expect(..., reason = "...")]`, never `#[allow]` — enforced by
`clippy::allow_attributes`. A suppression is a claim about the code beneath it, and an `expect`
warns when the claim stops holding while an `allow` never does. Say *why* it is sound, not what
is suppressed; the lint name already says that.

**Tests.** A fix that could silently come back gets a test whose doc comment says what the bug
was, so a future reader does not simplify it away. The `docs/audit/PROGRESS.md` rows are full of
worked examples.

**Coverage.** The `coverage` job compares against `.github/coverage-floor.txt` and fails below
it. The file explains when to raise it and the one legitimate reason to lower it.

**Mutation testing.** `cargo install cargo-mutants --locked`, then `cargo mutants` — about nine
minutes on a warm `target/`. Scoped by `.cargo/mutants.toml` to the four pure decision cores
(the matcher, the feature gate, the sync merge and its plan), which is where the signal is clean
and the run is cheap; the file argues the scope and lists the mutants excluded and why. Advisory:
the CI job is `continue-on-error`, and a survivor is triaged as a missing assertion rather than as
a build failure. Worth running after touching any of those four — the first run reported 43
survivors, and every one sat under a passing test that compared an *ordering* rather than a value.

**Dependencies.** `cargo deny` denies duplicate versions against an explicit skip list, and
`-D unnecessary-skip` fails on a skip whose duplicate has since resolved — so a pull request that
collapses one deletes its line. Adding a dependency to `services/api` needs a look at
`cargo tree -p tankovault-api -i boring-sys2`: that binary deliberately links one TLS stack.

## Conventions worth knowing

These are summarised, with their enforcement, in
[`docs/ENGINEERING_GUIDE.md`](docs/ENGINEERING_GUIDE.md);
`docs/audit/PROGRESS.md` ends with the conventions this codebase settled on and the reasoning
behind each — module splits and their glob re-exports, `citext` comparisons, the single RFC 9457
error shape, where shared policy lives, and why a predicate that must exist in several places
gets a differential test rather than a comment. Read that section before adding to any of them.

Two that come up constantly and are easy to miss:

- **Database fixtures come from `tankovault_test_support::seed`** —
  `seed::provider(&db, "alpha").create().await`, `seed::user`, `seed::series(…).chapters(&[…])`.
  Do not write another `a_provider`; there were seven, six of them byte-identical, and the
  seventh had quietly diverged. Override what your test is actually about
  (`.adapter(…)`, `.release_year(…)`) so the divergence is stated rather than inferred.
- **Every public `fn` returning `Result` needs a `# Errors` section**, and the lint enforces it.
  Name the variants the function can actually produce — in `crates/db` that is usually
  "`DbError::Sqlx` only — no other variant is reachable", which tells a caller the answer is
  always a 500 — and then say what it returns *instead of* an error, because most of this
  codebase turns a miss into `Ok(None)`/`Ok(false)`/`Ok(0)` and several of those choices are
  security-relevant. "Returns an error if the query fails" satisfies the lint and documents
  nothing.

## Commit messages

Conventional-commit prefixes (`feat`, `fix`, `refactor`, `docs`, `test`, `ci`, `chore`), and a
body that says *why*. The bodies in this repository's history are long on purpose: the
non-obvious part of most changes here is what was wrong before, and that is not recoverable from
the diff.
