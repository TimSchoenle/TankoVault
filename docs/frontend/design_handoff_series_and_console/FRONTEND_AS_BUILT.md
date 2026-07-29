# TankoVault frontend — as-built design record

Snapshot of what the **Rust frontend actually renders today** (`web/frontend/`, Dioxus 0.6-style
`rsx!` + Tailwind v4), written as the baseline for future design adjustments.

Read alongside — but do not confuse with — `docs/frontend/DESIGN_SPEC.md`, which is the *intent*
extracted from the original mockup. This file records the *implementation*, and flags where the
two differ.

- **Stack:** Dioxus (WASM), typed route enum, `progenitor` generated API client, SSE for live
  notifications, Tailwind v4 CSS-first theme + hand-authored `ik-*` component layer.
- **Styling entry point:** `web/frontend/input.css` → compiled to the committed
  `web/frontend/assets/main.css` (`npm run css:build` / `css:watch`). There is no
  `tailwind.config.js`; tokens live in `@theme` blocks at the foot of `input.css`.
- **Fonts:** self-hosted `.woff2` subsets in `assets/fonts/`, `@font-face` emitted from Rust
  (`src/app.rs::FontFaces`) so manganis content-hashes them.

---

## 1. Design language, as implemented

Dark-first, near-neutral blue-black ground; warm off-white text; **one** bold accent
(vermilion `#E4572E`) used as a line, a glow and a fill on primary buttons — never as a large
flood. A jade/teal role carries "healthy / synced / up to date". Numerics, IDs, timestamps and
kicker labels are mono. Contrast comes from the tonal ramp, not saturation.

Signature devices actually present in the code:

| Device | Where |
| --- | --- |
| 3px accent bar on the active rail item, `scaleX` in from the left (`ikbar`) | `.ik-nav-link.active::before` |
| Gradient brand tile + `Tankō`/`Vault` wordmark, mono tagline | `.ik-brand*`, `components/nav.rs` |
| Cover cards with a gradient placeholder + large typographic initial | `.ik-cover-fallback`, `components/cover.rs` |
| Pulsing live dot on real-time surfaces | `.ik-live.on .ik-live-dot`, `console/controls.rs` |
| Blurred cover backdrop behind the series hero | `.ik-hero-wrap`, `.ik-hero-bg` |
| Thin accent brush divider (used sparingly) | `.ik-brush`, `components/feedback.rs::Brush` |

**Deviation from the spec:** the mockup's per-series hue-derived cover gradient is *not*
implemented per series — the placeholder uses one global `--cover-fallback` gradient, swapped by
the cover-style knob. Per-series hues would need a computed inline style in `Cover`.

---

## 2. Tokens (single source of truth: `input.css`)

### Ground & surface (dark default)
`--bg #0b0e13` · `--rail #0c1016` · `--surface #12171e` · `--surface-2 #0e131a` ·
`--surface-feed #0f141a` · `--surface-unread #101720`

### Borders
`--border #1a212a` (structure) · `--border-ctl #232a33` (controls) · `--border-row #161c24`
(list rules) · `--border-soft #1e262f` (progress track)

### Text ramp
`--text #f2efe9` · `--text-2 #c9cfd6` · `--text-3 #b8c0ca` · `--muted #8a94a0` ·
`--faint #5c6672` · `--faint-2 #4d5763` · `--icon-off #7c8794`

### Accent + semantic
`--acc #e4572e` · `--acc2 #f07a56` · `--acc3 #f2a993` (accent text on tint) · `--acc-dk #b83a17`
(gradient stop) · `--jade #2e8b78` / `--jade-bright #3da88f` · `--star #cba43c`

Tinting convention used everywhere: fill `color-mix(in srgb, var(--acc) 8–18%, transparent)`,
border `var(--acc)` or `color-mix(... 40–55%, transparent)`, text `var(--acc3)`.

### Data-role colors (theme-invariant, `@theme` block)
Deliberately literal — a status must mean the same thing in both themes:
`--color-type-*` (manga/manhwa/manhua/webtoon), `--color-status-*` (ongoing/completed/hiatus/
cancelled), `--color-state-*` (active/degraded/challenged/solving/blocked/disabled),
`--color-run-*` (running/completed/failed/queued).

### Metrics
`--radius 14px` · `--rail-w 250px` · `--card 158px` · `--gap 18px`; plus
`--radius-pill 20px` / `-card 14px` / `-ctl 10px` / `-chip 8px`, and
`--shadow-cover 0 8px 22px rgb(0 0 0 /.35)` / `--shadow-hero 0 20px 50px rgb(0 0 0 /.55)`.

### Light theme ("Warm Paper")
`[data-theme="light"]` overrides the ground/border/text tokens only (paper `#efe9dc`, ink
`#1c1a15`). Accent and data roles are shared. **Note:** the light block does not override
`--faint-2`, `--icon-off`, `--surface-unread` sub-tints beyond those listed — check contrast
before adding new light-mode surfaces.

