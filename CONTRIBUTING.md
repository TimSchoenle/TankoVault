# Contributing

**[`docs/ENGINEERING_GUIDE.md`](docs/ENGINEERING_GUIDE.md) is the canonical guide to structure,
style and security** — every rule, and what enforces it. This file covers workflow: setup, what to
run, and what a change needs. The two do not overlap by design.

## Licensing of contributions

Read this before opening a pull request. It is short, and it asks for something real.

TankoVault is [PolyForm Noncommercial 1.0.0](LICENSE): free for any noncommercial use, commercial
use only by separate licence from the maintainer. That model only works if one party owns enough
of the copyright to grant those licences — so **by opening a pull request you agree to both of
the following**:

1. Your contribution is licensed to everyone under the project licence, and
2. you grant the maintainer a perpetual, worldwide, irrevocable, royalty-free and
   **sublicensable** right to use, modify, sublicense and relicense it, including under
   commercial terms and including in a paid hosted service.

Said plainly, because it is better said than discovered later: **you are giving the maintainer
the right to make money from your work, on terms you do not get.** That is the deal. It is the
same one Grafana, MinIO and Qt ask for, and if it is not one you want to accept, please open an
issue describing the change instead of a pull request implementing it — a good bug report is
worth as much here and costs you nothing.

