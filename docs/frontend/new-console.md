# Operator console v2 — implementation handoff

**Covers:** `web/frontend/src/views/console/**` (30 files, ~7 100 LOC), `services/api/src/admin/**`,
`crates/contracts/src/admin.rs`.
**Extends** the shipped console rather than replacing it — the master–detail shell, the capability
gating and the per-entity modules are the right shape and stay.
**Why:** the console is *complete* (every admin endpoint but three is wired) and *not yet
operable*. It cannot be linked to, it cannot be driven from the keyboard, it re-fetches the whole
world every four seconds instead of being pushed to, and its two triage surfaces — the audit trail
and the scan queue — are fixed-size unfiltered lists that fall off the end under load.

---

## 1. The change in one paragraph

The console stops being a *page with tabs* and becomes an *addressable, pushed, keyboard-driven
operations surface*. Rail selection, the inspector's selected row and each panel's filters move
into the URL, so an operator can paste "the provider that is failing" into chat. The 4 s poll is
replaced by a single ticketed SSE stream so a 30-run scan queue updates in 2 s without twelve
panels re-fetching in lockstep. The jump field becomes a real command palette over entities,
providers, users and actions. `J/K/↵/G-then-letter/?` replace the mouse. Overview stops being nine
point-in-time tiles and grows a trend line plus an attention feed that names what is wrong and
links straight to it. Audit and scans gain server-side filters, paging and a detail expander. And
the three admin endpoints the console never calls — `scan_stream`, `get_scan`,
`extend_privacy_request` — get used, the last of which is a statutory obligation the UI currently
cannot discharge.

---

## 2. What is already right — do not rebuild it

Verified against the current tree; the 2026-07 frontend audit's console findings are largely
remediated and re-doing them would be waste.

- **The rail model.** `RAIL` (`views/console/mod.rs:79`) groups 12 entities under four kickers, and
  `Entity::requires()` pairs each with a `(Permission, Feature)` so an entity is offered only when
  both hold. Empty groups are dropped whole. Keep this — extend it, never bypass it.
- **`RefreshTick`** is already the `Reload` newtype the audit asked for, and it is already
  deliberately *not* subscribed by Providers / Users / Flags / Recommendations, because a refetch
  landing mid-edit discards the form. That opt-out survives §3.2 unchanged.
- **The shared kit.** `async_block`, `async_block_list`, `async_view`, `Kpi`, `HealthPill`,
  `ListSearch`, `ListFooter`, `CompactPager`, `Section`, `SegControl`, `TabBar<T: TabKind>`,
  `NoSelection`, `OutcomeLine`, `InlineConfirm`, `TypeToConfirm` all live in `components/` now.
  Every new panel composes these; no panel gets its own error rendering.
- **Endpoint coverage.** Every `/v1/admin/*` route except the three named above is already called.
  This is not a "wire up the backend" project.
- **The vocabulary tests.** `the_picker_offers_every_adapter_kind` reads the committed
  `openapi.json` because `web/frontend` is a separate workspace with no compile-time link to the
  API. Any new hand-maintained enum in the console needs the same treatment.

---

## 3. Frontend work

### 3.1 The console gets a URL

Today `/console` is one route and the selected entity is `use_signal(|| first)`
(`views/console/mod.rs:269`). Reload, back button, bookmark and "here, look at this" all lose the
operator's position. Providers and Users additionally hold a selected row and a tab in local
signals, so the deepest state in the app is the least addressable.

Target route table (`app.rs`):

```rust
#[route("/console")]
Console {},
#[route("/console/:entity?:..query")]
ConsoleEntity { entity: ConsoleEntity, query: ConsoleQuery },
```

`ConsoleEntity` is the existing `Entity` enum promoted to `pub(crate)` with `FromStr`/`Display`
over the slugs it already has (`Entity::slug()`, `mod.rs:146`) — the slugs are the URL, so
`/console/providers`, `/console/scan-runs`, `/console/feature-flags` work on day one. An unknown
slug redirects to `/console` rather than 404ing; the rail is capability-filtered, so a slug the
reader may not see redirects the same way `current` already falls back today (`mod.rs:300`).