### Typography
| Role | Family | Used for |
| --- | --- | --- |
| `--font-display` | Bricolage Grotesque (variable 400–800) | wordmark, page/hero titles, section heads, KPI/stat values, cover initials |
| `--font-body` | IBM Plex Sans 400/500/600/700 | all UI text; body 15px / 1.55 |
| `--font-mono` | IBM Plex Mono 400/500/600 | chapter numbers, counts, timestamps, IDs, uppercase kickers, `⌘K` |

Scale in use: 9–11px mono pills/kickers · 12–13px meta · 14–15px body/controls · 16–20px
subheads · 19px section heads · 26–30px page titles & KPI values · 28px `.ik-page-title`.

### Motion
`ikfade` (view mount, .35s) · `ikbar` (active nav bar, .25s) · `ikpulse` (live dots, 1.6s) ·
`ikflow` (barber-pole scan progress, 2s) · `ik-shimmer` (skeletons, 1.3s). All animation is
disabled wholesale under `prefers-reduced-motion: reduce`.

---

## 3. Appearance knobs (all four are real)

Written as `data-*` on `<html>` + a `tv-*` localStorage key; re-applied by an inline script in
`index.html` **before first paint** (WASM boots too late to avoid a theme flash). Implemented in
`state/prefs.rs`, surfaced in Account → Appearance.

| Knob | Attribute / key | Values (default first) | Swaps |
| --- | --- | --- | --- |
| Theme | `data-theme` / `tv-theme` | dark, light | full ground/text ramp |
| Accent | `data-accent` / `tv-accent` | vermilion, amber, jade, azure, amethyst | `--acc`, `--acc2`, `--acc3`, `--acc-dk` |
| Density | `data-density` / `tv-density` | standard, cozy, compact | `--card`, `--gap` |
| Cover style | `data-cover` / `tv-cover` | ink, duotone, vivid | `--cover-fallback` |
| Language | `lang` / `tv-lang` | en, de (`locales/*.json`) | i18n catalogue |

Selecting a default *clears* the attribute so `:root` wins again — except theme, which always
writes (`dark` must be distinguishable from "no choice, follow OS").

Utilities that must keep re-tinting at runtime are declared in `@theme inline`; static tokens
(fonts, radii, shadows, animations) in a plain `@theme`.

---

## 4. Shell & chrome

`Shell` (`components/shell.rs`) = `.ik-app` grid `250px 1fr`, persistent across every route.
It also owns three background concerns: silent token refresh (fires 60s before `exp`, exponential
backoff on transient failure, sign-out only on a real 401), capability sync (keyed on the token),
and the SSE notification subscription feeding the rail badge.

- **Rail** (`.ik-rail`, `components/nav.rs`): brand lockup → grouped nav with mono kicker labels
  (`MAIN` / `LIBRARY` / `OPERATOR` / `ACCOUNT`) → spacer → user footer (avatar initial, name,
  status dot + capability tier, gear → Account) or a "Sign in" block when signed out.
  Entries are **capability-gated**: an item renders only if the reader is permitted *and* the
  deployment feature is on. Series detail keeps Discover lit (`same_screen`).
- **Top bar** (`.ik-topbar`, 64px sticky, `backdrop-filter: blur(14px)`): search field
  (`#tv-search`, focused by ⌘K/Ctrl+K bound in `index.html`, Enter → `/search?q=`), spacer,
  external-sync pill (jade when genuinely `linked`, otherwise a neutral "connect" pill), and the
  bell with unread count. Both actions only render when signed in.
- **Content** (`.ik-content`): 28/32px padding, `max-width: 1280px`, `ikfade` on mount.

---

## 5. Routes

`/` Home · `/discover` · `/series/:id` · `/watchlist` · `/notifications` · `/account`
(+ `/account/anilist-callback?code`) · `/search?q` · `/login` · `/verify-email?token` ·
`/forgot-password` · `/reset-password?token` · `/console` · `/:..segments` NotFound.
`/reading` redirects to Home (the old feed folded into the dashboard).

---

## 6. Screens, as built

**Home** — greeting + lifetime stat tiles; continue-reading rail; day-grouped "New in your
watchlist" feed (`TODAY`/`YESTERDAY` mono day heads); "Because you read …" recommendation shelf.
Signed-out → `SignInGate`.

**Discover** — two-pane `.ik-discover` (`298px` sticky filter panel + results). Filters:
content type chips, status chips, 3-state tag chips (neutral → include → exclude) with any/all
match toggle, provider checkboxes with counts, release-year dual range (1970–2026), min-chapters
range, saved presets. Results: panel toggle, sort `<select>` (updated/title/chapters/sources/
rating/year), removable active-filter chips, count line, cover grid, offset pagination with page
jump. **All filtering/sorting/paging is server-side** via `GET /v1/series` with
`X-Total-Count`/`X-Next-Cursor`; facets from `/v1/tags` and `/v1/providers` degrade to empty
rather than erroring. Page size 24.

