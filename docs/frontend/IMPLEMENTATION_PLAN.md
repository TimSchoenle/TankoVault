# TankoVault Frontend Redesign — Implementation Plan

> **Goal.** Bring the current Dioxus WASM SPA (`web/frontend/`) up to the enhanced
> `DESIGN_SPEC.md` (the TankoVault mockup), using **Tailwind CSS (compiled)** and an
> **inline-SVG icon module**, adopting the design as the canonical evolution of "Inkstone"
> and **stubbing** features whose backends don't exist yet.
>
> **Inputs:** `DESIGN_SPEC.md` (this folder), `project/TankoVault.dc.html` (raw mockup),
> `docs/design.md` §17 (existing frontend spec), `web/frontend/**` (current code).
>
> **Confirmed decisions (2026-07-21):**
> 1. **Styling** — compile Tailwind via CLI; tokens in `tailwind.config.js`; repeated
>    primitives in `@layer components`; utilities for one-off layout.
> 2. **Icons** — vendored **inline-SVG** Rust module (`src/icons.rs`), no web font.
> 3. **Scope** — adopt TankoVault as canonical; build all screens; **stub** backend-less
>    features with explicit `TODO(api)` markers.

---

## 1. Current state → target (gap analysis)

| Area | Today (`web/frontend`) | Target (`DESIGN_SPEC`) | Work |
| --- | --- | --- | --- |
| **Styling build** | `assets/main.css` hand-authored; Tailwind **never compiled** | Tailwind CLI compiles `input.css` → `assets/main.css`; `@layer components` + utilities | Rework build (§2) |
| **Tokens** | 8 tokens, Clash Display font, no role colors | Full ramp + role/state colors + Bricolage Grotesque (§DESIGN_SPEC 2–3) | Rewrite tokens (§3) |
| **Icons** | none (text/`•`/emoji) | ~60 glyphs | New `icons.rs` (§4) |
| **Fonts** | referenced, not shipped | Bricolage Grotesque + IBM Plex Sans/Mono, self-hosted | `@font-face` + subset (§3.3) |
| **Routing** | `/` = Discover; no Home, no Account | `/` = Home; add `/discover`, `/account` | Route table (§5) |
| **Shell** | flat rail, search-only topbar | grouped rail + icons + user footer; rich topbar | `components.rs` (§6) |
| **Home** | — (a thin `Reading` feed exists) | full dashboard | New view (§7.1) |
| **Discover** | top chips + 3 sorts, no paging | filter panel + tags + sliders + presets + pagination | Rewrite (§7.2) |
| **Series** | 2-block hero + chapters | blurred hero + stats + sidebar + progress | Rewrite (§7.3) |
| **Watchlist** | `<select>` mover | HTML5 drag/drop kanban (+ select fallback) | Rewrite (§7.4) |
| **Notifications** | flat list | tabs + kind icons + covers | Enhance (§7.5) |
| **Search** | grid | bigger field + count | Minor (§7.6) |
| **Account** | — | settings shell (mostly stub) | New view (§7.7) |
| **Console** | single stacked page | tabbed; +Solver, +Adapter-test, +Users | Restructure (§7.8) |
| **Auth** | card exists | reskin | Minor (§7.9) |
| **API** | `list_series(query,limit)` only | filter/sort/paginate + richer detail | Backend items (§9) |

The palette already matches (`--vermilion #E4572E`, `--jade #2E8B78`, ink grounds), so this is
an **evolution, not a rewrite of direction**. The `ik-*` class names stay valid as the
`@layer components` names — most existing markup keeps working while we migrate.

---

## 2. Build pipeline (Tailwind, compiled)

Today `main.css` is hand-authored and Tailwind is a dead mirror. Make Tailwind the real build.

**2.1 Add Node tooling** (dev-only, not shipped in the WASM bundle):
- `web/frontend/package.json` with `tailwindcss` (v3) + `@tailwindcss/cli`, and scripts:
  - `"css:build": "tailwindcss -i ./input.css -o ./assets/main.css --minify"`
  - `"css:watch": "tailwindcss -i ./input.css -o ./assets/main.css --watch"`
