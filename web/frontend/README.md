# TankoVault frontend (Dioxus WASM SPA)

The reader-facing product plus the operator console (design §17), built with **Dioxus 0.7** +
`dioxus-router` and the **Inkstone** design system.

This crate is intentionally **excluded from the host Cargo workspace** (it targets
`wasm32-unknown-unknown` via the `dx` CLI). Build and check it on its own.

## Develop

```bash
# From this directory (web/frontend). Two terminals for the full dev loop:
npm install              # first time only (dev-only Tailwind tooling)
npm run css:watch        # terminal 1: input.css -> assets/main.css on change
dx serve                 # terminal 2: dev server with hot reload

dx build --release       # static WASM + assets for CDN/API hosting
```

Gates this crate has to pass. It is outside the workspace, so run them here:

```bash
cargo fmt --check
cargo clippy --target wasm32-unknown-unknown --all-targets -- -D warnings
cargo test
```

## API access

Everything goes through the **generated** client (`tankovault-api-client`, produced by
`cargo run -p xtask -- openapi` from the API service's `utoipa` schema). There is no
hand-written DTO and no untyped escape hatch: if an endpoint is missing here, the fix is to
give it a response schema in `services/api` and regenerate, not to hand-roll a request.

`src/api` exposes a single `Api` handle, obtained with `api::use_api()`:

```rust
let api = api::use_api();          // `Copy` — capture it into as many closures as you like

let items = use_resource(move || {
    reload.track();                 // refetch when a mutation bumps this handle
    let client = api.client();      // reads the *live* bearer token, and subscribes
    async move { client.watchlist().send().await.map(ResponseValue::into_inner) }
});
```

Build the client in the **synchronous** part of a `use_resource` closure. Reading the session
token there subscribes the resource, so it refetches automatically when the boot-time silent
refresh lands a token a moment after first paint. A client captured at render time instead
leaves the screen stuck on its signed-out result, 401ing forever.

## Layout

| Path | What lives there |
| --- | --- |
| `src/main.rs` | Module tree and `launch`, nothing else. |
| `src/app.rs` | Route table, root contexts, bundled `@font-face` rules. |
| `src/api/` | The `Api` handle (`mod.rs`) and user-facing failure text (`error.rs`). |
| `src/state/` | Session (`mod.rs`), unverified JWT claim decoding (`jwt.rs`), appearance knobs (`prefs.rs`). |
| `src/components/` | Shell, rail, command bar, cover cards, and the shared loading/empty/error primitives. |
| `src/hooks.rs` | `Reload`, `Busy` and `Outcome` — the three patterns every screen repeats. |
| `src/util.rs` | Dependency-free formatting (relative time, chapter numbers, thousands). |
| `src/models.rs` | Re-exports of the generated types plus presentation-only labels and colours. |
| `src/views/` | One module per screen; `console/` and `account/` are directories, one file per tab or panel. |
| `src/icons.rs` | Inline-SVG icon set (`Icon` enum + `Ic` component; no web font). |

### Rendering a fetch

Use `async_view` / `async_list` from `components` rather than open-coding the
loading/error/empty match. They make "a failed fetch is always visible and always retryable" a
property of the app rather than of each screen — something the hand-rolled matches they
replaced had already drifted away from in several places.

## Styling

`assets/main.css` is **generated** by the Tailwind **v4** CLI from `input.css`
(`npm run css:build`, minified) and **committed** so `cargo`/CI builds need no Node.

v4 is CSS-first — there is no `tailwind.config.js`. `input.css` holds, in order:

1. `@import "tailwindcss"` + `@source "./src/**/*.rs"` (the content scan) and the `light:`
   custom variant;
2. the runtime design tokens as `:root` custom properties, plus the `[data-theme]` /
   `[data-accent]` / `[data-density]` / `[data-cover]` override blocks;
3. the bespoke `ik-*` component classes as plain CSS (never purged);
4. two `@theme` blocks at the foot — `@theme` for static tokens (fonts, radii, shadows,
   animations) and `@theme inline` for the palette, mapping `--color-*` onto the runtime `--*`
   variables so the `[data-*]` overrides keep re-tinting utilities at runtime. A non-inline
   `@theme` would freeze them at build time.

If you edit `input.css`, re-run `css:build` and commit the result.

**Fonts ship via `asset!()`, not CSS `url()`**: manganis does not rewrite `url()` references
inside the Tailwind-built stylesheet, so `@font-face` lives in `src/app.rs`. To add a weight,
vendor the `.woff2` into `assets/fonts/` and add an `asset!()` + `@font-face` line there — do
**not** put `url(fonts/…)` in `input.css`.

## `index.html`

Hand-written, and load-bearing. It applies the reader's saved theme **before the first paint**
(a WASM bundle cannot touch the DOM until it has downloaded, instantiated and rendered, which
is several hundred milliseconds of the wrong theme) and registers the global ⌘K/Ctrl+K search
shortcut once, where a component effect would stack a duplicate listener on every mount.

## Known follow-ups

- Scan-progress SSE still needs a token-bearing transport (`EventSource` cannot set
  `Authorization`); the console polls a shared 4s tick instead.
- Search tag-grouping and virtualised covers (blur-up) are stubbed pending API support.
- Series "related" needs `GET /v1/series/:id/related`; the slot is an honest placeholder.
- 2FA and password change have no schema or endpoint yet.
