# TankoVault — Design Specification (enhanced)

> **What this is.** The design system the SPA is built to: tokens, component inventory,
> per-screen breakdowns and interaction rules. It was extracted from the original HTML mockup,
> which is no longer kept in the tree — this document replaced it, and `web/frontend/input.css`
> plus `web/frontend/crates/inkstone-ui` are the implementation.
>
> Source files cite it by section (`DESIGN_SPEC §7.1`), so section numbers here are load-bearing.
> Where the shipped UI and this document disagree, the code is right and this is the bug.

---

## 1. Design language

A calm, high-contrast, **dark-first** reading environment — closer to print manga and sumi-e
ink than the usual neon-on-black tracker. Contrast comes from tonal ramps, not saturation.
Vermilion is the single bold accent (a hanko-seal red), used as a **line and a glow** —
active-nav stroke, unread badges, primary CTAs, progress fills — never as a flood. A second
jade/teal role carries "healthy / synced / up-to-date". This is a direct evolution of the
project's existing **"Inkstone"** direction (`docs/design.md` §17.1): same palette, same
philosophy, richer surface.

**Signature devices** (use sparingly, keep everything else quiet):
- Active-nav **3px vermilion bar** on the left edge, animated `scaleX` from the left.
- Cover cards with a **hue-derived gradient** placeholder and a bottom scrim carrying
  source-count + rating.
- **Mono numerics** everywhere counts/dates/IDs appear (fast numeric scanning).
- A pulsing **live dot** for real-time surfaces (console, SSE).

---

## 2. Color tokens

### 2.1 Ground & surface (dark, default)

| Token | Hex | Role |
| --- | --- | --- |
| `--bg` | `#0B0E13` | App background (near-black, faintly blue) |
| `--rail` | `#0C1016` | Sidebar / filter panel / table header ground |
| `--surface` | `#12171E` | Raised cards, inputs, buttons (secondary) |
| `--surface-2` | `#0E131A` | Deeper cards (console tiles, chapter list, settings) |
| `--surface-feed` | `#0F141A` | Feed day-header ground |
| `--surface-unread` | `#101720` | Unread notification row |

### 2.2 Borders & hairlines

| Token | Hex | Role |
| --- | --- | --- |
| `--border` | `#1A212A` | Structural dividers (rail, header, panel edges) |
| `--border-ctl` | `#232A33` | Control borders (inputs, buttons, chips) |
| `--border-row` | `#161C24` | Row dividers inside lists/tables |
| `--border-soft` | `#1E262F` | Progress-bar track, subtle card borders |

### 2.3 Text ramp

| Token | Hex | Role |
| --- | --- | --- |
| `--text` | `#F2EFE9` | Primary text (warm off-white) |
| `--text-2` | `#c9cfd6` | Secondary body (synopsis) |
| `--text-3` | `#B8C0CA` | Control labels, secondary buttons |
| `--muted` | `#8A94A0` | Secondary / meta text |
| `--faint` | `#5c6672` | Timestamps, placeholders, kbd hint |
| `--faint-2` | `#4d5763` | Nav group headers |
| `--icon-off` | `#7c8794` | Inactive nav icon |

### 2.4 Accent (vermilion — default)

| Token | Hex | Role |
| --- | --- | --- |
| `--acc` | `#E4572E` | The accent: active nav, CTAs, unread, progress start |
| `--acc2` | `#f07a56` | Lighter accent: progress end, "manhwa" type |
| `--acc3` | `#f2a993` | Accent text on tinted surfaces (chips, pills) |
| `--acc-dk` | `#b83a17` | Gradient dark stop (logo, buttons) |

Tinting convention seen throughout: fill = `color-mix(in srgb, var(--acc) 10–18%, transparent)`,
border = `var(--acc)` or `color-mix(... 40%, transparent)`, text = `var(--acc3)`.

### 2.5 Semantic role colors