- `.gitignore`: keep committing the built `assets/main.css` (so `cargo`/CI builds need no
  Node), but document it as generated.

**2.2 Wire into dev loop.** `dx serve` already watches `assets/` (`Dioxus.toml`
`web.watcher.watch_path = ["src","assets"]`). Run `npm run css:watch` alongside `dx serve`.
Document the two-terminal flow in `web/frontend/README.md`. Optionally add an `xtask`
subcommand (`xtask/`) that shells out to both so CI/devs have one entry point.

**2.3 `input.css` structure:**
```css
@tailwind base;
@tailwind components;
@tailwind utilities;

@layer base   { /* @font-face, :root tokens, html/body, scrollbar, ::selection */ }
@layer components { /* .btn, .card, .chip, .pill, .tile, .input, .nav-link, … */ }
```
Keep `content: ["./src/**/*.rs", "./index.html"]` so classes used in `rsx!` survive
purging. **Guard against purge surprises:** dynamically-built class strings (e.g. state-color
pills) must use whole literal class names or a `safelist` in `tailwind.config.js`.

**2.4 CI.** Add a step that runs `npm ci && npm run css:build` and fails if `git diff
--exit-code assets/main.css` shows drift (the committed CSS must match the config). `cargo
check --target wasm32-unknown-unknown` stays the Rust gate.

---

## 3. Design tokens & fonts

**3.1 `tailwind.config.js`** — replace the current partial theme with the full token set from
`DESIGN_SPEC` §2–4. Add:
- `colors`: `bg, rail, surface, surface2, surfaceFeed, surfaceUnread, border, borderCtl,
  borderRow, borderSoft, text, text2, text3, muted, faint, faint2, iconOff, acc, acc2, acc3,
  accDk, jade, jadeBright`, and role maps `type-*`, `status-*`, `state-*`, `run-*`, `star`.
- `fontFamily`: `display: ["Bricolage Grotesque", ...]`, `body: ["IBM Plex Sans", ...]`,
  `mono: ["IBM Plex Mono", ...]`.
- `borderRadius`: `pill: 20px, card: 14px, ctl: 10px, chip: 8px`.
- `boxShadow`: `cover, hero`.
- `keyframes` + `animation`: `fade, bar, pulse, flow`.
- `safelist`: the state/role pill classes built dynamically.

**3.2 `:root` variables** (in `@layer base`) — mirror the same values as CSS vars so
`@apply` and dynamic `style:` strings can read them. Keep `[data-theme="light"]` (Warm Paper)
overriding `--bg/--surface/--border/--text/--muted`. The accent knob (§8) swaps `--acc*` on a
`[data-accent="…"]` selector.

**3.3 Fonts (self-host, per icon/dep decision).** Do **not** hot-link Google Fonts at
runtime (offline/CSP fragility). Vendor subset `.woff2` files under `assets/fonts/` and
declare `@font-face` in `@layer base`:
- Bricolage Grotesque (variable, 400–800), IBM Plex Sans (400/500/600/700), IBM Plex Mono
  (400/500/600). `font-display: swap`.
- **Update `docs/design.md` §17.1** to name Bricolage Grotesque as the display face (was
  Clash Display/Zodiak) so spec and code agree.

**3.4 `@layer components` primitives** (the thin layer). Port/extend the existing `ik-*`
rules as `@apply`-based classes. Minimum set:
`btn / btn-primary / btn-icon / btn-block`, `card`, `chip / chip-active`, `tag-chip
(neutral/inc/exc)`, `pill` (+ `pill-state` helper), `input / field`, `nav-link (+active)`,
`kicker`, `tile`, `kpi`, `progress`, `switch`, `skeleton`, `live (+on)`, `table`. Everything
else = utilities inline. This keeps `rsx!` readable and centralizes the repeated look.

---

## 4. Icon system (`src/icons.rs`)

