# TankoVault — working agreement

Read [`docs/ENGINEERING_GUIDE.md`](docs/ENGINEERING_GUIDE.md). It is the canonical guide to
structure, style and security, and it labels every rule with what enforces it. This file is the
short version, kept here because it is what gets loaded first.

## The definition of done

`cargo run -p xtask -- ci` is still the gate a change has to pass — every offline gate CI runs,
in CI's order. It is **not** what you run by default. One pass rebuilds the workspace under
`--all-features` and `wasm32-unknown-unknown`, runs `test --workspace`, the doc tests, rustdoc
and the three `web/frontend` gates; that is minutes per iteration, and spending it after every
edit buys nothing CI will not report anyway.

Default while working — the whole obligation:

```
cargo check -p <the crate you touched>
```

Do **not** run `xtask ci`, `cargo test --workspace`, workspace clippy, the doc-test or rustdoc
gates, or the `web/frontend` gates on your own initiative. Run them when, and only when:

- the user asks for the full gate, or asks you to commit or push;
- the user asks you to verify a change end to end;
- you are chasing a failure those gates already reported, in which case run *that* gate, scoped
  as narrowly as it will go (`cargo test -p <crate> <test_name>`), not the suite around it.

"It compiles" is not done — but it is as far as *you* take it unless told otherwise. What the
remaining gates cost is the user's call to spend, not yours.

Regeneration is the exception and stays mandatory, because the artefacts are committed and
hand-editing them is banned (rule 6): OpenAPI surface changed → `cargo run -p xtask -- openapi`;
a `query!`/`query_as!` changed → `cargo run -p xtask -- sqlx-prepare` against a migrated
database; config surface changed → update `docs/CONFIGURATION.md` (`cargo run -p xtask --
config-docs` prints the current surface); either `Cargo.lock` moved → `cargo run -p xtask --
notices`, which needs `cargo-about` installed. The gates check these, they do not fix them.

## What `xtask ci` cannot tell you

`xtask ci` runs the **offline** gates only — no Docker, no database. CI runs more, and that gap
is where a green local run still turns the pull request red.

The one that actually bites: **a published endpoint you added, renamed or removed.**
`services/api/tests/me_access_matrix.rs` and `admin_access_matrix.rs` reconcile the OpenAPI
document against their access-control tables, so a route nobody classified fails with the
operation id in the message. Writing the handler and regenerating `openapi.json` is *not*
enough — the route also needs a row in `me_gates()`, `public_gates()` or `covered_elsewhere()`
(that last one carries the reason and where it is covered instead). Both suites need Docker, so
no offline gate mentions it:

```
cargo test -p tankovault-api --features integration --test me_access_matrix
```

The rule generalises: run the Docker-gated suite your change *touches*, not just the one you
wrote. A new `query!` is `repo_query_plans` (an `EXPLAIN` sweep with a cost ceiling); a new
`.gate(…)` is `feature_gating`; a fifth copy of the unread predicate is `repo_tracking`'s
differential. CI's own command is the full set:

```
cargo test -p tankovault-db -p tankovault-api -p tankovault-sync --features integration
```

## Ten rules you will otherwise break

1. **Never widen a Content-Security-Policy to make code work.** Change the code. The SPA's access
   token is in memory; the CSP is the ceiling on where an injected script could send it.
2. **Never call `document::eval` in the frontend** — it is `new Function(…)`, the served CSP
   blocks it, and the failure *aborts the WASM instance* rather than returning an error. Add a
   typed wrapper to `web/frontend/src/browser.rs`. Banned in `web/frontend/clippy.toml`.
3. **`#[expect(…, reason = "…")]`, never `#[allow]`.** An `expect` warns when its claim stops
   holding; an `allow` never does.
4. **Comments are short by default.** One summary line of rustdoc per public item; a module `//!`
   says what the module owns in a sentence or two. An inline `//` only where correct-looking code
   would otherwise be misread. No restating the code, no numbered walkthroughs of the lines below,
   no history prose. `# Errors`/`# Panics` stay — one line, naming the reachable variants.
   **Go long only for** (a) security rationale, (b) an invariant or ordering constraint the next
   refactor would silently break, (c) a test doc comment naming the bug it pins (rule 8). When
   trimming, keep the sentence that carries the risk and drop the narration around it.
5. **Do not simplify a test you do not understand.** If its doc comment describes a bug, it is
   there to stop that bug returning.
6. **Never hand-edit generated files** — `openapi.json`, `crates/api-client/src/lib.rs`,
   `THIRD-PARTY-NOTICES`. Run `cargo run -p xtask -- openapi` / `-- notices`.
7. **`web/frontend` is a separate workspace and inherits nothing** — not lints, not `clippy.toml`.
   A frontend URL and the API struct behind it have no compile-time relationship; `openapi.json`
   is the only connector.
8. **A fix that could silently come back gets a test whose doc comment says what the bug was.**
9. **Every secret value is a `secrecy` type — never a `String`, `&str` or `Vec<u8>`.**
   `SecretString` for text (DSNs, broker URLs, tokens, passwords, the pepper, webhook URLs with an
   embedded token), `SecretSlice<u8>` for key material, `Arc<…>` when it lives in an `AppState`
   that axum clones per request. Reading one is an explicit `expose_secret()`, and `rg
   expose_secret` is meant to stay a short, justifiable list. Do **not** implement
   `SerializableSecret`: a value that must go on the wire opts in at that one field with
   `#[serde(serialize_with = "crate::secret::expose_onto_wire")]`. A wrapped API DTO field needs
   `#[schema(value_type = String)]` so `openapi.json` stays byte-identical, and its rationale goes
   in a `//` comment — utoipa publishes `///` as the public `description`. Values that are *not*
   secrets (a PHC hash, a token digest, ciphertext) keep their plain types on purpose.
   Full table and the two deliberate exceptions: `docs/ENGINEERING_GUIDE.md` §2.2.
10. **Every commit message is a Conventional Commit** — `type(scope): subject`, always, with no
    exceptions for a one-line fix. Types in use: `feat`, `fix`, `docs`, `refactor`, `test`,
    `perf`, `build`, `ci`, `chore`. The scope is the crate or surface the change owns
    (`console`, `api`, `db`, `sync`, `deps`, …). The subject is imperative, lower-case and
    unpunctuated. A breaking change says so with `!` before the colon and a `BREAKING CHANGE:`
    footer. Release-please reads these to cut the changelog and pick the next version, so a
    mistyped type silently mis-versions the release rather than failing anything.

## When a gate fails

Read the rule's doc comment before changing the rule. Every entry in `clippy.toml`,
`web/frontend/clippy.toml` and `xtask/src/repo_lint/` carries its reason inline, and most of
them exist because the shortcut you are about to take was taken once already.

`cargo run -p xtask -- repo-lint` is the one that checks invariants no compiler sees: a CSP and
the HTML it governs, a secret published in the compose file and the code that refuses it.

## Reporting

Say what actually happened. If a suite fails, quote it. If you skipped a step, name it. If part
of the task is blocked, finish the rest and say what you left.

Verification is deliberately scoped now, so it has to be reported as scoped: name the command
you actually ran and state that the full gate was not run — "`cargo check -p tankovault-api`
passed; `xtask ci` not run". Never let a type-check be read as a green CI run.
