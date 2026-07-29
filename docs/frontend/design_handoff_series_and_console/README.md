# Handoff: TankoVault series page + operator console (turn 3)

## Overview

Three redesigned surfaces for TankoVault, a self-hosted manga aggregator:

1. **Series detail page** — the chapter list becomes the primary surface, with all read sources collapsed behind **one merged open control** per chapter, and a reworked **Tracking** sidebar (progress editor, external trackers with link/unlink, conflict resolution, personal fields, alerts, history).
2. **Operator console · Providers** — master–detail: entity rail → filtered provider list → deep inspector with inline config editing, dry-run-before-save, live politeness controls, and a gated danger zone.
3. **Operator console · Users** — same shell: identity edits, grouped permission grants, external-sync unlink, session revocation, export, and gated erasure.

The problems being solved: sources were previously a separate panel with a per-source open button (too many clicks, unclear which source you'd land on); tracking was a thin status card; the console was eleven stacked read-mostly panels with little direct editing and no way to reverse a merge or a sync link.

## About the design files

The files in this bundle are **design references authored in HTML** — prototypes showing intended look, structure and behaviour. They are **not production code to copy**. The runtime they use (`support.js`, `<x-dc>`, `style-hover` attributes) is a preview harness and must not be ported.

The task is to **recreate these designs in the existing codebase**: `web/frontend/` — a **Dioxus** (Rust → WASM) app styled by **Tailwind v4** plus a hand-authored `ik-*` component layer in `web/frontend/input.css` (compiled to the committed `assets/main.css` via `npm run css:build`). Use that established vocabulary — `ik-card`, `ik-btn`, `ik-pill`, `ik-tabs`, `ik-table`, `ik-chapter`, `ik-live`, the `--acc` / `--surface` / `--border-*` token set — rather than the literal hex values written into the mock. Every literal in the mock was read out of `input.css` in the first place; the mapping is in **Design tokens** below.

`FRONTEND_AS_BUILT.md` (included) documents the frontend as it exists today: token set, class inventory, screen-by-screen structure, and where to change what. Read it before starting.

## Fidelity

**High-fidelity.** Colors, typography, spacing, radii, states and copy are final and traceable to `input.css` and `src/icons.rs`. Recreate the UI faithfully — but through the existing `ik-*` classes and CSS variables, not by hard-coding hexes. Mock data (series titles, provider names, run ids, users) is illustrative only; it defines shape and states to support, not content.

Deliberately **not** covered by these mocks: skeleton/empty/error variants, the ≤820px responsive collapse, Warm Paper (light) theme verification, and undo toasts. Reuse the existing `SkeletonGrid` / `SkeletonRows` / `SkeletonBlock` / `ErrorBox` / `EmptyBox` components and the `async_view` / `async_list` helpers for those — every new fetched surface must go through those helpers.

---

## Screen 1 — Series detail (`3a`)

**Route:** `/series/:id` (`web/frontend/src/views/series.rs`)
**Purpose:** decide what to read next and open it in one click; keep local and remote progress honest.

### Layout

- App shell unchanged: `.ik-app` grid `250px 1fr`; sticky rail; 64px sticky top bar with `backdrop-filter: blur(14px)`.
- Hero band: `padding: 26px 30px 0`, subtle vertical gradient standing in for the blurred cover backdrop (production keeps `.ik-hero-wrap` + `.ik-hero-bg`: `opacity .20; blur(30px); scale(1.1)` plus the bottom fade).
- Hero grid: `186px 1fr`, `gap: 24px`. Cover 186×270, `border-radius: 14px`, `1px solid #232a33`, `box-shadow: 0 20px 50px rgba(0,0,0,.55)`, typographic fallback initial at `rgba(255,255,255,.10)`, Bricolage 68px.
- Body grid: `1fr 340px`, `gap: 26px`, `padding: 22px 30px 30px`.
- Left column: synopsis → chapter header → progress bar → filter chips → chapter table → footnote.
- Right column: Tracking card, then "Readers also follow" card. Both `background: #0e131a` (`--surface-2`), `1px solid #1a212a`, `border-radius: 14px`, `padding: 16px`.

### Hero components

- Back button: `.ik-btn` with the `Back` glyph, 12.5px/600.
- Type pill: `.ik-pill` in the content-type color (manga `#6fa8dc`) — fill `12%`, border `50%`, text the role color.
- Status: 7px dot in `#3da88f` + `ongoing`, mono 12px; year in `--faint`.
- Title: Bricolage **600**, 38px, `line-height 1.05`, `letter-spacing -.02em`. Never bolder than 600.
- Byline: 14px IBM Plex Sans, `--muted`; author · alt titles joined by `·`.
- Tag chips: 12px, `#12171e` on `1px solid #232a33`, radius 8px, padding `6px 10px`.
- Stat row: rating (Star glyph in `--star` `#cba43c`), chapter count, source count (Layers glyph in `#3da88f`) — all mono 14px, `gap: 20px`.
- **Primary action = split button.** Left half: `background #e4572e`, white, 600/14px, `padding 12px 16px`, label `Continue ch 152.6 on Asura` + `OpenInNew` glyph. Right half: `#b83a17`, `1px solid rgba(255,255,255,.2)` divider, `padding 0 11px`, `ChevronRight` rotated 90°. Whole control `border-radius: 10px; overflow: hidden`, `box-shadow: 0 6px 18px rgba(228,87,46,.28)`. Hover: `filter: brightness(1.07)` on the main half, `#e4572e` on the caret half.
- Secondary: `In watchlist` (`.ik-btn` + Bookmark glyph) and a 44×44 icon button with the bell in `--acc`.

### Chapter list (the core change)

Header row: kicker `CHAPTERS`, then `158 chapters · 152 read` (Bricolage 600/21px). Right-aligned: mono `opens on`, then **`AsuraScans → MangaDex`** with a `change` link — the resolved preference, read-only here; editing happens in settings.

Progress bar: 6px, track `#1e262f`, fill `linear-gradient(90deg,#e4572e,#f07a56)`, radius 20px.

Filter chips: `All 158` active (`.ik-chip.active`: fill `rgba(228,87,46,.14)`, border `--acc`, text `--acc3`), `Unread 6`, `Hide parts`; right side `newest first ⌄`.

Table: `1px solid #1a212a`, radius 12px. Every row is a grid: **`16px 78px 1fr 86px 92px 148px`**, `gap: 12px`, `padding: 11px 14px`, `border-bottom: 1px solid #161c24` (last row none), row hover `background: #0e131a`.

| Column | Content |
| --- | --- |
| 1 | unread = 8px `--acc` dot; read = `Check` glyph 13px in `#3da88f` |
| 2 | `ch 152.6` — mono 13.5px/500; accent on the next-unread row, `--text-3` otherwise |
| 3 | chapter title 14px (600 + `--text` on next-unread, 400 + `#c9cfd6` elsewhere); sub-line `next up · unread` mono 11px |
| 4 | **source monograms** — 22×22, radius 6, `#12171e` on `#232a33`, Bricolage 700/8.5px; the preferred source's monogram is `--acc3`, the rest `--muted`; overflow as `+2` in mono 9px |
| 5 | freshness — mono 12px, `#3da88f` when hours-fresh, `--faint` when old |
| 6 | the merged open control |

**The merged open control** — one per row, no per-source buttons anywhere:
- Next-unread row: filled split button (`#e4572e` / `#b83a17` caret), label `Open` + `OpenInNew`.
- All other rows: ghost split button — `#12171e` on `1px solid #232a33`, text `--text-3`, caret half divided by `1px solid #232a33`; hover lightens text to `--text` and background to `#0e131a`.
- Clicking the main half opens the **highest-ranked source that actually carries that chapter**, in a new tab. Clicking the caret opens the per-chapter source menu (spec below).
- Part-release rows (`ch 151.6`) may show `Open on MD` when only the backup carries them.

Part releases: a collapsed toggle row — `background: #0e131a`, rotated `ChevronRight`, `3 part releases between 151 and 152`, 12.5px `--muted`; expands to indented rows (`padding-left: 30px`, `--surface-2` ground, a `part` pill in `.ik-part-pill` style).

List footer: `Older chapters · 132 → 1` with a right-aligned `load 25 more`.

Footnote under the table, 12px `--faint`: "Monograms show who carries the chapter — the button opens the highest-ranked source that does."

### Per-chapter source menu (caret target)

Anchored to the caret, 290px wide, `#12171e` on `1px solid #232a33`, radius 12px, `box-shadow: 0 18px 44px rgba(0,0,0,.6)`. In the mock it renders to the **left** of the button so the rows behind stay legible; in production, standard below-right anchoring with viewport flipping is fine.

Structure:
1. Header: mono kicker `Open ch 152.6 on`.
2. Preferred row — `rgba(228,87,46,.10)` fill, `3px solid #e4572e` left bar, 24×24 monogram tile, name 600/12.5px, sub `preferred · 3h ago` in `--acc3`, trailing `Check` glyph in `--acc`.
3. Other sources that carry the chapter — sub `backup · 9h ago`; trailing 24×24 ghost icon button with the `Bookmark` glyph = **pin as preferred for this series**.
4. Sources that do **not** carry it — `opacity: .5`, sub explains why: `only up to ch 151`, `challenged · solving`. Not clickable.
5. Footer on `#0c1016`: `Change source order` link + mono hint `↵ opens preferred`.

Rows are `9px 13px`, separated by `1px solid #161c24`, hover `#0e131a`.

### Tracking sidebar

Header: MenuBook glyph in `--acc`, `Tracking` (Bricolage 600/17px), right-aligned `1 conflict` pill in accent.

1. **Conflict card** — `1px solid rgba(228,87,46,.40)`, fill `rgba(228,87,46,.06)`, radius 12px. `CloudSync` glyph + `AniList is 3 chapters behind`. Two side-by-side value boxes (`Here` / `AniList`, each `#12171e` on `#232a33`, radius 9, kicker mono 10px + Bricolage 600/15px). Actions: `Push ch 152` (filled accent), `Take 149` (ghost), `Trust newest` (bare text button, hover `--acc3`).
2. **Progress** — kicker + right-aligned `152 / 158`. Stepper: `−` / value / `+` in a single `1px solid #232a33` group, radius 10, 34px tall, value mono 13.5px centered in a 52px cell; then a full-width `Mark up to here read` ghost button. Saves optimistically.
3. **Trackers** — one row per registered tracker (data-driven from `/v1/me/sync/providers`, never hardcoded). 28×28 monogram tile (linked = jade tint + jade border; unlinked = neutral), name 600/13px, sub `pushed 149 · 6d ago` or `not linked`, trailing `Unlink` / `Link` ghost button (Unlink hover → `--acc3`). Footer strip on `#0c1016`: `mapped to #141017` + `Change`.
4. **Your notes** — 2×2 grid of `#12171e` boxes (Score in `--star`, Rereads, Started, Finished; em-dash when unset), then a notes box, 12.5px/1.55 `#c9cfd6`.
5. **Alerts** — three toggle rows (`New chapter`, `New source added`, `Series completed`). Switch = 38×21 track, radius 20; on = `--acc` with the 15px white knob at `left: 20px`; off = `#232a33`, knob at `left: 3px`.
6. **History** — left-rule timeline (`1px solid #232a33`, `padding-left: 12px`), rows mono 11.5px: `ch 152 · 2h ago · you, web` with the changed value in `--text`.

"Readers also follow": kicker + 2-up grid of 2:3 gradient placeholders with 12px titles.

---

## Screen 2 — Console · Providers (`3b`)

**Route:** `/console` (`web/frontend/src/views/console/`)
**Purpose:** run the fetch pipeline: see provider health, change config safely, control politeness, pause or remove a provider.

### The console shell (shared by every entity)

- Header bar 56px, `#0c1016`, `1px solid #1a212a` bottom: 26px gradient brand tile with the MenuBook glyph → `Console` (Bricolage 600/15px) → mono breadcrumb `/ providers` → a 420px-max `⌘K` jump field (`#12171e` on `#232a33`, radius 10, Search glyph, right-aligned kbd chip) → right side: `Live · 4s` pill (`.ik-live.on`: jade text, `rgba(46,139,120,.45)` border, pulsing 8px dot) and the primary action.
- Body grid **`186px 328px 1fr`**, `min-height: 660px`.
- **Pane 1 — entity rail**, `#0c1016`, `padding: 14px 10px`. Mono kicker groups: `CATALOGUE` (Series, Chapters, Merge queue), `PIPELINE` (Providers, Scan runs, Failed tasks, Solver), `PEOPLE & POLICY` (Users, Sync links, Feature flags, Privacy, Audit). Entry: `padding 8px 11px`, radius 9, label 500/13.5px, right-aligned count in mono 11px — accent-tinted counts for anything needing attention (`14` merges in `--acc3`, `2` privacy in `--star`, live runs jade with a dot). Active entry: `rgba(228,87,46,.10)` fill + absolute 3px `--acc` left bar (the existing `.ik-nav-link.active` recipe). Rail visibility stays capability-gated: `caps.can(permission) && caps.has_feature(feature)`.
- **Pane 2 — entity list**, `1px solid #1a212a` right border, flex column. Sticky search/filter header; optional bulk-selection bar; rows; footer pinned with `margin-top: auto` carrying the count and `↑↓ move · ↵ open`.
- **Pane 3 — inspector**: header (identity + actions + tab strip) over a `1fr 1fr` content grid, `padding: 18px 22px 22px`, `gap: 20px`.

Selected list row: `rgba(228,87,46,.08)` fill + `3px solid #e4572e` left border.
Bulk bar (when a selection exists): `rgba(228,87,46,.10)` strip, `3 selected` in mono `--acc3`, right-aligned small ghost actions; the destructive one gets an accent border.
Checkboxes: 15×15, radius 4 — checked = `--acc` fill + 11px white `Check`; unchecked = `1px solid #232a33`.

### Provider list rows

Name 600/13.5px + state pill (`active` jade / `degraded`+`challenged` `--star` / `blocked` `#c0392b` / `disabled` neutral), sub-line mono 10.5px `3,104 series · solve 98% · scanned 3h`, then a 4px solve-rate bar (`#1e262f` track; fill jade ≥95%, `--star` below). Disabled providers at `opacity: .6`, restored on hover.

### Provider inspector

Header: 44×44 monogram tile, name Bricolage 600/24px, state pill, mono meta line (`generic_config · 3,104 series · 41 new ch today · scanned 3h ago`); actions `Scan now`, `Pause` (ghost) and `Save changes` (filled). Tabs: **Config · Politeness · Coverage · Runs · Danger** (`.ik-tabs` recipe: active = accent border + `rgba(228,87,46,.12)` + `--acc3`).

Left column:
- **Identity** — label/field rows on a `96px 1fr` grid. `base_url` is inline-editable and shown focused (`1px solid #e4572e` + `0 0 0 3px rgba(228,87,46,.18)`); beneath it a `--star` warning: "domain change — 3,104 series will be re-pathed on the next scan". `adapter` is a segmented control (`generic_config` / `madara` / `custom`, active pill `#232a33`). `language` a select.
- **Adapter config** — JSON in a `#0b0e13` block, `1px solid #232a33`, mono 12px/1.65, `white-space: pre`; status `json · valid` in the header. Buttons `Dry-run`, `Format`, plus the rule stated in the UI: **save is blocked until a dry-run passes**.

Right column:
- **Dry-run result** — `18 parsed · 0 errors` jade pill + a 3-row sample table (`1fr 76px 84px`: title / chapters / latest), header on `#0c1016`, mono numerics.
- **Politeness** — `rps`, `concurrency`, `crawl delay` as slider rows (4px `#1e262f` track, `--acc` fill, 14px `--acc` knob with a 2px ground-colored ring, mono value right-aligned in a fixed-width cell), plus a `user agent` text field. Copy states the semantics: "applies to the next task".
- **Danger** — container `1px solid rgba(228,87,46,.40)`. `Blocklist this provider` acts inline (reversible). `Delete provider` names the exact consequence ("Drops 3,104 source links and 118k chapter rows… anything only this provider carried becomes unreadable") and is **type-to-confirm**: the operator types the slug; the Delete button sits at `opacity: .45` until it matches.

---

## Screen 3 — Console · Users (`3c`)

Same shell. Header notes `this surface does not auto-refresh while you edit` with a manual `Reload` — Users, Flags and Providers stay **off** the shared 4s `RefreshTick`; read-only panels stay on it.

- **List** — search field with a live hit count, filter chips (`staff ✕`, `any status`), rows: 30px circular avatar (jade gradient for staff, neutral otherwise), username 600/13px, sub `hana@posteo.de · 412 tracked`, right-aligned status pill (`staff`/`owner` accent, `active` jade, `suspended` `--star`). Suspended rows at `opacity: .65`.
- **Inspector header** — 48px avatar, username Bricolage 600/24px, role pill, mono meta (`id 9c02·41de · joined 2024-06-14 · 412 tracked · last seen 12m`); actions `Sign out everywhere` (ghost) + `Save changes` (filled). Tabs: **Identity & grants · Sessions · Library · Privacy · Activity**.
- **Identity** — `92px 1fr` rows: username, email (both inline inputs), `verified` shown as a jade Check + date, `status` as an `active` / `suspended` segmented control.
- **Permission grants** — grouped checklist (`CATALOGUE` / `PIPELINE` / `PEOPLE` sub-headers on `#0c1016`), one row per permission: checkbox, permission token in mono 12.5px, right-aligned provenance `granted by tim · 5mo`. A `preset: moderator ⌄` selector sits in the section header. Footnote: "Grants apply within one token lifetime — up to 15 minutes for an open session."
- **External sync** — one row per linked tracker: 28×28 tile, `AniList · hanareads`, sub `linked 4mo · 412 entries · pushed 12m`, actions `Force pull` (ghost) and `Unlink` (accent-tinted). Unlink expands an **inline confirm** on `rgba(228,87,46,.06)` stating "Drops the token and 412 entry mappings. Local progress is untouched; they can relink themselves." with `Cancel` / `Unlink` — reversible, so no typing required.
- **Sessions** — device, location + age (stale locations in `--star`), per-row `Revoke`.
- **Data & erasure** — `Export everything` inline; `Erase account` is type-to-confirm on the username, button at `opacity: .5` until it matches, copy noting the audit trail keeps the fact of erasure, not the data.
- **Recent actions** — left-rule timeline, mono, action token in `--text`.

---

## Interactions & behaviour

**Opening a chapter** — always `target="_blank"` to the provider's URL. Resolution order: per-series pinned source → global provider ranking → first source that carries the chapter. A source that is `blocked`/`challenged` is skipped for resolution but still listed (disabled) in the menu.

**Pinning** — the Bookmark action in the per-chapter menu sets a per-series override; `Change source order` deep-links to the global ranking in settings. Both are optimistic with rollback.

**Progress edits** — stepper and `Mark up to here read` write immediately (optimistic, roll back on failure) and push to linked trackers per the user's conflict policy.

**Conflict resolution** — three outcomes: push local, take remote, or set "always trust newest" as the standing policy. Resolving clears the conflict pill.

**Destructive actions** — two tiers, as designed:
- *Reversible* (pause, blocklist, unlink, revoke session, detach chapter, cancel run): act inline; surface an undo affordance where the backend allows it.
- *Irreversible* (unmerge, delete provider, erase account): **type-to-confirm** — the exact slug/username, with the action disabled (`opacity: .45–.55`) until the typed value matches. Confirm copy must name the concrete blast radius with real counts, never a generic "are you sure".

**Motion** — reuse the existing keyframes only: `ikfade` on view mount, `ikbar` on the active rail bar, `ikpulse` on live dots, `ikflow` on active scan progress, `ik-shimmer` on skeletons. Everything degrades under `prefers-reduced-motion: reduce`.

**Focus** — `:focus-visible { outline: 2px solid var(--acc); outline-offset: 2px }` on every interactive element, including the split button's two halves independently and the menu items.

**Keyboard (console)** — `↑↓` move the list selection, `↵` opens the inspector, `x` toggles selection, `⇧x` extends a range, `⌘K` opens the jump field. The hint line in the list footer documents whatever is actually implemented — do not print hints for unbound keys.

**Responsive** — not designed here. Below ~1100px the console's three panes must collapse (rail → icon strip, list+inspector → one at a time); below 820px the app rail already becomes a horizontal strip and the series body grid stacks (`input.css` §9).

## State

Series page: `preferred_source_id` (per-series override), resolved open target per chapter, expanded part-release groups, chapter filter (`all | unread | hide-parts`), sort direction, progress value (optimistic), conflict state per tracker, notes/score/date fields, plus the existing watchlist and notify flags.

Console: selected entity type, selected entity id, row multi-selection set, per-tab inspector state, dirty-field map + `dry_run_passed` gate for provider config, type-to-confirm buffers, and the shared pausable `RefreshTick` (4s) — with Users/Flags/Providers explicitly opted out.

## Design tokens

Take these from `input.css`; the hexes are listed only so the mock is decodable.

| Token | Value | Used for |
| --- | --- | --- |
| `--bg` | `#0b0e13` | app ground, JSON block |
| `--rail` | `#0c1016` | rail, table headers, footer strips |
| `--surface` | `#12171e` | cards, inputs, ghost buttons, monogram tiles |
| `--surface-2` | `#0e131a` | sidebar cards, row hover, part rows, inspector drawer |
| `--border` | `#1a212a` | structural edges |
| `--border-ctl` | `#232a33` | control borders |
| `--border-row` | `#161c24` | row rules |
| `--border-soft` | `#1e262f` | progress/slider tracks |
| `--text` | `#f2efe9` | primary text |
| `--text-2` | `#c9cfd6` | synopsis, chapter titles |
| `--text-3` | `#b8c0ca` | ghost button labels |
| `--muted` | `#8a94a0` | meta text |
| `--faint` | `#5c6672` | timestamps, kickers, placeholders |
| `--faint-2` | `#4d5763` | rail group headers |
| `--acc` | `#e4572e` | primary fill, active bar, unread |
| `--acc2` | `#f07a56` | progress gradient end |
| `--acc3` | `#f2a993` | accent text on tints |
| `--acc-dk` | `#b83a17` | caret half, brand gradient stop |
| `--jade` / `--jade-bright` | `#2e8b78` / `#3da88f` | healthy, read, synced |
| `--star` | `#cba43c` | rating, warnings, degraded |
| type role | `#6fa8dc` manga · `#3da88f` ongoing · `#6fa8dc` running · `#db4a2b` failed | data roles (theme-invariant) |

Tint recipe: fill `color-mix(in srgb, var(--acc) 8–16%, transparent)`, border `var(--acc)` or `color-mix(… 40–55%, transparent)`, text `var(--acc3)`.

Radii: cards `14px` (`--radius`), controls `9–10px`, chips/tiles `6–8px`, pills `20px`.
Spacing in the mock: card padding `16–22px`, row padding `9–11px 12–14px`, grid gaps `10–26px`.
Shadows: menus `0 18px 44px rgba(0,0,0,.6)`; hero cover `0 20px 50px rgba(0,0,0,.55)`; accent buttons `0 6px 18px rgba(228,87,46,.28)`.

Typography — Bricolage Grotesque (display: wordmark, titles, KPI/stat values, monograms), IBM Plex Sans (UI, 15px/1.55 body), IBM Plex Mono (numerics, ids, timestamps, uppercase kickers with `.05–.14em` tracking). All three are already self-hosted via `asset!()` in `src/app.rs`. Sizes used: 9–11px kickers/pills · 12–13px meta · 14–15px body/controls · 17–24px section and inspector titles · 38px hero title.

## Assets

- **Icons** — no new assets. Every glyph is an existing variant of the `Icon` enum in `web/frontend/src/icons.rs`, rendered via `Ic { icon, size }` (24×24 viewBox, `currentColor`, `stroke-width: 2`, round caps/joins): `MenuBook`, `OpenInNew`, `ChevronRight` (rotated 90° for a caret), `Check`, `Bookmark` (used as the pin-preferred affordance), `Block` (hide source), `Search`, `Star`, `Layers`, `Settings`, `Back`, `Notifications`, `CloudDone`, `CloudSync`, `Home`, `Explore`, `Watchlist`, `Dashboard`. If a genuinely new glyph is needed (a true "pin", a "chevron-down"), add a variant to the enum rather than inlining SVG at the call site.
- **Cover art** — CSS gradient placeholders in the mock; production lazy-loads `cover_url` with the typographic-initial fallback (`components/cover.rs`).
- No images, logos or brand assets are included or required.

## Files in this bundle

| File | What it is |
| --- | --- |
| `TankoVault Redesign.dc.html` | the design canvas. **Turn 3 (`3a`, `3b`, `3c`) is the approved direction** — build from it. Turn 2 (`2a`/`2b`) and turn 1 (`1a`–`1f`) are earlier explorations kept for context; `2a`, `1c` and `1e` are the components turn 3 is assembled from. Everything else in turn 1 is superseded. |
| `FRONTEND_AS_BUILT.md` | the current frontend documented: tokens, `ik-*` class inventory, per-screen structure, "where to change what", and known divergences. |
| `support.js` | preview runtime for the HTML file only. **Do not port.** |

Open the HTML file in a browser to inspect the designs; it pans and zooms as a canvas, newest turn at the top.

## Scope note

These three screens are one slice. Still on the old chrome and not covered here: Home, Discover, Search, Watchlist, Notifications, the six Account panels, Auth, and eight of the eleven console entities (Chapters, Merge queue, Scan runs, Failed tasks, Solver, Sync links, Feature flags, Privacy, Audit).

Several controls drawn here have **no endpoint today** — implement the UI behind a capability/feature check, or stub with an explicit TODO rather than shipping a dead control: chapter reassign/detach, unmerge, admin-side sync unlink, provider delete, per-source freshness, per-chapter read state, rating, related series.