Vendor the ~60 glyphs the design uses as a single Rust module — no web font, works offline,
tree-shaken, crisp at any size.

**4.1 Shape:**
```rust
#[derive(Clone, Copy, PartialEq)]
pub enum Icon { Home, Explore, Search, Watchlist, Notifications, Console, Account,
    MenuBook, Settings, PlayCircle, Bolt, AutoAwesome, Layers, Star, Tune, Bookmark,
    Notify, CloudDone, ArrowForward, ChevronRight, Close, Check, OpenInNew, Radar,
    Merge, Group, History, ShieldLock, Code, /* … */ }

#[component]
pub fn Ic(icon: Icon, #[props(default = 20)] size: u32,
          #[props(default)] class: String) -> Element { /* match → <svg> */ }
```
Usage: `Ic { icon: Icon::Home, size: 20 }`. Color via `currentColor` + a text-color utility.

**4.2 Glyph source.** Map each Material Symbol used in the mockup to its **Lucide** (or
Phosphor) equivalent (both MIT). Inline the 24×24 `<path>`. Keep one `match` arm per glyph;
`stroke="currentColor"` or `fill="currentColor"` consistently. Provide `aria-hidden="true"`
by default and an optional `title` prop for standalone icon buttons (a11y).

**4.3 Inventory** (from `DESIGN_SPEC` §6–7): nav (home, explore, search, collections_bookmark,
notifications, space_dashboard, account_circle, settings), actions (play_arrow/circle,
arrow_back/forward, chevron_right, close, check, done_all, add, radar, restart_alt,
open_in_new, more_horiz), status (layers, star, bolt, auto_awesome, cloud_done/sync,
check_circle, notifications_active, local_fire_department, schedule, task_alt, pause_circle,
cancel), console (dns, shield_lock/shield, code, merge, group, history, error, verified_user,
devices, hub, sync_alt, public, block), settings (person, palette, dark_mode/light_mode,
mail, chat, download, upload, devices_other). ~55–60 total.

---

## 5. Routing (`src/main.rs`)

Adopt the design's nav model. Home becomes the landing; the old `Reading` feed folds into
Home.

```rust
#[layout(Shell)]
    #[route("/")]              Home {},
    #[route("/discover")]      Discover {},
    #[route("/search?:q")]     Search { q: String },
    #[route("/series/:id")]    Series { id: String },
    #[route("/watchlist")]     Watchlist {},
    #[route("/notifications")] Notifications {},
    #[route("/account")]       Account {},
    #[route("/console")]       Console {},
    #[route("/login")]         Login {},
    #[route("/:..segments")]   NotFound { segments: Vec<String> },
```
- Add `Home` and `Account` views; remove the standalone `Reading` route (its feed lives in
  Home §7.1) or keep a `#[redirect("/reading", || Route::Home {})]` for old links.
- `same_screen()` in `components.rs` must map `Series → Discover` (already does) and any Home
  sub-states to Home.
- Route guard: `Account`/`Watchlist`/`Notifications`/`Home` require a session (render
  `SignInGate` when unauthenticated, as today); `Console` requires operator role (already).

---

## 6. App shell (`src/components.rs`)

**6.1 Sidebar (`Shell`)** — rebuild the rail to the spec:
- **Brand lockup**: gradient rounded tile + `Icon::MenuBook`, `Tankō`+acc`Vault`, mono
  tagline `SOURCE · TRACK · SYNC`.
- **Grouped nav**: `NavGroup { label }` kicker headers (`MAIN`/`LIBRARY`/`OPERATOR`) + the
  existing `NavLink` extended with an `icon: Icon` prop and the animated active bar (already
  in CSS as `.nav-link.active::before`; add `animation: bar .25s`). Console link stays
  operator-gated.
- **User footer**: `SessionButton` becomes a footer block — avatar initial (jade gradient),
  username + `admin · synced` status, settings gear linking to `Account`. When signed out,
  render the "Sign in" primary button.

