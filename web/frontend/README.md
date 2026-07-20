# TankoVault frontend (Dioxus WASM SPA)

The reader-facing product plus the operator console (design §17), built with
**Dioxus 0.7** + `dioxus-router` and the **Inkstone** design system.

This crate is intentionally **excluded from the host Cargo workspace** (it targets
`wasm32-unknown-unknown` via the `dx` CLI). Build and check it on its own.

## Develop

```bash
# From this directory (web/frontend):
dx serve                 # dev server with hot reload
dx build --release       # static WASM + assets for CDN/API hosting

# Plain type-check (what CI can gate on without the dx CLI):
cargo check --target wasm32-unknown-unknown
```

The API is called at the **same origin** under `/v1/...` (see `src/api.rs`,
`API_BASE`). Serve the built assets behind the API (or a CDN that proxies `/v1`).

## Styling

`assets/main.css` is a **self-contained, hand-authored** Inkstone stylesheet — the app is
fully styled with no build step. `tailwind.config.js` + `input.css` mirror the same tokens
for anyone who prefers to author utility classes with the Tailwind CLI.

## Layout

- `src/models.rs` — DTOs mirroring the API JSON contract + domain enum tokens.
- `src/api.rs` — typed `gloo-net` client (Bearer auth, friendly error mapping).
- `src/state.rs` — in-memory session (token + JWT-decoded role) provided via context.
- `src/components.rs` — app shell (left rail + command bar), cover cards, skeletons, states.
- `src/views/` — the screens: Discover, Series detail, Reading, Watchlist, Notifications,
  Search, Login/Register, and the operator Console.

## Known follow-ups

- Live updates: the notification WS/SSE and the scan-progress SSE need a token-bearing
  transport (EventSource can't set `Authorization`); the console polls a run's status for
  now.
- Search tag-grouping and virtualised covers (blur-up) are stubbed pending API support.
- Native drag-between-columns on the Watchlist (a status `<select>` is the current mover).
