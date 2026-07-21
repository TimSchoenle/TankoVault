# TankoVault Frontend Redesign — Progress & Handoff

> **Purpose.** This is the *frontend-only* status tracker for the TankoVault redesign
> (`web/frontend/`) described in [`DESIGN_SPEC.md`](./DESIGN_SPEC.md) and
> [`IMPLEMENTATION_PLAN.md`](./IMPLEMENTATION_PLAN.md). It is separate from the whole-system
> [`../IMPLEMENTATION_STATUS.md`](../IMPLEMENTATION_STATUS.md). Update it at the end of every
> session so the next agent can pick up without re-deriving context.

**Last updated:** 2026-07-21 (Frontend Session 7)

Legend: ✅ done & compiling · 🟡 partial/in-progress · ⬜ not started · 🔒 blocked on backend

> **Session 7 (F6 frontend rewire — consume the §9 endpoints).** The optional follow-up from
> Session 6 is now done: every screen consumes its matching new endpoint, so no `TODO(api)`
> stub remains except the genuinely-absent AniList sync (§9.4, no endpoint) and series
> "related" (no endpoint). `cargo check --target wasm32-unknown-unknown` is clean (0 warnings).
> What landed this session (client-side only; `models.rs` + `api.rs` + the five views):
> - **§9.1 Discover — server-side filter/sort/paginate.** `views/discover.rs` now builds a
>   `SeriesFilter` from the controls and calls the new `api::list_series_filtered`, which reads
>   the `X-Total-Count` / `X-Next-Cursor` headers into a `SeriesPage`. Content-type/status send
>   the first selection (server is single-valued); tags (include/exclude by slug), release-year
>   (sent only when narrowed past the 1970–2026 slider extents so unknown-year series aren't
>   dropped), and min-chapters all apply server-side. The client-interim filtering is gone.
> - **§9.3 Discover provider facet.** The provider control is populated from the public
>   `GET /v1/providers` (`api::public_providers`) as a "Name (N)" `<select>`; it adds an
>   active-filter chip and is included in the reset.
> - **§9.2 Series enrichment.** `views/series.rs` renders `alt_titles` and `tags` in the hero,
>   a **Primary** badge on the richest source (`is_primary`), and per-chapter **read-state**
>   (dimmed row + check + "Re-read") from the now token-aware `api::series_chapters`.
> - **§9.3 Home.** `views/home.rs` replaces the placeholders with `GET /v1/me/continue`
>   (ContinueCard rail), `/v1/me/stats` (Reading + Chapters-read tiles), and
>   `/v1/me/recommendations` ("Because you read" shelf).
> - **§9.4 Account.** `views/account.rs`: **Profile** now edits username/email via
>   `PATCH /v1/me/profile`; **Security & sessions** lists `GET /v1/me/sessions` with per-row
>   revoke (`DELETE`); **Notification prefs** toggles persist via `GET`/`PUT
>   /v1/me/notification-prefs`. Sync stays an honest stub (no endpoint).
> - **§9.5 Console.** `views/console.rs`: the Users tab renders the `GET /v1/admin/users`
>   directory (identity/role/tracked/joined), and **Re-solve** now calls the dedicated
>   `POST /v1/admin/providers/{id}/resolve` (audited) instead of a raw fast-scan.

