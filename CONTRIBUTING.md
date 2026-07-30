# Contributing

## Before you push

```
cargo run -p xtask -- ci
```

That runs every gate CI runs that needs no Docker, no network and no database — `fmt`, pedantic
`clippy` with all features, the offline tests, the doc tests, the OpenAPI drift check, and the
three `web/frontend` gates — in CI's order, stopping at the first failure. It exists because the
alternative is reading `ci.yml` and replicating eighteen jobs by hand, which is how
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

`docs/audit/PROGRESS.md` ends with the conventions this codebase settled on and the reasoning
behind each — module splits and their glob re-exports, `citext` comparisons, the single RFC 9457
error shape, where shared policy lives, and why a predicate that must exist in several places
gets a differential test rather than a comment. Read that section before adding to any of them.

## Commit messages

Conventional-commit prefixes (`feat`, `fix`, `refactor`, `docs`, `test`, `ci`, `chore`), and a
body that says *why*. The bodies in this repository's history are long on purpose: the
non-obvious part of most changes here is what was wrong before, and that is not recoverable from
the diff.