`ConsoleQuery` follows `views/watchlist/query.rs` exactly — a struct with `Default`,
`From<&str>`, `Display`, `encode_component`/`decode_component`, and its round-trip test. Fields:

| Param | Type | Owner |
| --- | --- | --- |
| `sel` | `Option<Uuid>` | the selected provider / user / request row |
| `tab` | `Option<String>` | the inspector's tab (`config`, `runs`, `coverage`, `danger`, `grants`, …) |
| `q` | `String` | the panel's `ListSearch` text |
| `status` | `Option<String>` | Users' `StatusFilter`, Privacy's queue filter, Scans' run state |
| `provider` | `Option<String>` | provider slug filter on Scans / Audit |
| `since` | `Option<String>` | `1h` / `24h` / `7d` window on Audit, Scans, Overview |
| `page` | `u32` | the paged panels |

Rules: the URL is the source of truth, panels read it and never shadow it in a signal; a filter
change `replace`s history, a selection change `push`es it (so back moves between rows, not between
keystrokes); every panel that has a filter today must be migrated in the same change, because a
half-migrated console is worse than an unmigrated one.

**Test:** a `ConsoleQuery` round-trip test in the `query.rs` style, plus one asserting every
`Entity` slug parses back to itself — the same defect class `the_picker_offers_every_adapter_kind`
pins.

### 3.2 Live push replaces the 4 s poll

`REFRESH_MS = 4000` (`mod.rs:34`) bumps one shared tick; every subscribed panel then re-issues its
own GET. On Overview that is `system_stats` twice (the rail's copy at `mod.rs:290` and the panel's
at `overview.rs:17` — the same payload fetched twice per tick) plus `provider_stats`. It is
strictly worse than the push infrastructure the app already has for notifications
(`live.rs`, ticketed `EventSource`, exponential backoff, `stream_ticket` minted per attempt).

Build `views/console/live.rs` as the console's counterpart:

- one `EventSource` for the whole console, opened once at `Console` mount, closed on unmount or
  token change, reusing `live.rs`'s backoff constants and per-attempt ticket discipline;
- named events → typed signals in a `ConsoleLive` context: `stats` (the `SystemStats` payload the
  rail and Overview both read — one fetch, two readers), `runs` (the scan queue), `attention` (see
  §3.5). Panels read the context instead of holding a `use_resource` on a tick;
- `RefreshTick` **stays** for the panels that have no stream event and for the manual refresh
  button. What changes is that the tick no longer fires on a timer while a stream is healthy;
- the pause switch (`controls::LiveControls`) becomes "detach from the stream", and the bar shows
  the connection state — `live` / `reconnecting in Ns` / `paused` — because an operator staring at
  a stale queue must be able to tell that it is stale.

The mid-edit opt-out (`Entity::auto_refreshes`) is unchanged: Providers, Users, Flags and
Recommendations keep ignoring pushes into their forms.

Backend prerequisite: §4.1. Without it there is no stream a browser can open.

### 3.3 A real command palette

