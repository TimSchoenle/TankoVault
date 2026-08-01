# Watchlist redesign — implementation handoff

**Design:** `TankoVault Redesign.dc.html`, turn 4 — option `4a` (list, primary) and `4b` (cover grid, secondary view).
**Replaces:** `web/frontend/src/views/watchlist.rs` in full (kanban board, §7.4).
**Why:** the board is five drag-and-drop columns; the real list is 598 entries / 564 in Reading. Columns can't hold that, drag can't retarget 40 titles, and every card is a fixed-height block so nothing is scannable.

---

## 1. The change in one paragraph

Status stops being a *layout* and becomes a *filter*. One virtualized list replaces five columns. Default sort is **newest release**, and rows are grouped by release recency (Today / Earlier this week / Earlier), so the top of the page is always the triage queue. Each row is 54px and carries only what decides "read this now": progress, unread count, when it dropped, and one **Continue** that opens the next unread chapter. Status change / mute / source override / remove move into a row menu, and the same actions run over a multi-select. Drag-and-drop is deleted; `J/K/X/↵/S` replace it.

---

## 2. Layout spec (option 4a)

All colors/fonts already exist in the shipped Inkstone theme — reuse `ik-*` classes and `var(--acc)` / `var(--muted)` etc. rather than the literal hexes in the design file.

**Page head** — title + `598 titles tracked · 1,684 chapters unread`, right side: sync-status chip (`AniList synced 4m ago`) and `Sync now` (existing `sync_now` handler, unchanged).

**Status tab strip** — `Reading · Plan to read · On hold · Completed · Dropped | All`, each with a count pill. Active tab = accent tint + 1px accent border, bottom border transparent so it merges with the toolbar. Counts come from the server (see §4.2), not from `items.len()`.

**Toolbar** (sticky, 34px controls, `padding:13px 26px`):
- filter input, placeholder `Filter 564 titles by name, tag, author…`, `/` shortcut hint — debounced 200ms, sent as `q=`
- `Unread only` toggle chip (default **on**)
- `Released [any time ▾]` — any / 24h / 7d / 30d
- `Source issues (7)` chip — amber; only rendered when count > 0
- `Sort [Newest release ▾]` — newest release / most unread / recently added / title / progress
- view toggle: list (4a) / grid (4b)

**Column header** (32px): select-all checkbox · Title · Progress · Unread (right) · Released (sorted, caret) · actions.
Grid used by header **and** every row: `grid-template-columns: 34px 1fr 176px 74px 128px 122px; gap:12px; padding:0 26px`.

**Group header** (30px, sticky under the toolbar): `TODAY` · `12 titles · 631 chapters` · `Mark group read`.

**Row** (54px):
- checkbox (selected row: accent left inset shadow `inset 2px 0 0 var(--acc)` + 4% accent wash)
- 28×38 cover (existing `Cover` component) + title, ellipsized to one line + `AsuraScans · 3 sources` submeta. Warning triangle after the title when a source is offline; submeta turns amber and reads `source offline`. `muted` chip after the title when `notify == false`.
- progress: `1420 / 1524` mono + 3px bar. Bar is grey (not accent) when muted; jade at 100%.
- unread: accent-tint pill; `—` in dim when 0
- released: `12m ago` + `ch 1524` beneath
- actions: `Continue` (accent outline on the focused row, neutral otherwise) + `⋯` menu

**Row menu**: Move to → Plan to read / On hold / Dropped · Mark all read · Mute notifications · Change source… · **Remove from watchlist** (destructive, accent text).

**Bulk bar** — floats bottom-center, appears on first selection: `N selected | Move to… | Mark read | Mute | Remove | ✕`.

**Caught-up rows** render at 62% opacity, full opacity on hover, and swap `Continue` for the text `Up to date`.

**Footer**: `Rows 1–40 of 564 · scroll to load` + keyboard legend.

**4b (grid)** is the same data, same filters, same groups: 7-up 2:3 cover cards, unread badge top-right, progress bar across the bottom of the cover, 2-line clamped title, `1420 / 1524 · 12m` beneath. Warning badge top-left when a source is offline.

---

## 3. Frontend work (`web/frontend`)

### 3.1 Delete
`Column`, `WatchCard`, `Dragging`, `dragover`, `column_style`, and every `ondrag*`/`ondrop` handler. The per-card `<select>` goes with them — the row menu is the accessible mover now.

### 3.2 New components (all in `views/watchlist.rs` unless they grow)
| Component | Notes |
| --- | --- |
| `StatusTabs` | counts from `/v1/me/watchlist/counts`; drives `status` query param |
| `FilterBar` | owns `q`, `unread_only`, `released_since`, `sort`, `view`; all mirrored to the URL |
| `GroupHeader` | pure; takes label + title/chapter counts + `on_mark_group` |
| `WatchRow` | memoized on `(series_id, status, notify, unread, last_read_number, selected, focused)` |
| `RowMenu` | popover, focus-trapped, `Esc` closes, arrow keys move |
| `BulkBar` | renders only when `selected.len() > 0` |
| `CoverGrid` | 4b |

### 3.3 List performance
598 rows × ~14 DOM nodes will not stay smooth as a single render. Two acceptable routes, in order of preference:

1. **Server pages + sentinel** — `limit=60`, `offset`, an `IntersectionObserver` sentinel div at the bottom bumps the page. Simple, works with the sticky group headers, and the group aggregates come from the server so they stay correct across pages.
2. **Client windowing** — keep the full `Vec` in a signal, render `[first_visible - 10, last_visible + 10]` with spacer divs sized `n * 54px`. Only if you want the whole list in memory for instant client-side filtering.

Do **not** ship the naïve `for item in items` at this scale.

### 3.4 State that must survive reload
`status`, `sort`, `q`, `unread_only`, `released_since`, `view` → URL query string (shareable, back button works). Density/view preference → `localStorage`. Scroll position and keyboard focus index → restore on back-nav from a series page; this is the single biggest quality-of-life win for a 500-title list.

### 3.5 Keyboard
Roving `tabindex` on rows; container `onkeydown`:
`J` / `↓` next · `K` / `↑` prev · `X` toggle select · `↵` Continue · `S` open status submenu · `Shift+J/K` extend selection · `Esc` clear selection · `/` focus filter · `⌘A` select all in current filter.
Rows are `role="row"` inside `role="grid"`, `aria-selected` on the row, real `<input type="checkbox">` in the first cell with an `aria-label` naming the title.

### 3.6 Optimistic updates
Status change, mute and mark-read should mutate the local signal immediately and roll back on error (today every mutation does a full `reload.bump()` → refetches 598 rows). Keep `reload.bump()` only for sync and for bulk operations.

### 3.7 i18n
New keys under `watchlist.*` in `web/frontend/locales/en.json` + `de.json`: `filterPlaceholder`, `unreadOnly`, `releasedWithin`, `sourceIssues`, `sortBy`, `sort.*`, `group.today`, `group.thisWeek`, `group.earlier`, `markGroupRead`, `continue`, `upToDate`, `sourceOffline`, `muted`, `moveTo`, `markAllRead`, `changeSource`, `nSelected`, `viewList`, `viewGrid`, `rowsOf`. Remove `watchlist.moveToColumn`, `watchlist.subtitle` (the drag instruction).

---

## 4. Backend work — this is the blocking part

`WatchlistCard` / `WatchlistItem` today carries: `series_id, series_title, cover_url, status, notify, added_at, last_read_number, unread, sync_excluded`. The design needs four things it does not have.

### 4.1 New fields on `WatchlistCard` (`crates/db/src/repo/tracking/watchlist.rs::watchlist_detailed`)
| Field | Type | Feeds |
| --- | --- | --- |
| `total_chapters` | `i64` | progress denominator + bar % |
| `latest_chapter_number` | `Option<f64>` | `ch 1524` |
| `latest_chapter_at` | `Option<OffsetDateTime>` | `12m ago`, recency grouping, default sort |
| `preferred_source_name` | `Option<String>` | row submeta |
| `source_count` | `i64` | `· 3 sources` |
| `source_degraded` | `bool` | warning triangle + `Source issues` filter |

`total_chapters` and `latest_chapter_*` are the same `series_sources ⋈ chapters` lateral the unread subquery already walks — fold them into one `LEFT JOIN LATERAL` rather than adding three correlated subqueries (the file's own note about the 835-entry measurement applies).

`source_degraded` should read whatever the provider-health state already is (last scan run failed / provider paused / blocklisted) — if there's no single flag today, define it as *the preferred source's last scan run failed* and leave the richer version to the Providers console work.

### 4.2 `GET /v1/me/watchlist` — query params
`status`, `q`, `unread_only`, `released_since`, `source_issues`, `sort` (`released|unread|added|title|progress`), `order`, `limit`, `offset`. Response becomes `{ items, total, counts: { reading, planned, paused, completed, dropped, all }, groups: [{ key, title_count, chapter_count }] }`.
Sorting and filtering must happen in SQL — the point of the redesign is that the client never holds 598 rows to sort them.

### 4.3 Bulk mutation
`POST /v1/me/watchlist/bulk` — `{ series_ids: [..], status?, notify? }` and `DELETE /v1/me/watchlist/bulk` — `{ series_ids: [..] }`. Without these, "move 40 dropped titles" fires 40 `PUT`s and 40 refetches. Cap at ~200 ids per call; single transaction; return per-id success so the UI can report partial failure.

### 4.4 Mark group read
`POST /v1/me/progress/bulk-read` — `{ series_ids: [..] }`, sets each series' progress to its latest chapter. Reuses the existing per-series mark-read path plus `spawn_targeted_push` per series so external sync stays consistent.

### 4.5 Regenerate
`crates/api-client` is progenitor-generated from `openapi.json` — update the OpenAPI doc, regenerate, then `models.rs` re-exports pick the new fields up.

---

## 5. Suggested order

1. **Backend 4.1 + 4.2** (fields + server-side filter/sort/paginate) — nothing above works without `latest_chapter_at`.
2. **Frontend list** — new `watchlist.rs`, kanban deleted, paged rows, tabs, toolbar, groups, row menu. Ship here; it's already a large win.
3. **Bulk (4.3, 4.4) + BulkBar + keyboard multi-select.**
4. **Grid view (4b) + optimistic updates + scroll/focus restoration.**

## 6. Interim fallback

If 4.1 slips, ship step 2 with `sort=added` as default, group headers hidden, the Released column showing `added_at` labelled "Added", and the progress cell showing just `last_read_number` with no bar. Everything else in the design works on today's payload. Don't ship the kanban to buy time — the list on partial data is still better than five columns.