| Purpose | Value(s) |
| --- | --- |
| Jade base / bright | `#2E8B78` / `#3DA88F` (success, synced, "read on" layers icon) |
| **Content type** | manga `#6FA8DC` · manhwa `var(--acc2)` · manhua `#3DA88F` · webtoon `#CBA43C` |
| **Series status** | ongoing `#3DA88F` · completed `#6FA8DC` · hiatus `#CBA43C` · cancelled/unknown `#8A94A0` |
| **Provider state** | active `#3DA88F` · degraded `#CBA43C` · challenged `#DB4A2B` · solving `#6FA8DC` · blocked `#c0392b` · disabled `#8A94A0` |
| **Run state** | running `#6FA8DC` · completed `#3DA88F` · failed `#DB4A2B` · queued `#8A94A0` |
| Rating star | `#CBA43C` |
| Exclude-tag | border/text `#a94436` |

Pill recipe (state chips): `background: {color}22; border: 1px solid {color}55; color: {color}`,
mono, uppercase, `letter-spacing:.05em`, `border-radius:20px`, `padding:3px 9px`.

### 2.6 Cover gradient recipe (placeholder art)

Each series derives two hues (`H`, `H2 = (H+40+rand·60) mod 360`). The **cover style** knob
picks the recipe:

- **ink** (default): `linear-gradient(155deg, hsl(H 44% 27%) 0%, hsl(H2 40% 13%) 60%, hsl(H2 34% 8%) 100%)`
- **duotone**: `linear-gradient(155deg, hsl(H 20% 27%) 0%, hsl(H 26% 15%) 45%, hsl(H 34% 8%) 100%)`
- **vivid**: `radial-gradient(120% 120% at 20% 0%, hsl(H 74% 46%) 0%, hsl(H2 80% 26%) 55%, hsl(H2 72% 12%) 100%)`

A large `Bricolage Grotesque` initial at `rgba(255,255,255,.09–.13)` sits centered as the
fallback glyph. In production this is the placeholder behind a lazy-loaded `cover_url`.

---

## 3. Typography

| Role | Family | Weights | Used for |
| --- | --- | --- | --- |
| Display | **Bricolage Grotesque** | 400–800 | Wordmark, page/hero titles, section headers, KPI values, cover initials |
| Body / UI | **IBM Plex Sans** | 400/500/600/700 | All UI text |
| Data / labels | **IBM Plex Mono** | 400/500/600 | Chapter numbers, counts, timestamps, IDs, uppercase kicker labels, `⌘K` hint |

> The current codebase's `docs/design.md` §17.1 names *Clash Display / Zodiak* for display.
> The mockup uses **Bricolage Grotesque**. Treat Bricolage as the canonical display face and
> update §17.1 (see plan §3).

**Observed type scale (px):**

| px | Usage |
| --- | --- |
| 9–11 | Mono pill/kicker labels, badges |
| 12–13 | Secondary/meta text, chip labels |
| 14–15 | Body, buttons, card titles |
| 16–20 | Section subheads, sidebar headings |
| 22–28 | Page titles, stat numbers |
| 26–30 | KPI values |
| 34–38 | Hero titles (`-0.02em` tracking) |
| 56–96 | Cover fallback initials |

Line-height: `1.05` on hero titles, `1.3` on card titles, `1.55–1.7` on body/synopsis.

---

## 4. Spacing, radius, elevation

- **Radius:** pills `20px`/`999px`; cards `12–16px`; controls `8–11px`; small chips `6–8px`.
- **Grid density knob** drives the discover grid: cozy `card:198px / gap:22px` · standard
  `158/18` · compact `124/12`. Grid = `repeat(auto-fill, minmax(var(--card), 1fr))`.
- **Elevation:** covers `box-shadow: 0 8px 22px rgba(0,0,0,.35)`; hero cover
  `0 20px 50px rgba(0,0,0,.55)`. On this dark ground, elevation = edge + ambient shadow.
- **Sidebar** `250px` fixed · **filter panel** `298px` · **series sidebar** `340px` ·
  **settings sub-nav** `236px` · **header** `64px` sticky with `backdrop-filter: blur(14px)`.

---

## 5. Motion

| Keyframe | Effect | Applied to |
| --- | --- | --- |
| `ikfade` | opacity 0→1, translateY 8px→0, `.3–.4s` | View mount (each screen root) |
| `ikbar` | `scaleX(0)→(1)` from left, `.25s` | Active-nav bar |
| `ikpulse` | opacity `.5↔1`, `1.4–1.6s` loop | Live dots (console, active run) |
| `ikflow` | background-position shift, `2s` linear loop | Active scan progress bar (barber-pole) |
| `ikspin` | `rotate(360deg)` | (declared; spinners — avoid on content) |