**Search** — same cover grid with a large query echo and result count.

**Series detail** — `.ik-hero-wrap` blurred backdrop; `186px` cover; type pill + status dot +
year; display title; stat row; watchlist/notify actions. Body grid `1fr 340px`: synopsis +
chapter list (mono `Ch N`, sub-chapter "part" rows indented under a collapsible toggle) on the
left; "Read on" source cards (PRIMARY badge, `open_in_new`), Tracking card, "Readers also follow"
slot on the right. Fields the API does not expose (rating, per-chapter read state, read-%) are
**omitted, never fabricated**; related series is an honest placeholder pending
`/v1/series/:id/related`.

**Watchlist** — `.ik-board` kanban of status columns, HTML5 drag-and-drop between columns with
`.ik-col.dragover` highlight, plus a per-card status `<select>` as the keyboard-operable
equivalent. Cards carry notify toggle + remove. "Sync now" drives the external tracker.

**Notifications** — filter tabs (All/Unread/Chapters/Sync, client-side), rows with a kind icon
tile, cover thumb, `Title — text`, relative time, unread left-bar + darker ground; mark-all-read;
keeps the rail badge in sync. Unknown notification kinds still render (kind token as the line).

**Account** — `.ik-tabs` + one panel per concern, each in a `PanelCard` (max 560px):
Profile (display name/email, `PATCH /v1/me/profile`, name updates instantly — no relog),
Appearance (the four knobs + language), Security & sessions (list/revoke sessions; password &
2FA say "no endpoint yet" rather than showing dead controls), Sync & integrations (data-driven
from `/v1/me/sync/providers`), Notification preferences (toggle rows), Privacy & data (export,
formal request, delete account — each gated on its feature flag). Panels are feature-gated;
Appearance is always present. Sign-out sits in the page header.

**Operator console** — header (dashboard icon + title + `LiveControls` pill with pause/resume and
manual refresh) then `.ik-tabs`. **Eleven** tabs, each declaring the permission *and* feature that
open it, and only visible tabs render (a reader with one grant gets a one-tab console):
Overview (KPI tiles + per-provider stats table) · Live scans (trigger, active-run progress,
history, failure triage) · Providers (health tiles, create form, per-provider editor: edit, state,
scan, dry-run, delete) · Solver · Adapter test · Merge queue · Sync admin (linked accounts,
mappings, backlogs) · Users (searchable directory + detail drawer: identity, suspension, forced
sign-out, permission grants, erasure) · Feature flags · Privacy queue (urgency-ordered, overdue
marked, resolve vs. actually-do split) · Audit trail.
Read-only panels share one pausable 4s `RefreshTick`; the **editing** surfaces (Users, Flags,
Providers) are deliberately off the tick so a background refetch cannot land mid-edit.

**Auth** — centered `.ik-auth` 400px card: wordmark lockup, login/register toggle, fields, error
line and a neutral `.ik-note` status line (e.g. "check your inbox"), plus forgot/reset/verify
pages.

---

## 7. Component & class inventory (`ik-*`)

Everything is a hand-authored class in `input.css`, below the Tailwind import so the content scan
cannot purge it. Tailwind utilities are for one-off layout only; inline `style:` strings are used
for local tweaks.

- **Shell:** `ik-app`, `ik-rail`, `ik-brand`/`-tile`/`-tag`, `ik-wordmark`, `ik-navgroup(-label)`,
  `ik-nav-link(.active)`, `ik-nav-badge`, `ik-rail-spacer`, `ik-userbox`, `ik-avatar`,
  `ik-status-dot`, `ik-main`, `ik-topbar(-actions)`, `ik-search`, `ik-bell`, `ik-content`.
- **Headings:** `ik-page-title`, `ik-page-head`, `ik-home-head`, `ik-kicker`, `ik-section-head`,
  `ik-subhead`, `ik-brush`.
- **Cards & data:** `ik-grid`, `ik-card(-body/-title/-meta)`, `ik-cover`, `ik-cover-fallback`,
  `ik-stat-row`/`ik-stat`, `ik-tiles`/`ik-tile`, `ik-kpis`/`ik-kpi(-label/-value/-sub)`,
  `ik-table(-compact)`, `ik-tablewrap`, `ik-row(.unread)`, `ik-daygroup`/`ik-dayhead`,
  `ik-board`/`ik-col(.dragover)`, `ik-wl-card`, `ik-kind`, `ik-sidebar-card`,
  `ik-chapter(.part)`, `ik-chapter-toggle`, `ik-part-pill`, `ik-chevron`, `ik-source-card`,
  `ik-source-tile`, `ik-hero(-wrap/-bg)`, `ik-body-grid`, `ik-stat-inline`.