> **Session 6 (F6 backend enrichment — the last remaining scope).** The additive
> `IMPLEMENTATION_PLAN.md` §9.1–9.5 endpoints are now implemented in `services/api` +
> `crates/db`, so every `TODO(api)` stub across the redesign is unblocked. All new SQL is
> runtime-checked `query`/`query_as` (no build-time DB), and the whole workspace —
> `cargo check --workspace --all-targets` — is clean; `cargo clippy` adds no new warnings.
> The frontend already renders against these shapes (all additions are `Option`/extra
> fields), so **nothing in `web/frontend/` had to change** and older clients keep working.
> What landed this session:
> - **§9.1 Discover filter/sort/paginate.** `GET /v1/series` now accepts
>   `query, content_type, status, provider, tag[], exclude_tag[], year_min, year_max,
>   min_chapters, sort (updated|title|chapters|sources|year), page|cursor, limit` and filters/
>   sorts/paginates **server-side** (new `catalog::SeriesFilter` + `list_series_filtered`,
>   one `'static` SQL with `($n IS NULL OR …)` guards and `count(*) OVER()`). The JSON body
>   stays a `SeriesSummary[]` (backward-compatible); the match total + next page ride on the
>   `X-Total-Count` / `X-Next-Cursor` response headers. Repeated `tag`/`exclude_tag` params
>   use `axum_extra`'s `Query` (added the `query` feature).
> - **§9.2 Series detail enrichment.** `SeriesDetail` now carries `alt_titles[]`, `tags[]`,
>   and `sources[].is_primary` (richest source by chapter count); `ChapterDto` gains an
>   auth-scoped `read` flag (populated via a best-effort optional-token extractor, omitted for
>   anonymous callers). **Rating/author are design-only (absent from the domain) and remain
>   deliberately omitted — never fabricated.**
> - **§9.3 Me / reader.** New `GET /v1/me/continue` (in-progress cards), `GET
>   /v1/me/recommendations` ("because you read" by shared tags, falling back to recent), and
>   `GET /v1/me/stats` (tracking/reading/completed/chapters-read/unread — no fabricated
>   "streak", since there is no per-chapter read log). `GET /v1/me/watchlist` now **embeds**
>   `series_title`, `cover_url`, `last_read_number`, and `unread` (kills the N+1 detail fetch).
>   New public `GET /v1/providers` (id/slug/name + series counts) for the Discover filter.
> - **§9.4 Account.** `PATCH /v1/me/profile` (username/email, `409` on dup), `GET
>   /v1/me/sessions` + `DELETE /v1/me/sessions/{id}` (active refresh-token families, scoped to
>   the owner), and `GET/PUT /v1/me/notification-prefs` (new `users.notification_prefs jsonb`
>   via migration `0009_account.sql`). 2FA has no schema and is left for a later phase.
> - **§9.5 Console.** `GET /v1/admin/users` (identity + role + tracked count) and `POST
>   /v1/admin/providers/{id}/resolve` (operator "Re-solve" = fast re-scan, proxied to the
>   control-plane, audited). Live-scan SSE already existed (`/v1/admin/scans/stream`).
> - **Net:** F6 is complete; the whole TankoVault redesign (F0–F6) is now done server-side.
>   The optional follow-up is purely frontend polish — swapping each client-interim/stub to
>   consume the matching new endpoint (Discover server filtering, Series read-state, Home
>   continue/recs/stats, Account panels, Console Users/Re-solve).