All decorative motion must degrade under `prefers-reduced-motion: reduce` (quality floor §7).

---

## 6. Component inventory

Grouped by where they appear. Each maps to a Dioxus component in the plan.

### 6.1 App shell
- **Sidebar** (`250px`): brand lockup (gradient tile + `menu_book` glyph + `Tankō`**`Vault`** +
  mono tagline `SOURCE · TRACK · SYNC`); grouped **nav** (`MAIN` / `LIBRARY` / `OPERATOR`
  kicker headers, items with icon + label + optional count badge + active bar); **user footer**
  (circular avatar initial, username, status dot + `admin · synced`, settings gear → Account).
- **Header** (`64px`, sticky, blurred): search input (`max 520px`, leading `search` icon,
  trailing `⌘K` kbd chip, placeholder `Search 8,420 series, tags, authors…`); flex spacer;
  **notifications bell** with count badge. (The header's "AniList synced" pill was removed —
  link status belongs to Account → Sync, which is the surface that can act on it.)

### 6.2 Cards & rows
- **Cover card** (discover/search): 2:3, type pill (TL), unread badge (TR), bottom scrim with
  `layers` source-count + `star` rating; title (2-line clamp) + status dot + chapter count.
- **Continue-reading card**: 64×90 cover, title, `ch N · via Provider`, progress bar, `% read`,
  `Next: N →`.
- **Feed row**: 38×52 cover, title, detail line, `+N ch` accent tag, relative time.
- **Rec cover card** (horizontal rail): 148×210, type pill, source-count, title, match line.
- **Watchlist card**: 40×56 cover, 2-line title, `ch N`, notify bell + unread count.
- **Notification row**: kind icon tile, cover thumb, `Title — text`, sub, relative time, unread
  left-bar + darker ground.
- **Source card** ("Read on"): 36×36 provider tile, name + `PRIMARY` badge, chapter count,
  `open_in_new`.
- **Related card** ("Readers also follow"): 2:3 cover + title, 2-up grid.
- **KPI tile**: label + icon, big mono value (role-colored), sub-line.
- **Provider health tile**: name + state pill, `series / solve%`, solve bar, last-scan age,
  left accent border in state color.
- **Run row**: state pill, provider + mode, run id + started, mini progress + `done/total`.
- **Failed-task card**: `provider · kind`, target path, red error line, left accent border.
- **Merge card**: `match %` + reason, A vs B compare (cover + meta + "Keep this"), `sync_alt`
  divider, "Not a duplicate".
- **User row / audit row / session row / pref row**: table/list rows (see screens §7.7–7.9).

### 6.3 Controls
- **Filter chip** (type/status): tinted when active (`acc 16%` fill, `--acc3` text).
- **Tag chip (3-state cycle)**: neutral → include (`+`, acc) → exclude (`−`, `#a94436`,
  line-through) → neutral. Click cycles.
- **Provider checkbox**: 16px box (checked = acc fill + white `check`), label, count.
- **Range slider** (year min/max, min-chapters): native `input[type=range]`, `accent-color:
  var(--acc)`.
- **Sort select**: native `<select>`, 6 options (updated / title / chapters / sources / rating /
  year).
- **Tab strips**: notifications (all/unread/chapters/sync), console (8 tabs), settings sub-nav
  (5). Active = tinted bg + acc border + acc text.
- **Toggle switch**: `40×22` track (acc when on), sliding knob.
- **Buttons**: primary (acc fill, white), secondary (surface + ctl border), icon (square 32–44),
  block. Chips/pills as above.
- **Text input / textarea**: surface bg, ctl border, `10px` radius; adapter config is a mono
  `340px` textarea.
- **Active-filter chip**: removable (`× close`), acc-tinted rounded.
- **Pagination**: Prev/Next + numeric page dots (active tinted) + mono `cursor:` hint.

