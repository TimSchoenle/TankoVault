# TankoVault — working agreement

Read [`docs/ENGINEERING_GUIDE.md`](docs/ENGINEERING_GUIDE.md). It is the canonical guide to
structure, style and security, and it labels every rule with what enforces it. This file is the
short version, kept here because it is what gets loaded first.

## The definition of done

```
cargo run -p xtask -- ci
```

Every offline gate CI runs, in CI's order. "It compiles" is not done. If you changed the OpenAPI
surface, the config surface or a SQL query, regenerate before running it — the gates check, they
do not fix.

## Eight rules you will otherwise break

1. **Never widen a Content-Security-Policy to make code work.** Change the code. The SPA's access
   token is in memory; the CSP is the ceiling on where an injected script could send it.
2. **Never call `document::eval` in the frontend** — it is `new Function(…)`, the served CSP
   blocks it, and the failure *aborts the WASM instance* rather than returning an error. Add a
   typed wrapper to `web/frontend/src/browser.rs`. Banned in `web/frontend/clippy.toml`.
3. **`#[expect(…, reason = "…")]`, never `#[allow]`.** An `expect` warns when its claim stops
   holding; an `allow` never does.
4. **Comments say *why*, and usually *what was wrong before*.** This codebase is heavily
   commented by choice. Do not strip rationale to tidy up; match the density of the file.
5. **Do not simplify a test you do not understand.** If its doc comment describes a bug, it is
   there to stop that bug returning.
6. **Never hand-edit generated files** — `openapi.json`, `crates/api-client/src/lib.rs`. Run
   `cargo run -p xtask -- openapi`.
7. **`web/frontend` is a separate workspace and inherits nothing** — not lints, not `clippy.toml`.
   A frontend URL and the API struct behind it have no compile-time relationship; `openapi.json`
   is the only connector.
8. **A fix that could silently come back gets a test whose doc comment says what the bug was.**

## When a gate fails

Read the rule's doc comment before changing the rule. Every entry in `clippy.toml`,
`web/frontend/clippy.toml` and `xtask/src/repo_lint.rs` carries its reason inline, and most of
them exist because the shortcut you are about to take was taken once already.

`cargo run -p xtask -- repo-lint` is the one that checks invariants no compiler sees: a CSP and
the HTML it governs, a secret published in the compose file and the code that refuses it.

## Reporting

Say what actually happened. If a suite fails, quote it. If you skipped a step, name it. If part
of the task is blocked, finish the rest and say what you left.