> **Session 5 (verification + handoff sync).** No frontend code changes were needed — the
> redesign was re-verified end-to-end and the whole-system doc was brought in line. This session:
> - **Re-verified the full build.** `cargo check --target wasm32-unknown-unknown` is clean and
>   `dx build --release --platform web` produces a working bundle: all **8 `.woff2` fonts** +
>   `main.css` + `app_bg.wasm` + `app.js` land in `…/target/dx/app/release/web/public/assets/`.
>   The `wasm-opt failed` line on this Windows host is the known non-fatal size-pass issue (see
>   "How to build & verify"), not an app error.
> - **Re-ran `npm run css:build`** — the committed minified `assets/main.css` (~28 KB) reproduces
>   from `input.css` + `tailwind.config.js` with no drift; `ik-*`, `data-accent`, and
>   `prefers-reduced-motion` rules are all present. (`@font-face` is intentionally **not** in the
>   CSS — it ships via `asset!()` in `src/main.rs`.)
> - **Synced the whole-system tracker.** `../IMPLEMENTATION_STATUS.md` §Frontend was stale (it
>   still described the pre-redesign UI and listed now-done items — drag-between-columns, AniList
>   controls in the UI — as "pending"). It now records the TankoVault redesign as ✅ complete
>   (frontend F0–F5) with F6 backend as the only remaining scope, satisfying IMPLEMENTATION_PLAN
>   §14 ("this plan's phases tracked in `docs/IMPLEMENTATION_STATUS.md`").
> - **Net:** every frontend-completable task remains done; the only open work is the additive
>   **F6 backend** endpoints (§9), which are outside `web/frontend/` and gate the honest
>   `TODO(api)` stubs.
>
> **Session 4 landed (F0 fonts — the last frontend item).** The self-hosted `.woff2` subsets
> are now vendored and shipping, so **every frontend-completable task is done**. The only work
> left in the whole redesign is the additive **F6 backend** endpoints (§9), which are outside
> `web/frontend/` and merely gate the honest `TODO(api)` stubs. New this session:
> - **Real fonts vendored + wired** — latin `.woff2` subsets for **Bricolage Grotesque**
>   (variable, 400–800), **IBM Plex Sans** (400/500/600/700) and **IBM Plex Mono** (400/500/600)
>   were pulled from the `@fontsource(-variable)` packages into `web/frontend/assets/fonts/`.
> - **Bundling fix (important gotcha).** A plain `url()` inside the Tailwind-built `main.css` is
>   **not** processed by manganis — the release bundle copied `main.css` but left the fonts
>   behind (they 404'd → system fallback). Fix: the `@font-face` rules are now emitted from a
>   Rust `FontFaces` component in `src/main.rs` using `asset!()`-resolved URLs, so the files are
>   content-hashed and copied into the bundle. Verified with `dx build --release`: all 8 fonts
>   land in `…/public/assets/*.woff2` alongside the wasm/js/css. `font-display: swap` is set and
>   the system stacks in `input.css` remain the fallback.
> - **`package.json`** now lists the three `@fontsource` packages as `devDependencies` so a
>   future agent can re-vendor the subsets.
>
> **Session 3 (recap): all screens + design-system features against today's API.** New that
> session:
> - **Appearance knobs fully wired** (`views/account.rs` + `input.css` §8): Theme
>   (Inkstone Dark / Warm Paper), **Accent** (vermilion/amber/jade/azure/amethyst), **Density**
>   (cozy/standard/compact), and **Cover style** (ink/duotone/vivid). Each writes a `data-*`
>   attribute on `<html>` and persists a `tv-*` `localStorage` key; defaults clear the attribute
>   so the `:root` value wins. CSS-variable swaps for every knob live in `input.css`.
> - **Boot re-apply + OS fallback** (`components.rs`): on load the persisted `tv-*` knobs are
>   re-applied, and on the first visit the theme falls back to the OS
>   `prefers-color-scheme` (light/dark).
> - **Auth reskin done** (`views/auth.rs`): centered 400px `.ik-auth` card with the wordmark
>   lockup (gradient `MenuBook` tile + `Tankō`·acc`Vault` + `SOURCE · TRACK · SYNC` tagline),
>   register/login toggle ("New here? Create an account" / "Have an account? Sign in"), and
>   Enter-to-submit.
> - **`reading.rs` removed**: its day-grouped feed logic folded into Home (§7.1); no dangling
>   module references remain.
> - **`prefers-reduced-motion`** honored (all keyframe animations disabled) and the responsive
>   breakpoints (≤980px sidebar stack, ≤820px rail collapse + 2-up grid) are in place.
> - **CSS build normalized**: `assets/main.css` was regenerated with `npm run css:build`
>   (minified) so it now **exactly reproduces** what the committed `package.json` script
>   produces — the previously-committed copy had drifted (un-minified). All `ik-*`, `data-accent`,
>   and media-query rules verified present in the built output.
> - **Doc consistency**: `docs/design.md` §17.1 now names **Bricolage Grotesque** as the display
>   face (was the *Zodiak / Clash Display* placeholder), matching `DESIGN_SPEC.md` §3 and the
>   `--font-display` token.
>
> **Session 2 (recap).** F2 Discover + Series, F3 Watchlist kanban DnD, F5 tabbed Console
> (Overview / Live scans / Providers / Challenge & solver / Adapter test / Merge / Users / Audit).
>
> **Session 1 (recap).** F0 Tailwind CLI build pipeline + full §2–5 token set; inline-SVG
> `icons.rs`; design nav routing (`/`=Home); rebuilt shell; Home dashboard; Notifications tabs.

---

## How to build & verify

```powershell
# From web/frontend/ — compile the Tailwind stylesheet (dev-only Node tooling).
cd web/frontend
npm install            # first time only (offline OK once node_modules present)
npm run css:build      # input.css -> assets/main.css (committed, minified)
npm run css:watch      # alongside `dx serve` during development

# Rust gate (run from web/frontend/). Must stay clean.
cargo check --target wasm32-unknown-unknown

# Release bundle (verifies fonts + assets ship correctly).
dx build --release --platform web

# Full dev loop: two terminals — `npm run css:watch` + `dx serve`.
```

> **wasm-opt note.** `dx build --release` may log `wasm-opt failed` on some Windows hosts; the
> build still completes and emits a working bundle (wasm-opt is an optional size pass). Not a
> blocker introduced by app code.

`assets/main.css` is **generated** from `input.css` + `tailwind.config.js` and committed so
`cargo`/CI builds need no Node. If you change `input.css`/config, re-run `css:build` and commit
the result. **Always commit the minified output** (`--minify`) so it matches the `css:build`
script and CI's drift check.

---

## Phase status (from IMPLEMENTATION_PLAN §12)

| Phase | Deliverable | Status |
| --- | --- | --- |
| **F0 — Foundation** | Tailwind CLI build + committed CSS; full tokens; `icons.rs`; component layer; self-hosted fonts | ✅ (real `.woff2` subsets vendored + shipping via `asset!()`) |
| **F1 — Shell + Home** | Grouped rail + icons + user footer; rich topbar; `/`=Home routing; Home dashboard | ✅ |
| **F2 — Discover + Series** | Filter panel + tags + sliders + sort + pagination; blurred-hero Series + sidebar | ✅ (server filter/sort/paginate is client-interim until §9.1) |
| **F3 — Watchlist + Notifications** | Kanban DnD (+select fallback); notification tabs + kind icons + covers | ✅ (cover thumbs on notifications still `TODO(api)`) |
| **F4 — Account + theme** | Settings shell; Appearance theme/accent/density/cover wired; other panels stubbed | ✅ (stub panels await §9.4) |
| **F5 — Console reskin** | Tabbed console; Solver + Adapter-test panels; Users stub; live-scan banner | ✅ (SSE live-scan + solve metrics await §9.5) |
| **F6 — Backend enrichment** | §9.1–9.5 endpoints; recs/continue/stats; account + console-users; re-solve | ✅ (endpoints shipped in `services/api` + `crates/db`; frontend rewire to consume them is optional polish) |

---

## Detailed component status

### Build & tokens (F0)
- ✅ `package.json` + Tailwind CLI scripts (`css:build`/`css:watch`).
- ✅ `tailwind.config.js` full token set (colors/fonts/radius/shadow/keyframes/animation + safelist).
- ✅ `input.css` — @tailwind directives + full `:root` tokens (DESIGN_SPEC §2) + light theme +
  **accent/density/cover knob variable swaps (§8)** + keyframes + `prefers-reduced-motion` +
  responsive media queries + component classes (plain CSS, purge-safe).
- ✅ **Fonts** — self-hosted latin `.woff2` subsets vendored under `assets/fonts/`
  (Bricolage Grotesque variable 400–800, IBM Plex Sans 400/500/600/700, IBM Plex Mono
  400/500/600). `@font-face` rules are emitted from the `FontFaces` component in `src/main.rs`
  via `asset!()` (NOT from `input.css` — a plain `url()` in the Tailwind CSS is not processed
  by manganis and would 404 in release). System stacks remain the fallback; `font-display: swap`.
  Verified bundled by `dx build --release`.
- ✅ `src/icons.rs` — inline-SVG `Icon` enum + `Ic` component (~45 glyphs).

### Screens
- ✅ Home (`views/home.rs`) — greeting + stat tiles (`/v1/me/stats`), continue-reading rail
  (`/v1/me/continue`), folded-in feed, and "Because you read" recs (`/v1/me/recommendations`).
- ✅ Discover (`views/discover.rs`) — **server-side** filter/sort/paginate via
  `api::list_series_filtered` (type/status/provider/tags/year/min-chapters/sort/page); provider
  facet from `/v1/providers`; active-filter chips + count line + header-driven pager.
- ✅ Series (`views/series.rs`) — blurred-hero + `1fr 340px` grid + Read-on/Tracking sidebar,
  now with alt-titles, tag chips, primary-source badge, and per-chapter read-state (§9.2).
  Rating/author stay omitted (design-only, absent from the domain); "related" awaits an endpoint.
- ✅ Watchlist (`views/watchlist.rs`) — kanban with HTML5 DnD + `<select>` keyboard fallback.
- ✅ Notifications (`views/notifications.rs`) — filter tabs + kind icons + unread subtitle.
  (Cover thumbs still need embedded `cover_url` — minor `TODO(api)`.)
- ✅ Search (`views/discover.rs::Search`) — big field + `{N} results · trigram` line.
- ✅ Account (`views/account.rs`) — settings shell; **Appearance** + **Profile** (`PATCH
  /v1/me/profile`) + **Security & sessions** (`GET/DELETE /v1/me/sessions`) + **Notification
  prefs** (`GET/PUT /v1/me/notification-prefs`) all wired; Sync stays an honest stub (§9.4).
- ✅ Console (`views/console.rs`) — 8-tab layout; Solver/Adapter-test/Users tabs + existing
  Overview/Live-scans/Providers/Merge/Audit.
- ✅ Auth (`views/auth.rs`) — reskinned 400px card, wordmark lockup, register/login toggle.

> **Note.** Every screen renders against **today's** API; each backend gap is a visible, honest
> `TODO(api)` note rather than a fabricated value or fake success.

---

## Backend / API work items (see IMPLEMENTATION_PLAN §9) — DONE (Session 6)

All §9.1–9.5 endpoints are **implemented and compiling** in `services/api` + `crates/db`
(runtime-checked SQL; migration `0009_account.sql`). Each remains additive, so the frontend
still renders against them unchanged; consuming them in the UI is optional polish.

- ✅ §9.1 `GET /v1/series` filter/sort/paginate — server-side via `catalog::SeriesFilter` +
  `list_series_filtered`; body stays `SeriesSummary[]`, total/next on `X-Total-Count` /
  `X-Next-Cursor` headers. Handler: `series::list`.
- ✅ §9.2 `SeriesDetail` enrichment — `alt_titles[]`, `tags[]`, `sources[].is_primary`, and
  auth-scoped `ChapterDto.read`. Repo: `catalog::list_series_titles`/`list_series_tags`.
  (Rating/author are design-only → omitted, not faked.)
- ✅ §9.3 `GET /v1/me/continue`, `GET /v1/me/recommendations`, `GET /v1/me/stats`; watchlist
  now embeds `series_title`/`cover_url`/`last_read_number`/`unread`; public `GET /v1/providers`.
  Repo: `tracking::{watchlist_detailed,continue_reading,me_stats,recommendations}`,
  `providers::list_public`.
- ✅ §9.4 `PATCH /v1/me/profile`, `GET /v1/me/sessions` + `DELETE /v1/me/sessions/{id}`,
  `GET/PUT /v1/me/notification-prefs` (new `users.notification_prefs`). Repo: `users::*`.
  (2FA deferred — no schema yet.)
- ✅ §9.5 `GET /v1/admin/users` (`users::list_users`) + `POST /v1/admin/providers/{id}/resolve`
  (`admin::resolve_provider`). Live-scan SSE was already present (`/v1/admin/scans/stream`).

---

## Pick up next (ordered)

> **F0–F6 are complete — including the frontend rewire (Session 7).** Every screen now
> consumes its §9 endpoint. The items below are done ✅; the only remaining honest stubs are
> features with no backend endpoint at all (AniList sync, series "related").

1. ✅ **Discover (§9.1)** — `api::list_series_filtered` sends the full filter/sort/page and
   reads `X-Total-Count`/`X-Next-Cursor` into `SeriesPage`; client-interim filter removed.
2. ✅ **Series (§9.2)** — `alt_titles`/`tags`/`is_primary` and per-chapter `read` state render.
3. ✅ **Home (§9.3)** — continue/recs/stats consume `GET /v1/me/continue|recommendations|stats`.
4. ✅ **Account (§9.4)** — Profile (`PATCH /v1/me/profile`), Security (`GET/DELETE
   /v1/me/sessions`) and Notification-prefs (`GET/PUT /v1/me/notification-prefs`) are live.
5. ✅ **Console (§9.5)** — Users tab from `GET /v1/admin/users`; Re-solve →
   `POST /v1/admin/providers/{id}/resolve`.
6. ✅ **Discover provider filter (§9.3)** — provider `<select>` populated from `GET /v1/providers`.

### Genuinely remaining (no endpoint yet)
- **AniList sync & integrations** (Account → Sync; Series sidebar) — no `/v1/me/sync` endpoint.
- **Series "related"** — no `/v1/series/:id/related` endpoint (Home recs cover the general case).
- **2FA / password change** — no schema/endpoint yet (Account → Security notes this).
- **Saved filter presets** (Discover) and **notification cover thumbs** — minor niceties.

## Notes / gotchas
- CSS strategy: custom rules live as **plain CSS** after the `@tailwind` directives in
  `input.css` (not inside `@layer`), so Tailwind's content-purge never drops the semantic
  `ik-*`/component classes. A `safelist` in the config also protects dynamically-composed names.
- Commit the **minified** `assets/main.css` (matches the `css:build` script + CI drift check).
- Binary target is `app` (not the package name) — see `Cargo.toml`.
- Keep every stubbed action honestly "not yet available"; never fake success (DESIGN_SPEC §9).
- No git history in this repo; use project-wide search, not `git grep`.
- **Fonts ship via `asset!()`, not CSS `url()`**: manganis does not rewrite/bundle `url()`
  references inside the Tailwind-built `main.css`, so `@font-face` lives in `src/main.rs`
  (`FontFaces`). If you add more weights/faces, vendor the `.woff2` into `assets/fonts/` and add
  an `asset!()` + `@font-face` line there — do NOT put `url(fonts/…)` in `input.css`.
- The `@fontsource*` packages are `devDependencies` only (source of the vendored subsets); the
  committed `.woff2` files under `assets/fonts/` are what actually ships.