### 6.4 States
- **Empty**: centered `search_off` glyph, headline, hint, "Reset filters" CTA.
- **Skeleton**: shimmer placeholders (never spinners on content).
- **Live pill**: dot (pulse when on) + `Live · 4s` / `Paused`.

---

## 7. Screen specs

The mockup exposes **9 screens** via a single `view` state. Nav order:
`MAIN → Home · Discover · Search` / `LIBRARY → Watchlist · Notifications` /
`OPERATOR → Console · Account`.

### 7.1 Home (dashboard) — *new screen*
Landing for signed-in readers.
1. **Header block**: mono greeting (time-of-day) + `Welcome back, {name}`; right-aligned **3
   stat tiles** — new chapters (acc), reading (count), chapters read (jade).
2. **Continue reading**: `play_circle` heading + grid of continue-reading cards
   (`auto-fill minmax(300px,1fr)`).
3. **New in your watchlist**: `bolt` heading + "See all" → Notifications; card containing
   **day-grouped feed rows** (`TODAY` / `YESTERDAY` headers).
4. **"Because you read X" rows**: one or more horizontal scroll rails of rec cover cards, each
   with an `auto_awesome` jade heading + "recommended for you".

### 7.2 Discover
Two-pane: collapsible **filter panel** (`298px`) + **results**.
- **Filter panel**: `tune` "Filters" + "Reset"; active-filter count line; **CONTENT TYPE**
  chips; **STATUS** chips; **GENRES / TAGS** with `match: any/all` toggle + 3-state tag chips
  (include/exclude legend); **PROVIDER** checkboxes with counts; **RELEASE YEAR** dual range
  (`min–max`); **MIN. CHAPTERS** range; **SAVED PRESETS** list.
- **Results**: panel-toggle icon button, "Discover" title, **Sort** select; active-filter
  removable chips; `{N} series · page {p} of {n}` count; cover-card grid; **pagination** (Prev
  / page dots / Next + `cursor:` hint). **No-results** empty state with reset.
- Filtering logic in the mock is client-side over generated data; **production requires
  server-side filter/sort/pagination** (see plan §7.2 + §9).

### 7.3 Series detail
- **Hero**: blurred cover backdrop (`opacity .20; blur 30px`) + bottom fade; Back button;
  `186×270` cover; type pill + status dot + year; `38px` title; `by {author} · {alt}`; tag
  chips; **stat row** (rating★ / chapters / sources-layers); **actions** — `Continue ch N`
  (primary), `In watchlist` (bookmark), notify bell icon.
- **Body grid** (`1fr 340px`):
  - Left: **Synopsis**; **Chapters** header with read-progress bar + `%`; chapter list rows
    (`Ch N` mono, optional title, provider, date, unread dot / read check).
  - Right sidebar: **Read on** source cards (PRIMARY badge, chapter count, `open_in_new`);
    **Tracking** card (Status, Notify toggle, AniList synced); **Readers also follow** 2-up
    grid.
- **Data gaps vs. current API**: rating, author, alt-titles, tags, per-source `is_primary`,
  per-chapter read-state, and read-% are **not** in `SeriesDetail`/`ChapterDto` today
  (plan §9).

### 7.4 Watchlist
- Header: title + subtitle ("Drag a title between columns…"), `Sync AniList` + `New collection`
  buttons.
- **Kanban**: horizontally-scrolling columns — Reading (acc / `local_fire_department`), Planned
  (`#6FA8DC` / `schedule`), Completed (jade / `task_alt`), Paused (`#CBA43C` / `pause_circle`),
  Dropped (`#8A94A0` / `cancel`). Each: icon + label + count; draggable cards; column
  highlights on drag-over; drop moves status.
- **HTML5 drag/drop** (`draggable`, `onDragStart/Over/Leave/Drop`). Keep the `<select>` mover as
  the accessible fallback.

### 7.5 Notifications
- Header: title + `{N} unread · live push via SSE`; "Mark all read".
- **Filter tabs**: All / Unread / Chapters / Sync.
- **Rows**: kind icon tile (new_chapter `auto_stories` acc, source_added `add_link` `#6FA8DC`,
  completed `task_alt` jade, sync `sync` jade), cover thumb, `Title — text` + sub, relative
  time, unread left-bar.