Every commit also needs a [Developer Certificate of Origin](https://developercertificate.org/)
sign-off, which is your assertion that you had the right to submit the code in the first place:

```
git commit -s -m "fix: ..."
```

Contributions carrying someone else's copyright — code lifted from another project, output you
are not free to relicense — cannot be accepted, whatever the upstream licence, because clause 2
is not yours to grant for them.

## Before you push

```
cargo run -p xtask -- ci
```

Every gate CI runs that needs no Docker, network or database — `fmt`, pedantic `clippy` with all
features, offline tests, doc tests, the OpenAPI drift check, and the three `web/frontend` gates —
in CI's order, stopping at the first failure.

It does **not** cover, and CI still does: the integration suites (Docker), the `sqlx` offline-cache
check (a live migrated Postgres), coverage, `cargo deny`/`audit`/supply-chain (network), the image
builds, the Tailwind rebuild (Node), the Prometheus rule tests, and secret scanning.
`xtask/src/ci.rs` says why each is excluded.

Install the pre-commit hook once — it regenerates the OpenAPI artifacts when the schema changes:

```
cargo run -p xtask -- install-hooks
```

## What CI runs on your pull request

A `changes` job diffs your branch against its base and publishes booleans; a job whose inputs
provably did not change is skipped. Three things to know before reading a run:

- **A skipped job is a pass, not a gap.** `.github/workflows/ci.yml` is an input to every filter, so
  touching the workflow re-runs everything, and a weekly scheduled run ignores the filters entirely.
- **`CI` is the only required status check** — the aggregate job at the bottom of `ci.yml`. Do not
  add individual jobs to branch protection: a required check that never reports is pending forever,
  so every skipped job would block its own pull request.
- **Pull requests build two images, not eight.** The full matrix builds on `main` and weekly.

Force the whole suite from a branch with the `full` input (**Actions → CI → Run workflow**).
`cargo mutants` runs only that way. Dependency updates come from Renovate (`renovate.json`).

## Local setup

```
docker compose -f deploy/docker-compose.yml up --build      # the whole stack, app on :3000
```

Copy `deploy/local.env.example` to `deploy/local.env`, fill it in, and pass
`--env-file deploy/local.env`. The published placeholders are refused at boot in every profile,
deliberately — a working default would only move the failure later.

For a database without the stack:

```
export DATABASE_URL=postgres://tankovault:tankovault@localhost:5432/tankovault
cargo run -p xtask -- migrate
cargo run -p xtask -- seed
```

`cargo run -p xtask -- reset` drops and recreates the `public` schema and needs
`TANKOVAULT_CONFIRM_RESET=1`. Local development only: a mis-pointed `DATABASE_URL` is otherwise
indistinguishable from an intentional wipe.

Integration suites need Docker:

```
cargo test -p tankovault-db -p tankovault-api -p tankovault-sync --features integration
```

They reuse one container named `tankovault-test-postgres` and sweep leftover `tv_test_*` databases
older than an hour. Each test binary pays ~85 seconds in migrations, so a full run is around
fifteen minutes — expected, not a hang. `docker rm -f tankovault-test-postgres` starts clean.

## What changes need

**Generated artifacts.** `openapi.json` and `crates/api-client/src/lib.rs` are committed and
generated. Change a handler's `#[utoipa::path]` or a `crates/contracts` type and run
`cargo run -p xtask -- openapi`; `--check` is a CI gate. Never edit either by hand.

**Configuration.** `docs/CONFIGURATION.md` is the surface — every `TANKOVAULT_*` key, its default,
and which services read it. Add, rename or retire a field and the document has to follow: a test
derives the surface from the config structs and the `std::env::var` call sites and fails on a
disagreement either way. An unknown key is *ignored* at boot rather than rejected, so a stale row
costs an operator a silent no-op. Keys are read from the leftmost cell of a table row only;
`cargo run -p xtask -- config-docs` prints the derived list.

**SQL.** The `query!` macros are checked against the committed `.sqlx/` cache. Change any query
*text* and run `cargo run -p xtask -- sqlx-prepare` against a migrated Postgres 17. Moving a query
between files needs nothing — the cache is keyed on text, not location. The workspace must compile
with `DATABASE_URL` unset; that is what proves the cache is complete.

**Suppressions.** `#[expect(..., reason = "...")]`, never `#[allow]`, enforced by
`clippy::allow_attributes`. An `expect` warns when its claim stops holding; an `allow` never does.
Say why it is sound, not what is suppressed.

**Tests.** A fix that could silently come back gets a test whose doc comment says what the bug was,
so a future reader does not simplify it away.

**Coverage.** The `coverage` job compares against `.github/coverage-floor.txt` and fails below it.
That file explains when to raise it and the one legitimate reason to lower it.

**Mutation testing.** `cargo install cargo-mutants --locked`, then `cargo mutants` — about nine
minutes on a warm `target/`. Scoped by `.cargo/mutants.toml` to the four pure decision cores (the
matcher, the feature gate, the sync merge and its plan), which argues the scope. Advisory: a
survivor is a missing assertion, not a build failure. Worth running after touching any of the four.

**Dependencies.** `cargo deny` denies duplicate versions against an explicit skip list, and
`-D unnecessary-skip` fails on a skip whose duplicate has resolved. Adding a dependency to
`services/api` needs a look at `cargo tree -p tankovault-api -i btls-sys`: that binary deliberately
links one TLS stack.

## Conventions worth knowing

Summarised with their enforcement in
[`docs/ENGINEERING_GUIDE.md`](docs/ENGINEERING_GUIDE.md). Two that come up constantly:

- **Database fixtures come from `tankovault_test_support::seed`** —
  `seed::provider(&db, "alpha").create().await`, `seed::user`, `seed::series(…).chapters(&[…])`.
  Do not write another `a_provider`. Override what your test is actually about (`.adapter(…)`,
  `.release_year(…)`) so the divergence is stated rather than inferred.
- **Every public `fn` returning `Result` needs a `# Errors` section**, one line, naming the variants
  it can actually produce and what it returns *instead of* an error — several of those
  `Ok(None)`/`Ok(false)`/`Ok(0)` choices are security-relevant. See §3.2.

## Commit messages

Conventional-commit prefixes (`feat`, `fix`, `refactor`, `docs`, `test`, `ci`, `chore`) and a body
that says *why*.

The prefix is load-bearing: release-please derives the version bump and changelog from it. `feat`
gives a minor, `fix` a patch, and a `feat!` or `BREAKING CHANGE:` footer gives a major. A change
released under the wrong prefix cannot be corrected without a new release.

## Releases

You do not cut one by hand. Merging to `main` maintains a release pull request; merging *that* tags
the repository and publishes nine multi-architecture images. Before touching `.github/workflows` or
`deploy/docker`:

- `Cargo.lock` is synced automatically on every pull request by `update-lockfile.yaml`. Expect a bot
  commit called "sync Cargo.lock with the workspace manifests"; do not revert it.
- The release build compiles natively for `linux/arm64` as well as `amd64`. Anything you add to the
  Dockerfile that names an architecture breaks half of it, and the amd64 CI leg will not tell you.
  `deploy/docker/Dockerfile`'s sysroot staging is the pattern to follow.

[`docs/RELEASING.md`](docs/RELEASING.md) has the full flow, the required secrets and the reasoning
behind the release-please configuration.