**6.2 Header (`TopBar`)**:
- Search input (leading `Icon::Search`, trailing `⌘K` mono chip) — keep the Enter→
  `Search { q }` behavior; add a real `⌘K`/`Ctrl+K` global key handler focusing it.
- Flex spacer.
- **"AniList synced" pill** (`Icon::CloudDone`, jade) — **static/stub** until sync status has
  an endpoint (`TODO(api): GET /v1/me/integrations`).
- **Notifications bell** with the live unread badge (reuse `UnreadBadge` context) → links to
  `Notifications`.

**6.3 Shared components to add:** `StatTile`, `SectionHeading { icon, title }`,
`ProgressBar { pct }`, `Toggle { on, onchange }`, `Pill { label, color }`, `Tabs`, `Avatar`.
Keep `CoverCard`, `Cover`, `SkeletonGrid`, `ErrorBox`, `EmptyBox`, `SignInGate` — extend
`CoverCard` with type pill, unread badge, rating/source scrim (guarded on optional data).

---

## 7. Per-screen implementation

Each screen: files, data source, and the shape to build. `TODO(api)` marks anything needing a
backend addition (collected in §9).

### 7.1 Home — `src/views/home.rs` (new)
- **Data**: `api::feed(token)` (exists) for the "New in watchlist" day-groups (reuse
  `group_by_day` from `reading.rs`); `api::watchlist(token)` for counts. Continue-reading needs
  last-read + next-unread per series → `TODO(api): GET /v1/me/continue` (or derive from feed +
  progress). Stats tiles: "new chapters" = feed len; "reading" = watchlist Reading count;
  "chapters read" → `TODO(api)`.
- **Build**: greeting + `Welcome back, {username}` (username from JWT/session, else "reader");
  3 `StatTile`s; `SectionHeading` "Continue reading" + continue cards (stub/empty until API);
  "New in your watchlist" feed card; **"Because you read" rec rails** → **stub** (hide unless
  `TODO(api): GET /v1/me/recommendations`). Signed-out → `SignInGate`.
- Reuse `reading.rs` logic; then delete `reading.rs`.

### 7.2 Discover — `src/views/discover.rs` (rewrite)
- **Client filter state** (signals): `types, statuses, provs: Vec`, `inc, exc: Vec<tag>`,
  `match_all: bool`, `year_min/max, min_ch, sort, page, panel_open`.
- **Panel** (`FilterPanel` component): type chips, status chips, tag 3-state chips (from
  `api::tags()` — already defined, currently unused), provider checkboxes (from
  `api::providers` counts — operator-only; for readers use a public providers list →
  `TODO(api): GET /v1/providers`), dual year range, min-chapter range, saved presets → **stub**.
- **Results**: sort `<select>` (6 options), removable active-filter chips, count line, cover
  grid, pagination (page dots + prev/next).