### 7.6 Search
Big `56px` search field, `{N} results · trigram fuzzy match`, cover-card grid (reuses discover
card at a smaller size). Instant, keyboard-navigable.

### 7.7 Account & settings — *new screen*
Sub-nav (`236px`) + panel:
- **Profile**: avatar + identity, editable fields (display name, username, email, default
  language), 3 lifetime stat tiles.
- **Security & sessions**: password/2FA card; **active sessions** list (device, location, age,
  "this device" badge, Revoke / Revoke all).
- **Sync & integrations**: AniList connection card (pull/push, conflict policy segmented:
  Local / Remote / **Newest** wins); cross-device sync status.
- **Notification prefs**: toggle rows (new chapter, new source, sync events, email digest,
  Discord webhook).
- **Appearance**: theme picker (Inkstone Dark / Warm Paper) — maps to the real light/dark
  toggle; optionally accent/density/cover-style knobs (§8).
- **Backend gaps**: profile update, sessions, AniList, 2FA, prefs have **no endpoints today** —
  build the shell, stub actions with clear TODO markers (plan §9).

### 7.8 Operator Console
`space_dashboard` header (role: admin), **live** pill + pause/refresh, **Trigger scan**. Design
uses a **tab bar** (the current code stacks all panels — plan §7.8 introduces tabs):
1. **Overview**: 9 KPI tiles + provider-health tiles.
2. **Live scans**: active-run banner (barber-pole progress, done/total/failed/ETA), recent runs
   list, failed-task triage.
3. **Providers**: table with inline-editable `base_url` (domain migration), adapter kind, state
   pill, scan/edit/disable actions, "Add provider".
4. **Challenge & solver**: active back-end card (TRAWL), 7d solve-success, per-provider
   solve bars + "Re-solve".
5. **Adapter test**: `providers.config` JSON textarea + "Test adapter (dry-run)" → parsed-sample
   panel side-by-side.
6. **Merge queue**: side-by-side A/B compare, Keep / Not-a-duplicate.
7. **Users**: table (user, email, role, tracked, state) — **no list endpoint today**, stub.
8. **Audit**: action / object / who · role / age list.

### 7.9 Auth
Centered `400px` card: wordmark, "Sign in to sync your library", email/username + password,
"Sign in", "Create an account" link.

---

## 8. Theme knobs (optional, from the mockup's `data-props`)

| Knob | Options (default **bold**) | Effect |
| --- | --- | --- |
| `accent` | **vermilion** / amber / jade / azure / amethyst | Swaps `--acc*` set |
| `density` | cozy / **standard** / compact | Swaps `--card` / `--gap` |
| `coverStyle` | **ink** / duotone / vivid | Swaps cover gradient recipe |

Accent palettes: vermilion `#E4572E`, amber `#D98A2B`, jade `#2E9B7F`, azure `#3E86C9`,
amethyst `#8B5CF6` (each carries `a / lt / tx / dk`). These are low-priority polish; wire them
as CSS-variable swaps if/when Appearance ships.

---

## 9. Accessibility & quality floor (must hold)

Carried from `docs/design.md` §17.3 — the redesign must not regress:
- Responsive to mobile; cover grid reflows to 2-up; sidebar collapses.
- Visible **keyboard focus rings** (`:focus-visible` 2px accent, offset 2px); full keyboard nav
  of lists, chips, and the command bar.
- `prefers-reduced-motion` respected (brush/live/flow animations degrade to static).
- Loading = **skeletons**, never spinners on content; errors **name what failed + how to
  retry**; empty states are invitations.
- **Optimistic** watchlist/progress/notify toggles with rollback on failure.
- **WCAG AA** contrast for all text/bg pairs; vermilion body-size text uses `--acc3` on tint,
  not `--acc` on ground.
- Copy is plain and action-named ("Add to watchlist" → toast "Added to watchlist").

---

## 10. Source-of-truth notes for implementers

- Every hex/px above was read from the original mockup rather than guessed, and the shipped
  values live in `web/frontend/input.css` — check there before trusting a number here.
- The mockup's data (series, providers, runs, users, sessions) was generated client-side for
  preview only: it defines *shape and states to support*, not real content.
