# TankoVault frontend (Dioxus)

The reader-facing product plus the operator console (design §17), built with **Dioxus 0.7** +
`dioxus-router` and the **Inkstone** design system.

One component tree, **two builds**:

| Feature | Renderer | Ships as |
| --- | --- | --- |
| `web` (default) | `wasm32-unknown-unknown` in a browser | the SPA image, served by `services/frontend` |
| `desktop` | a `wry` webview — WebView2 on Windows, WebKitGTK on Linux | installers attached to the GitHub release |

They are **mutually exclusive**; `src/platform/mod.rs` says so to the compiler, because the
alternative failure is a wall of unrelated resolution errors. Everything either build needs from
the system it runs on is behind `src/platform`, and nothing under `src/views` knows which it is.

Four differences are not incidental and are documented where they live:

- **There is no served origin on desktop.** `src/views/connect.rs` asks for one on first run and
  stores it; `platform::origin()` answers from there. The access token still lives in memory
  only — the settings file holds the server URL and the appearance choices, never a credential.
- **The refresh cookie is kept in the OS credential store, not in a file.** A native `reqwest`
  has no cookie jar unless asked for one, and one that lives only in memory ends the session
  every time the app closes. `src/api/session_store.rs` mirrors the cookie into the Windows
  Credential Manager or the freedesktop Secret Service instead — encrypted at rest and scoped to
  the reader's login, which is what the browser gives the web build for free and a file in the
  config directory would not give at all. Where no credential store is available the calls are
  silent no-ops and the old close-signs-you-out behaviour returns.
- **Passkeys go through Windows Hello on Windows, and are unavailable elsewhere.** The
  origin rule that blocks a webview ceremony — `rp.id` must be a registrable suffix of the
  *document's* origin — is the browser's, and a wry webview serves this app from its own custom
  protocol. Windows exposes the same ceremony natively through `webauthn.dll`, which takes the
  `clientDataJSON` from the caller, so the desktop build talks to it directly and claims the
  origin the reader connected to. That makes the origin binding **this app's assertion rather
  than a browser's guarantee**; read the module contract in `src/webauthn.rs` before relying on
  it. Linux has no OS passkey provider, so `is_available()` answers `false` there.
- **The app can outlive its own window.** With "keep TankoVault running in the tray" on
  (Settings → Window), the close button hides the window and the app keeps receiving chapter
  pushes; the tray icon opens it again and is the only thing that ends it. `components/tray.rs`
  owns both halves — the icon's lifetime and the window's close behaviour — because either one
  without the other is a defect. The switch is offered only where a tray actually exists:
  always on Windows, and on Linux only when the appindicator library is installed, which
  `platform::desktop`'s `tray::available` probes for rather than assumes. It is not a
  preference there — the library is opened with `dlopen` and the crate behind it *panics* when
  it is missing, which under `panic = "abort"` ends the app.

This crate is intentionally **excluded from the host Cargo workspace** (the web build targets
`wasm32-unknown-unknown` via the `dx` CLI). Build and check it on its own.

## Develop

```bash
# From this directory (web/frontend). Two terminals for the full dev loop:
npm install              # first time only (dev-only Tailwind tooling)
npm run css:watch        # terminal 1: input.css -> assets/main.css on change
dx serve                 # terminal 2: dev server with hot reload

dx build --release       # static WASM + assets for CDN/API hosting
```

The desktop build, against a local stack (`deploy/docker-compose.yml`):

```bash
dx serve --platform desktop --no-default-features --features desktop
```

`dx`'s version must match the `dioxus` in `Cargo.lock` — `dx bundle` refuses outright on a
mismatch. `cargo install dioxus-cli@0.7.10 --locked`.

On Linux the desktop build links WebKitGTK:

```bash
sudo apt-get install -y libwebkit2gtk-4.1-dev libsoup-3.0-dev libxdo-dev libayatana-appindicator3-dev
```

Gates this crate has to pass. It is outside the workspace, so run them here — and **both feature
sets**, or a `#[cfg]` that stops compiling on the other side is first noticed by a release:

```bash
cargo fmt --check
cargo clippy --target wasm32-unknown-unknown --all-targets -- -D warnings
cargo test
cargo clippy --no-default-features --features desktop --all-targets -- -D warnings
cargo test --no-default-features --features desktop
```

## Packaging the desktop app

`.github/workflows/release-please.yaml` cuts these for every release; this is the same command:

```bash
dx bundle --platform desktop --release --no-default-features --features desktop --package-types msi,nsis
```

`deb,appimage` on Linux. The bundle's identity, icons and the WebView2 install mode are in
`Dioxus.toml`; the icons under `assets/icons/` are the SPA's own brand mark (`.ik-brand-tile` +
`Icon::MenuBook`) so the two clients are recognisably one product.