- **Data / the important decision**: the current `/v1/series?query&limit` cannot filter by
  type/status/provider/tag/year/chapters, sort, or paginate. Two options:
  - **(Recommended) Extend the API** — `GET /v1/series` gains
    `content_type, status, provider, tag (repeatable), year_min, year_max, min_chapters, sort,
    cursor|page, limit` and returns `{ items, total, next_cursor }`. Add the matching
    `api::list_series(filter: SeriesFilter)` and a `SeriesFilter` struct. This is the correct,
    scalable path. (`TODO(api)` §9.1.)
  - **(Interim) Client-side** — fetch a larger page and filter/sort/paginate in the browser;
    acceptable only as a temporary bridge (won't scale past a few hundred rows). Gate behind a
    clear comment so it's replaced.
- No-results empty state with "Reset filters".

### 7.3 Series detail — `src/views/series.rs` (rewrite layout)
- **Data**: `api::series_detail(id)`, `api::series_chapters(id, source)`, `api::watchlist`
  (exists). Missing for full parity: `rating, author, alt_titles, tags, sources[].is_primary,
  chapters[].read, read_pct` → `TODO(api)` §9.2. Render each **only when present**; the screen
  must degrade gracefully (no rating row if no rating).
- **Build**: blurred cover-backdrop hero (cover as `background` + blur + fade overlays); Back
  button (`nav.go_back()`); cover; type/status/year; title; author/alt (when available); tag
  chips (when available); stat row (rating/chapters/sources, omit rating if absent); actions —
  `Continue ch N` (needs progress; else "Start reading"), `WatchControls` (existing logic,
  restyled), notify bell.
- **Body grid** `1fr 340px`: left = synopsis + chapters (progress bar when read-state known;
  per-row read/unread markers when known; keep "Read" link + "Mark read" from `reading.rs`);
  right sidebar = **Read on** source cards (PRIMARY badge when `is_primary`), **Tracking** card
  (status + notify toggle + AniList row-stub), **Readers also follow** (reuse related; needs
  a related endpoint → `TODO(api): GET /v1/series/:id/related`, else hide).
- Keep the `SourceChip`/`WatchControls`/`ChapterRow` logic; restyle to the new card/list look.

### 7.4 Watchlist — `src/views/watchlist.rs` (rewrite to kanban DnD)
- **Data**: `api::watchlist` + per-item `series_detail` (as today) for title/cover; N+1 detail
  fetches should move behind a batch endpoint → `TODO(api): GET /v1/series?ids=` or embed
  title/cover in the watchlist row (§9.3).
- **Build**: horizontally-scrolling columns (`WatchStatus::COLUMNS`) each with accent + icon +
  count; **HTML5 DnD**: cards `draggable:true` + `ondragstart` (store `series_id` in a signal),
  columns `ondragover`(preventDefault)+`ondrop` → `api::set_watchlist(new_status)` with
  **optimistic** move + rollback. Keep the `<select>` as the keyboard-accessible mover
  (quality floor: DnD is not keyboard-operable alone). Notify bell + unread count on cards.

### 7.5 Notifications — `src/views/notifications.rs` (enhance)
- **Data**: `api::notifications` (exists). Filter tabs (All/Unread/Chapters/Sync) filter the
  loaded list by `kind` client-side. Kind→icon/color map per `DESIGN_SPEC` §7.5. Cover thumb
  needs `series_id`→cover (already deep-linked; fetch or embed cover → minor `TODO(api)`).
- Keep the `UnreadBadge` sync effect and `mark_all` (exists). Add per-row `mark_read` on open.

### 7.6 Search — `src/views/discover.rs::Search` (minor)
- Bigger `56px` field, `{N} results · trigram fuzzy match` line, reuse the new `CoverCard`.
  Tag-grouping stays a follow-up (needs tag search API).

### 7.7 Account — `src/views/account.rs` (new, mostly stub)
- Sub-nav (`SettingsNav`) + panels. Build the **shell and layout fully**; wire only what
  exists:
  - **Profile** — display identity from session; fields are visual; save = `TODO(api): PATCH
    /v1/me/profile`.
  - **Appearance** — **wire for real**: theme toggle writes `data-theme` on `:root` (persist to
    `localStorage`); optional accent/density knobs write `data-accent`/`data-density`.
  - **Security & sessions**, **Sync & integrations** (AniList), **Notification prefs** — render
    with `TODO(api)` stubs and a small "Not yet available" affordance; do not fake success.
- Gate on session; render `SignInGate` when signed out.

### 7.8 Console — `src/views/console.rs` (restructure to tabs)
- Introduce a `console_tab` signal + `Tabs` bar (8 tabs, `DESIGN_SPEC` §7.8). Move each existing
  panel under its tab; the shared `tick` auto-refresh stays.
  - Overview = `SystemOverview` + provider-health tiles (add tiles from `provider_stats`).
  - Live scans = `ScanQueue` (add the active-run banner with the `flow` barber-pole bar).
  - Providers = `ProvidersPanel` (already has inline `base_url` edit / migration).
  - **Challenge & solver** = **new** panel from `provider_stats` (state + solve figures) +
    `set_provider_state`/re-solve action (`TODO(api): re-solve` if not covered by trigger_scan).
  - **Adapter test** = **new** panel using `api::test_adapter` (exists) — config textarea +
    parsed-sample view.
  - Merge = `MergeQueue` (exists). Audit = `AuditPanel` (exists).
  - **Users** = **new**, **stub** — only `system_stats.users_total` exists; no list endpoint
    (`TODO(api): GET /v1/admin/users`).
- Move `REFRESH_MS`/live controls into the header row per the mockup.

### 7.9 Auth — `src/views/auth.rs` (reskin)
- Restyle to the centered `400px` card; keep register/login logic. Add "Create an account"
  toggle.

---

## 8. Theme & knobs

- **Light/dark** already supported via `[data-theme]`. Wire the Appearance toggle (§7.7) to set
  it + persist. Respect `prefers-color-scheme` on first load.
- **Accent / density / cover-style** knobs (`DESIGN_SPEC` §8) are **optional polish**: implement
  as `[data-accent]`/`[data-density]` variable swaps and pass `coverStyle` into the cover
  gradient helper. Ship only if time allows; not on the critical path.

---

## 9. Backend / API work items

Grouped, each tagged **Required** (screen is wrong/broken without it), **Enhance** (screen
works but is thinner than the design), or **Stub** (render shell, defer). Frontend `models.rs`
gets a matching DTO for each shipped item.

**9.1 Series listing (Required for Discover).** Extend `GET /v1/series` with
`content_type, status, provider, tag[], year_min, year_max, min_chapters, sort, page|cursor,
limit`; return `{ items, total, next_cursor }`. Add `SeriesFilter` + update `api::list_series`.
Touches `services/api` (handler + query) and `crates/db` (repo query).

**9.2 Series detail enrichment (Enhance for Series).** Add to `SeriesDetail`/`ChapterDto` where
the domain has data: `rating?`, `author?`, `alt_titles[]`, `tags[]`, `sources[].is_primary`,
and (auth-scoped) `chapters[].read` + `read_pct`. Anything the domain genuinely lacks (e.g.
rating) is **design-only** — omit gracefully, don't invent.

**9.3 Me / reader (Enhance/Stub).**
- `GET /v1/me/continue` — continue-reading cards (Home, Series CTA). *Enhance.*
- Embed `series_title`+`cover_url` in `WatchlistItem` and `Notification` (kills N+1 detail
  fetches on Watchlist/Notifications). *Enhance.*
- `GET /v1/me/recommendations` — "Because you read" / "Readers also follow". *Stub.*
- `GET /v1/me/stats` — lifetime chapters read / streak (Home, Profile). *Stub.*
- `GET /v1/providers` (public) — provider filter list + counts for Discover. *Enhance.*

**9.4 Account (Stub, unblock later).** `PATCH /v1/me/profile`; `GET/DELETE /v1/me/sessions`;
AniList OAuth connect + `POST /v1/me/sync/{pull,push}` + conflict policy; 2FA; `GET/PUT
/v1/me/notification-prefs`. Build UI shells now; wire per `docs/design.md` §20 Phase 5.

**9.5 Console (Enhance/Stub).** `GET /v1/admin/users` (Users tab — *Stub* today); confirm a
**re-solve** action exists or add `POST /v1/admin/providers/:id/resolve` (*Enhance*); live scan
progress via SSE instead of polling (*Enhance*, `docs/design.md` §17.4 known follow-up).

> **Principle:** the frontend must render correctly against **today's** API. Every enrichment
> above is additive and optional at the DTO level (serde `#[serde(default)]` / `Option`), so
> shipping the UI never blocks on the backend.

---

## 10. `models.rs` additions

- `SeriesFilter` (serialize to query string), `SeriesPage { items, total, next_cursor }`.
- Optional fields on `SeriesSummary` (`rating`, `unread`, `chapters`), `SeriesDetail`
  (`rating`, `author`, `alt_titles`, `tags`), `ChapterDto` (`read`), `SourceDto`
  (`is_primary`), `WatchlistItem`/`Notification` (`series_title`, `cover_url`) — all `Option`
  / `#[serde(default)]` so current responses still decode.
- `ContinueItem`, `Recommendation`, `MeStats`, `ProviderPublic`, `UserRow` for the new
  endpoints (added as each ships).
- `NotifKind` enum (`new_chapter | source_added | completed | sync | unknown`) for the tab
  filter + icon map.

---

## 11. Quality floor (verify on every screen — `DESIGN_SPEC` §9)

- Keyboard: chips/tabs/cards are real `<button>`/`<a>`; DnD has the `<select>` fallback;
  `⌘K` focuses search; visible `:focus-visible` ring.
- `prefers-reduced-motion`: `fade/bar/pulse/flow` degrade to static (media query in
  `@layer base`).
- Loading = skeletons; errors name failure + retry (`ErrorBox`); empty = invitation
  (`EmptyBox`).
- Optimistic watchlist/progress/notify with rollback.
- AA contrast; body-size accent text uses `--acc3` on tint.
- Responsive: rail collapses, grid → 2-up, panels stack (existing `@media (max-width:820px)`
  extended to the new panel/kanban/sidebar layouts).

---

## 12. Phased delivery

Each phase is independently demoable; no phase depends on a later one. Backend items land
alongside the phase that needs them, but every phase renders against today's API first.

| Phase | Deliverable | Depends on |
| --- | --- | --- |
| **F0 — Foundation** | Tailwind CLI build + committed CSS; full tokens; `@font-face`; `icons.rs`; `@layer components`. No visual regressions. | §2–4 |
| **F1 — Shell + Home** | Grouped rail + icons + user footer; rich topbar; new routing (`/`=Home); Home dashboard (feed + counts; recs/continue stubbed). | F0 |
| **F2 — Discover + Series** | Filter panel, tags, sliders, sort, pagination (client-interim, then §9.1); blurred-hero Series + sidebar + progress. | F1, §9.1/9.2 |
| **F3 — Watchlist + Notifications** | Kanban DnD (+select fallback); notification tabs + kind icons + covers. | F1, §9.3 |
| **F4 — Account + theme** | Settings shell; Appearance theme toggle wired; other panels stubbed with `TODO(api)`. | F1 |
| **F5 — Console reskin** | Tabbed console; new Solver + Adapter-test panels; Users stub; live-scan banner (SSE when ready). | F1, §9.5 |
| **F6 — Backend enrichment** | Land §9.1–9.5 endpoints; replace client-interim filtering; remove N+1 fetches; wire recs/continue/stats. | §9 |

---

## 13. Risks & notes

- **Tailwind purge vs. dynamic classes** — state/role pills built at runtime must use literal
  class names or `safelist`; otherwise they vanish in production CSS. (§2.3)
- **Committed generated CSS** — CI must fail on `main.css` drift so the checked-in file always
  matches the config (§2.4).
- **N+1 fetches** — Watchlist/Notifications currently fetch `series_detail` per row; fix via
  §9.3 embedding before those lists get large.
- **DnD accessibility** — never ship kanban DnD as the *only* mover; keep the `<select>`.
- **Design-only data** — rating/author/alt/streak may not exist in the domain; the UI must
  omit, never fabricate (invariant: links & metadata only, `docs/design.md` Appendix A).
- **Scope honesty** — every stubbed action shows an honest "not yet available" state; no fake
  success toasts.

---

## 14. Definition of done

- `npm run css:build` reproduces the committed `assets/main.css`; `cargo check --target
  wasm32-unknown-unknown` is clean; `dx build --release` produces a working bundle.
- All 9 screens match `DESIGN_SPEC` §7 within the data available; stubs are visibly-but-honestly
  incomplete with `TODO(api)` in code.
- Quality floor §11 verified on Home, Discover, Series, Watchlist, and Console.
- `docs/design.md` §17.1 updated (Bricolage Grotesque); this plan's phases tracked in
  `docs/IMPLEMENTATION_STATUS.md`.