- **Controls:** `ik-btn` (+`.primary`, `.block`, `ik-btn-icon`), `ik-input`, `ik-select`,
  `ik-field`, `ik-chip`, `ik-tagchip(.inc/.exc)`, `ik-pill` (+`.jade`/`.vermilion`/`.acc`/`.run`),
  `ik-tabs`/`ik-tab`, `ik-switch(.on)`, `ik-progress`, `ik-range(-row)`, `ik-checkline`,
  `ik-afchip`, `ik-pagination`, `ik-page-jump`, `ik-panel-toggle`, `ik-link`, `ik-icon-link`.
- **States:** `ik-skeleton`/`ik-skel-cover`, `ik-empty`, `ik-error`, `ik-note`, `ik-fail`,
  `ik-live(.on)`/`ik-live-dot`, `ik-toast`.
- **Utilities:** `ik-muted`, `ik-mono`, `ik-flex`.

**Shared Rust components:** `Shell`, `Rail`, `TopBar`, `Cover`, `CoverCard`, `PanelCard`,
`SkeletonGrid`/`SkeletonRows`/`SkeletonBlock`, `ErrorBox`/`ErrorLine`, `EmptyBox`, `OutcomeLine`,
`SignInGate`, `Brush`, plus `async_view` / `async_list` — the two helpers that render every
fetched resource's loading / error+retry / empty / content states. Roughly thirty call sites used
to open-code that match; **new data surfaces must go through these helpers** or the "a failed
fetch is always visible and always retryable" property stops holding.

**Icons:** inline SVG only — one `Icon` enum + `Ic { icon, size }` component (`src/icons.rs`),
24×24 Lucide paths drawn in `currentColor`. No icon font, no Material glyph names (the spec's
`menu_book`-style names map onto enum variants), no Phosphor.

---

## 8. Quality floor currently honoured

- Keyboard focus: `:focus-visible { outline: 2px solid var(--acc); outline-offset: 2px }` on
  buttons, nav links, chips, tabs, bell.
- Loading is **skeletons**, never spinners on content; errors name the failure and offer retry;
  empty states are separate from errors.
- `prefers-reduced-motion: reduce` kills all animation.
- Drag is never the only mover (watchlist `<select>` fallback).
- Optimistic toggles with rollback on failure (watchlist / notify).
- Body-size accent text uses `--acc3` on a tint, not `--acc` on the ground.
- Responsive: ≤980px collapses the series body grid; ≤820px the rail becomes a horizontal strip,
  the cover grid goes 2-up, hero cover shrinks to 120px, content padding drops to 20/16px.
- i18n: every string resolves through the catalogue (`locales/en.json`, `de.json`); the wordmark
  is deliberately not translated.

---

## 9. Where to change what

| Change | Touch |
| --- | --- |
| Any color, radius, spacing, font | `input.css` `:root` / `@theme` blocks, then rebuild `assets/main.css` |
| A new component style | plain CSS **below** the Tailwind import in `input.css` (or it gets purged) |
| Light-mode value | `[data-theme="light"]` block |
| A new appearance knob | `state/prefs.rs` + the `[data-*]` block + `index.html` boot script + `account/appearance.rs` |
| A new icon | add an `Icon` variant + path in `src/icons.rs` |
| A new screen | `Route` enum in `src/app.rs` + a view module + a capability-gated `NavLink` |
| A new console tab | `ConsoleTab` enum: label key, icon, and `requires() -> (Permission, Feature)` |
| A new fetched surface | wrap it in `async_view` / `async_list`; add skeletons, never a spinner |
| Copy | `web/frontend/locales/*.json`, both languages |

---

## 10. Known divergences & gaps (design-relevant)

1. **Nocturne is not the frontend's system.** `docs/frontend/project/_ds/nocturne-*` (blurple
   `#9184d9`, Inter, Phosphor, outlined buttons) is mockup scaffolding. The shipped language is
   the vermilion/Inkstone one above; primary buttons here are **filled**, not outlined.
2. Per-series cover hue derivation is missing (one global gradient instead).
3. `docs/design.md` §17.1 still names Clash Display / Zodiak for display type; the code ships
   Bricolage Grotesque.
4. Rating, per-chapter read state and read-% have no API and are omitted rather than faked;
   related series is a placeholder.
5. Password change and 2FA are stated as unavailable rather than stubbed with dead controls.
6. `ikspin` is declared in the spec but intentionally unused.
7. Console tab count is **11**, not the spec's 8 (Sync admin, Feature flags, Privacy queue added).
8. Account has **6** panels, not the spec's 5 (Privacy & data added).