**The published installers are unsigned**, so Windows SmartScreen warns on first run. The
release attaches `sha256sums.txt` beside them.

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
| `crates/inkstone-ui/` | The design system's components, in a crate with no application in it — see its README. A workspace member, so the gates below cover it. |
| `src/components/` | App-flavoured wrappers over that kit (i18n + icons injected), plus the shell, rail, command bar and cover cards. |
| `src/hooks.rs` | `Reload`, `Busy` and `Outcome` — the three patterns every screen repeats. |
| `src/util.rs` | Dependency-free formatting (relative time, chapter numbers, thousands). |
| `src/models.rs` | Re-exports of the generated types plus presentation-only label keys and colours. |
| `src/i18n.rs` | The `i18nrs` provider, the `Translator` handle, and the catalogue parity tests. |
| `locales/` | One JSON message catalogue per shipped language. |
| `src/views/` | One module per screen; `console/` and `account/` are directories, one file per tab or panel. |
| `src/icons.rs` | Inline-SVG icon set (`Icon` enum + `Ic` component; no web font). |

### Rendering a fetch

Use `async_view` / `async_list` from `components` rather than open-coding the
loading/error/empty match. They make "a failed fetch is always visible and always retryable" a
property of the app rather than of each screen — something the hand-rolled matches they
replaced had already drifted away from in several places.

## Messages (i18n)

Every reader-facing string comes from a catalogue in `locales/`, resolved through
[`i18nrs`](https://crates.io/crates/i18nrs) (`dio` feature). No screen holds English text.

```rust
let i18n = use_i18n();                                  // context lookup, not a hook

i18n.t("nav.watchlist")                                 // plain message
i18n.args("home.welcome", &[("name", &name)])           // `{name}` placeholders
i18n.plural("series.sources", count, &[])               // `.one` / `.other`, `{count}` implied
i18n.t(status.label_key())                              // enum label, via its catalogue key
```

The `Translator` is `Copy`, so pass it into handlers and spawned futures — that is how
`api::friendly_error(i18n, err)` words a failure.

Rules the catalogues have to keep, all three enforced by `cargo test`:

- **`en.json` is the source of truth**, and every other locale defines *exactly* the same
  keys. `i18nrs` falls back to an arbitrary catalogue for a missing key (it takes the first
  `HashMap` entry, whose order is undefined), so a key missing from one locale renders
  unpredictably — including as the literal `Key '…' not found`.
- **Never split a sentence around markup.** Interpolate the whole thing
  (`"{total} series · page {page} of {pages}"`); span-wrapped fragments fix the word order to
  English and leave a translator with unorderable scraps.
- **Enums carry a `label_key()`, not a label.** Colours and tokens stay in Rust; the words
  live in the catalogue, so the two cannot drift into separate enumerations.

Adding a language means adding a `locales/<code>.json` and one `Locale` entry in `src/i18n.rs`.
A language outside the one/other plural split also needs a real plural rule in
`Translator::plural`, which asserts on the shipped set so it cannot be forgotten.

The choice persists to `localStorage` under `tv-lang`, alongside the appearance knobs; the
picker is on Account → Appearance. An unset language follows `navigator.language`, matched on
the primary subtag (`de-AT` → `de`).

## Styling

`assets/main.css` is **generated** by the Tailwind **v4** CLI from `input.css`
(`npm run css:build`, minified) and **committed** so `cargo`/CI builds need no Node.

v4 is CSS-first — there is no `tailwind.config.js`. `input.css` holds, in order:

1. `@import "tailwindcss"` + `@source "./src/**/*.rs"` (the content scan) and the `light:`
   custom variant, then `@import` + `@source` for `crates/inkstone-ui` — the kit owns the
   control classes and their markup lives outside this crate's `src/`, so the scanner has to be
   told about it or every one of them is purged;
2. the runtime design tokens as `:root` custom properties, plus the `[data-theme]` /
   `[data-accent]` / `[data-density]` / `[data-cover]` override blocks;
3. the app's own `ik-*` chrome as plain CSS (never purged); the kit's controls — button, pill,
   chip, input, field, table, modal, the layout primitives — are in
   `crates/inkstone-ui/styles/inkstone.css` instead, which names no colour and reads the
   custom properties from (2);
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

Hand-written, and load-bearing. It applies the reader's saved theme and `lang` **before the
first paint** (a WASM bundle cannot touch the DOM until it has downloaded, instantiated and
rendered, which is several hundred milliseconds of the wrong theme, and `lang` decides
hyphenation at parse time) and registers the global ⌘K/Ctrl+K search shortcut once, where a
component effect would stack a duplicate listener on every mount.

## Known follow-ups

- Scan-progress SSE still needs a token-bearing transport (`EventSource` cannot set
  `Authorization`); the console polls a shared 4s tick instead.
- Search tag-grouping and virtualised covers (blur-up) are stubbed pending API support.
- Series "related" needs `GET /v1/series/:id/related`; the slot is an honest placeholder.
- 2FA and password change have no schema or endpoint yet.