`JumpField` (`mod.rs:481`) renders a `⌘K` chip and, on click, focuses the *top bar's* search box —
which searches series. The console's own affordance sends the operator to the reader's search. Its
doc comment is honest about this ("rather than advertising a command palette the app does not
have"); the answer is to build the palette.

`components/palette.rs`, opened by `⌘K` / `Ctrl-K` while the console is mounted (`Esc` closes,
`↑/↓` move, `↵` runs, `Tab` never leaves the trap). Sources, in order:

1. **Entities** — the 12 rail entries, filtered by capability. Navigates.
2. **Providers** — from the already-loaded provider list; `↵` opens
   `/console/providers?sel=<id>`.
3. **Users** — debounced 200 ms against `list_users` (`DirectoryQuery` already takes `q`).
4. **Actions** — verbs, each gated on the permission its endpoint requires: *trigger scan on…*,
   *pause provider…*, *sweep merge candidates*, *rebuild matching keys*, *rebuild recommendation
   model*, *revoke sessions for…*, *toggle flag…*. Destructive verbs open the panel with the
   confirm affordance armed; they never fire from the palette directly. `TypeToConfirm` exists for
   exactly this and must not be bypassed to save a keystroke.

Results carry the permission-derived subtitle so an operator learns *why* something is absent.
The palette is a console component, not an app-global one — the top bar's `⌘K` keeps its meaning
outside `/console`, and inside it the console's binding wins.

### 3.4 A keyboard model

The watchlist has a full one (`views/watchlist/mod.rs:626-690`: `J/K`, arrows, `X`, `↵`, `Esc`,
character shortcuts). The console — the surface a power user lives in — has none beyond the
`TabBar`'s arrow handling.

Container `onkeydown` on `.ik-cons`:

| Key | Action |
| --- | --- |
| `G` then `o/m/p/s/u/f/a/…` | jump to entity by initial (`?` legend lists them) |
| `J` / `K` / `↓` / `↑` | move the list selection (master–detail entities) |
| `↵` | open the focused row in the inspector |
| `/` | focus the panel's `ListSearch` |
| `⌘K` | palette (§3.3) |
| `R` | manual refresh — the existing `tick.bump()` |
| `P` | pause / resume the live stream |
| `Esc` | clear selection, close drawer, close palette |
| `?` | shortcut legend overlay |

Roving `tabindex` on `.ik-cons-row`, `role="listbox"`/`option` with `aria-selected` on the list
column, and — this is the part that is missing today and is cheap — an `aria-live="polite"` region
announcing the outcome of every mutation, because `OutcomeLine` currently renders text no screen
reader is told about.

### 3.5 Overview becomes an incident surface

`SystemOverview` is nine `Kpi` tiles of point-in-time counters (`overview.rs:57-114`). Three carry
an accent when non-zero. Nothing says whether a number is rising, and nothing links anywhere.

Three additions:

1. **Trend.** Each tile grows a 24 h sparkline from §4.3 and a delta against the same hour
   yesterday. `chapters_24h` sitting at 0 is invisible today unless the operator remembers
   yesterday's figure; a flat line makes a stalled pipeline obvious in one glance.
2. **Attention feed** — the panel above the tiles, and the reason the console gets opened at all.
   Server-ranked (§4.3), each row is *what is wrong* + *since when* + a link that lands on the
   fixing surface with the right row selected (which §3.1 makes expressible): providers unhealthy →
   `/console/providers?sel=…`, failed tasks in 24 h → `/console/scan-runs?status=failed`, merge
   candidates aging past 7 days → `/console/merge-queue`, privacy requests within 7 days of
   `due_at` → `/console/privacy?sel=…`, a feature flag overridden more than 30 days ago →
   `/console/feature-flags`. Empty state is a single line, not a hidden panel: "nothing needs
   attention" is information.
3. **Tiles become links.** Every tile navigates to the entity that explains it. A number with no
   next step is a poster.

### 3.6 Audit trail: filter, page, expand

`audit_log` returns "up to 40 most recent" records with no parameters at all
(`services/api/src/admin/system.rs:59`), and `AuditPanel` renders four columns of them. On a system
with a nightly sweep the 40 rows are one operator-minute deep, and `AuditView.detail: Json`
(`crates/contracts/src/admin.rs:85`) — the *what actually changed* — is fetched and thrown away.

- Filters, all URL-backed (§3.1) and all server-side (§4.2): actor, action, target, time window.
- `CompactPager` (already used by Users) over a `{ items, total }` envelope.
- A row expander rendering `detail` as pretty JSON via the existing `config_editor_text`
  (`mod.rs:512`), with the before/after of a permission or flag change called out.
- Action tokens get a `.ik-pill` tone: destructive (`*.delete`, `*.revoke`, `*.erasure`) vermilion,
  grants amber, everything else neutral. Wording via a `console.audit.action.*` catalogue lookup
  with the raw token as the fallback, so a new server-side action renders as itself rather than as
  a missing key.
- `users/activity.rs` is the same defect with a sharper edge: it fetches the global 40 and filters
  by `entry.actor == username` (`activity.rs:46-51`), so a user who has not acted inside the last
  40 *system-wide* actions renders "no recent actions" — the panel is silently wrong, not merely
  shallow. It moves to `?actor=<id>` on the server filter, and gets a test whose doc comment says
  so.

### 3.7 Scan runs: a detail drawer and a triage loop

`list_scans` returns 30 runs, unparameterised; `scan_failures` returns a flat feed. `get_scan`
(`/v1/admin/scans/{run_id}`) is generated in the client and never called, so a run is a row that
cannot be opened.

- Scans becomes master–detail (`Entity::is_master_detail`, `mod.rs:189`): the list on the left, a
  run drawer on the right driven by `get_scan` + `?sel=<run_id>`.
- The drawer shows per-task state, attempts, the error text, elapsed and throughput, with
  *retry this run* and *cancel* where the permission allows.
- Failures gain grouping: identical `error` strings collapse to one row with a count and the
  providers affected. Twelve rows of the same broken selector are one problem, and the current
  feed presents it as twelve.
- Filters: provider, mode, state, window — all §4.4, all URL-backed.

### 3.8 Bulk actions

Every queue in the console is one-at-a-time. The merge queue is the acute case: a sweep can enqueue
hundreds of candidates and each is `dismiss_merge_candidate` plus a full list refetch
(`merge.rs:235`). Privacy already renders a checkbox but only to arm a single confirm
(`privacy.rs:121`).

Add multi-select to the merge queue, the sync unmapped/unmatched queues, and the user directory,
using the watchlist's model verbatim: checkbox column, `X` to toggle, `Shift+J/K` to extend,
`⌘A` to select the current filter, a `BulkBar` that appears on first selection. Bulk dismissal
needs §4.5; the sync queues can batch client-side against the existing per-item endpoints with a
capped concurrency of 4 and a per-id outcome list, but merge must not (hundreds of round trips).
Report partial failure per id — a bulk action that says only "3 failed" is not actionable.

### 3.9 Privacy: the extend action is missing

`/v1/admin/privacy/requests/{id}/extend` exists, is generated as
`extend_privacy_request`, and is called by nothing. The panel renders `due_at` (`privacy.rs:165`)
but offers no way to move it. GDPR Art. 12(3) permits a two-month extension *provided the subject
is informed within one month* — so the operator's only route today is a psql session, and the
audit trail records nothing. Add the action beside the deadline: a reason field (required — it goes
into the audit `detail`), the new `due_at` previewed before confirm, and an amber pill on the row
once a request has been extended.

While there: sort the queue by `due_at` ascending by default and colour the deadline — vermilion
inside 7 days, amber inside 14. A statutory clock should not need arithmetic.

### 3.10 Operator preferences that survive a reload

Alongside the `tv-*` knobs in `state/prefs.rs`, to `localStorage`, not the URL (they are the
operator's, not the link's):
last entity (so `/console` bare lands where they left off), table density (`compact` is currently
hardcoded in the class strings), live-stream paused, the Overview tiles they pinned, and column
visibility on the wide tables. `ProviderStatsTable` renders 10 columns at ~12 px; letting an
operator drop four is the difference between a table and a wall.

### 3.11 Mobile and the a11y floor

Below 760 px the grid collapses to `"rail" "list" "insp"` and the rail becomes a wrapping row of
12 unlabelled-group buttons (`input.css:1260-1264`) — the group kickers are `display:none`, so
Providers and Privacy sit side by side with no indication they are different kinds of thing. Make
the small-screen rail a single `<select>`-backed entity switcher plus a back affordance from
inspector → list, and let the palette (§3.3) carry the rest; an operator on a phone is triaging,
not administering.

Floor to hold on every panel touched, from `DESIGN_SPEC.md` §9: visible focus ring on every
control (the rail has one, the rows have one, the new drawer must), `aria-current` on the active
entity (already correct at `mod.rs:421`), 4.5:1 contrast on the 11–12 px mono metadata — several
`var(--faint)` uses at that size are the ones to re-measure with the CSS-contrast probe — and
`prefers-reduced-motion` respected by the new drawer and palette transitions.

### 3.12 i18n

Every new string is a catalogue key in **both** `web/frontend/locales/en.json` and `de.json` —
`locales_define_the_same_keys` fails the build otherwise, and a missing key renders as the key
itself rather than as an error. New namespaces: `console.palette.*`, `console.keys.*` (the legend),
`console.attention.*`, `console.audit.action.*`, `console.audit.filter.*`, `console.scan.detail.*`,
`console.bulk.*`, `console.privacy.extend.*`, `console.live.*` (`live` / `reconnecting` / `paused`
/ `stale`). Verbs stay imperative; the German column is not optional.

---

## 4. Backend work — the blocking parts

### 4.1 `/v1/admin/scans/stream` cannot be opened by a browser

The endpoint exists, polls `scan_runs` every 2 s and pushes a `runs` event
(`services/api/src/admin/scans.rs:143`) — but it is declared `security(("bearer_auth" = []))` and
extracts `AuthUser`, and `EventSource` cannot set an `Authorization` header. That is precisely why
`/v1/me/stream` takes a single-use `stream_ticket` in the query string instead
(`services/api/src/openapi.rs:34-53`). As shipped, the admin stream is reachable only by a non-browser
client, which is why the console polls at all.

The fix is the me-stream shape, not a new mechanism: take `Query<StreamQuery>`, `consume` the
ticket, resolve the user, then `require(Permission::ScansRead)` — the permission check must happen
*after* ticket redemption and must not be skippable, because a ticket proves session, not authority.

Then widen it into the console's one stream — `GET /v1/admin/stream`, same ticket scheme, emitting
named events the caller is entitled to:

| Event | Payload | Gate |
| --- | --- | --- |
| `stats` | `SystemStats` | `system.stats` |
| `runs` | `Vec<ScanRun>` | `scans.read` |
| `attention` | `Vec<AttentionItem>` (§4.3) | per-item, intersected with the caller's permissions |

Emit only the events the caller's permissions allow; a stream that leaks a count to a reader who
cannot open the panel is a disclosure, and the access-matrix suites will not catch it because they
test status codes, not event names. Keep `/v1/admin/scans/stream` as-is for one release, then
delete it. Cadence stays 2 s for `runs`, 10 s for `stats`, 30 s for `attention` — the current
whole-console 4 s poll is both too fast for stats and too slow for a run in flight.

### 4.2 `/v1/admin/audit` needs parameters and a page

Today: no params, hard-coded 40 (`system.rs:59-64`). Add an `IntoParams` query struct in the shape
`DirectoryQuery` / `CandidateFilter` / `QueueQuery` already use:

```
actor: Option<Uuid>, action: Option<String>, target: Option<String>,
since: Option<OffsetDateTime>, until: Option<OffsetDateTime>,
limit: u32 (default 40, cap 200), offset: u32
```

Response becomes `{ items: Vec<AuditView>, total: i64 }`. Filtering happens in SQL — the point is
that the client never holds the trail to filter it. `repo::audit::list_recent` gains a filtered
sibling; the `audit_log` index on `(created_at DESC)` needs `(action, created_at DESC)` and
`(actor_id, created_at DESC)` alongside it, and both must clear `repo_query_plans`' cost ceiling
(the `EXPLAIN` sweep is Docker-gated — run it, it is the gate this change is most likely to trip).

### 4.3 `/v1/admin/stats/history` and the attention feed

Two new read endpoints, both `system.stats`-gated:

- `GET /v1/admin/stats/history?window=24h&bucket=1h` → `Vec<StatsSample>`, the sparkline source for
  §3.5. Serve it from an hourly rollup table (`admin_stats_hourly`, written by the sync service's
  existing scheduler), not by re-running the `stats` aggregate per bucket — the live `system_stats`
  query is already the heaviest read on the admin surface and 24 replays of it per page load is a
  self-inflicted outage.
- `GET /v1/admin/attention` → `Vec<AttentionItem>` — `{ kind, severity, subject_id, subject_label,
  since, count }`. Ranking lives on the server so the rules stay in one place and so the console,
  the stream and any future alerting agree on what "needs attention" means. Each item declares the
  permission required to act on it and the server filters by the caller's set.

### 4.4 `/v1/admin/scans` needs parameters

Same treatment: `provider`, `mode`, `state`, `since`, `limit` (default 30, cap 200), `offset`, and
a `{ items, total }` envelope. `scan_failures` gains `provider`, `since`, `limit`, and a
`group_by=error` variant returning `{ error, count, providers, latest_at }` for §3.7.

### 4.5 Bulk merge dismissal

`POST /v1/admin/merge-candidates/dismiss` takes one candidate. Accept `{ candidate_ids: [..] }`,
cap ~200, single transaction, return per-id outcome. One audit row per candidate — a bulk action
that writes one summary row makes the trail useless for the thing it exists for.

### 4.6 Non-negotiables for every route added above

- `cargo run -p xtask -- openapi`, then regenerate `crates/api-client`. Never hand-edit either.
- Every new operation id needs a row in `me_gates()` / `public_gates()` / `covered_elsewhere()` —
  `admin_access_matrix.rs` fails with the operation id in the message, and the suite is
  Docker-gated so no offline gate will warn you first:
  `cargo test -p tankovault-api --features integration --test admin_access_matrix`.
- Every new `query!`/`query_as!` → `cargo run -p xtask -- sqlx-prepare` against a migrated database,
  then `repo_query_plans`.
- New contract types are `secrecy`-typed where they carry anything sensitive; nothing in the
  attention feed or the audit `detail` expander may widen what a permission already withholds.

---

## 5. Phases

Each phase is independently shippable and independently valuable. Conventional commits throughout,
scope `console` for frontend work and the owning crate for backend work.

**Phase 1 — addressability.** §3.1 (routes + `ConsoleQuery` + migrate every panel's filters), §3.10
(prefs). No backend. This is the phase that makes every later one describable in a link, so it goes
first even though it is the least visible.
`feat(console): make console state addressable`

**Phase 2 — push.** §4.1 (ticketed admin stream), §3.2 (console `live.rs`, one `SystemStats` fetch
instead of two, connection state in the bar). Deletes the timer, not the tick.
`feat(api): open the admin event stream to ticketed browsers` /
`feat(console): drive the console from the live stream`

**Phase 3 — triage.** §4.2 + §3.6 (audit filters, paging, detail expander), §4.4 + §3.7 (scan run
drawer, grouped failures). The two surfaces that are currently unusable under load.
`feat(console): filter and page the audit trail`

**Phase 4 — the operator's hands.** §3.3 (palette), §3.4 (keyboard model + `aria-live`), §3.11
(mobile + a11y floor).
`feat(console): add the command palette and keyboard model`

**Phase 5 — throughput and obligations.** §4.5 + §3.8 (bulk), §3.9 (privacy extend — pull this
forward if a real request is ever near its deadline; it is a compliance gap, not a convenience),
§4.3 + §3.5 (history, attention feed, linked tiles).
`feat(console): dismiss merge candidates in bulk` / `feat(console): extend privacy deadlines`

---

## 6. Non-goals

- **No new charting dependency.** Sparklines are inline `<svg>` polylines; the CSP forbids
  `unsafe-eval` and a charting library is not worth widening anything for. See rule 1.
- **No `document::eval`,** including in the palette's focus handling — a typed wrapper in
  `browser.rs` or nothing. Rule 2; the failure mode is an aborted WASM instance, not an error.
- **No write actions in the palette.** It navigates and it arms; confirmation stays on the panel
  that owns the consequence.
- **No client-side filtering of anything paged.** If a filter is worth having it is worth a SQL
  predicate; `users/activity.rs` is the existing violation and §3.6 removes it.
- **Not a metrics stack.** §4.3 is one rollup table for one screen. If real observability is wanted
  later it is Prometheus, not `admin_stats_hourly`.

---

## 7. Definition of done, per phase

- `cargo check -p tankovault-api` (or the crate touched) and, from `web/frontend`,
  `cargo clippy --target wasm32-unknown-unknown -- -D warnings`.
- `cargo run -p xtask -- openapi` re-run and the diff committed whenever a route or DTO moved.
- `cargo test -p tankovault-api --features integration --test admin_access_matrix` for any phase
  that adds a route — the failure names the operation id and no offline gate reports it.
- `cargo test -p tankovault-db --features integration repo_query_plans` for any phase that adds a
  query.
- Both locale files updated; `locales_define_the_same_keys` green.
- `cargo run -p xtask -- ci` before the pull request, and the report says which gates were actually
  run and which were not.
