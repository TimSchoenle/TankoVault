# TankoVault frontend (Dioxus WASM SPA)

The reader-facing product plus the operator console (design §17), built with
**Dioxus 0.7** + `dioxus-router` and the **Inkstone** design system.

This crate is intentionally **excluded from the host Cargo workspace** (it targets
`wasm32-unknown-unknown` via the `dx` CLI). Build and check it on its own.

## Develop

```bash
# From this directory (web/frontend). Two terminals for the full dev loop:
npm install              # first time only (dev-only Tailwind tooling)
npm run css:watch        # terminal 1: input.css -> assets/main.css on change
dx serve                 # terminal 2: dev server with hot reload

dx build --release       # static WASM + assets for CDN/API hosting

# Plain type-check (what CI can gate on without the dx CLI):
cargo check --target wasm32-unknown-unknown
```

The API is called at the **same origin** under `/v1/...` (see `src/api.rs`,
`API_BASE`). Serve the built assets behind the API (or a CDN that proxies `/v1`).

## Styling

`assets/main.css` is **generated** by the Tailwind CLI from `input.css` + `tailwind.config.js`
(`npm run css:build`, minified) and **committed** so `cargo`/CI builds need no Node. The
bespoke component/`ik-*` classes are authored as plain CSS *below* the `@tailwind` directives
in `input.css` (so they are never content-purged); utilities remain available for one-off
layout. Design tokens (DESIGN_SPEC §2–5) live as `:root` CSS vars in `input.css` and as the
Tailwind `theme` in the config. If you edit either, re-run `css:build` and commit the result.

The redesign is tracked phase-by-phase in
[`../../docs/frontend/PROGRESS.md`](../../docs/frontend/PROGRESS.md).

## Layout

- `src/models.rs` — DTOs mirroring the API JSON contract + domain enum tokens.
- `src/api.rs` — typed `gloo-net` client (Bearer auth, friendly error mapping).
- `src/state.rs` — in-memory session (token + JWT-decoded role) provided via context.
- `src/icons.rs` — inline-SVG icon module (`Icon` enum + `Ic` component; no web font).
- `src/components.rs` — app shell (grouped left rail + command bar), cover cards, skeletons.
- `src/views/` — the screens: Home, Discover, Series detail, Watchlist, Notifications,
  Search, Account, Login/Register, and the operator Console.

## Known follow-ups

- Live updates: the notification WS/SSE and the scan-progress SSE need a token-bearing
  transport (EventSource can't set `Authorization`); the console polls a run's status for
  now.
- Search tag-grouping and virtualised covers (blur-up) are stubbed pending API support.
- Native drag-between-columns on the Watchlist (a status `<select>` is the current mover).
