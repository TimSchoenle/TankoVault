# Reading Progress & External Sync — Design v2

**Status:** implemented end-to-end (backend, API, and frontend UI). Supersedes `design.md` §6
(the `read_progress` / `sync_mappings` / `external_accounts` tables) and §15 (external sync
service) for the areas covered here. Everything else in `design.md` stands.

> **Implementation note.** Delivered in migration `0014_progress_sync_v2.sql` plus the
> `tankovault-db` repo (`tracking.rs`, `sync.rs`), the `sync` service (three-way merge in
> `mapping.rs`, engine reconciliation + scheduled loop in `engine.rs`, endpoints in
> `main.rs`), and the API (`services/api/src/me.rs` + routes). The Dioxus frontend is fully
> wired (§B.8): the account Sync panel exposes the automatic-sync toggle, the plain-language
> conflict-policy picker, a pending-conflicts badge, a conflict-resolution inbox, and a
> recent-sync-activity log (`views/account.rs`); the series page has a per-title sync opt-out
> and per-chapter mark read/unread via the two-scalar endpoint (`views/series.rs`); and the
> admin console surfaces the read-only per-account policy columns (§B.7, `views/console.rs`).

**Why this doc exists.** The current tracking model (`read_progress(user_id, series_id,
last_read_number)`) uses one undifferentiated scalar for both whole chapters and sub-chapter
*part* releases, so reading a part can silently corrupt whole-chapter progress (§0), and the
external-sync conflict policy is a process-wide env var rather than a per-user, persisted
setting. This document redesigns both, as two related but independently shippable systems:

- **Part A** — splits local read tracking into two independent **scalar** frontiers (whole
  chapters vs. part releases) so the two can never be conflated. Deliberately stays scalar —
  no per-chapter ledger, no row-per-read-chapter table, no change to the existing
  monotonic-progress mental model.
- **Part B** — an external-sync engine with a persistent, user-controlled automatic-sync
  policy and a proper conflict-resolution model.

---

## 0. Problems with the current model (grounded in the current code)

1. **Marking a *part* read silently marks the *whole* chapter read once it appears.** If a
   user reads part release `152.3` and marks it read, `last_read_number = 152.3`. When the
   compiled whole chapter `152` is later scanned in, `152 <= 152.3` is true, so it renders as
   "read" — even though the user only read one-sixth of it. The existing
   `floor(number)`-based unread-*count* fix (series-detail grouping, `continue_reading`,
   `me_stats`) patches the read *side* of this bug but the write side (`progress_set`) is
   still wrong. **This is the bug this design fixes** (Part A).
2. **The per-chapter "Mark read / Mark unread" UI button is backed by one undifferentiated
   scalar.** "Unread" has to special-case "step back to the previous rendered row's number"
   because whole-chapter and part-chapter progress share the same field — an undocumented
   workaround rather than a designed behaviour. This design keeps the same frontier mental
   model (progress remains monotonic — see Non-goals) but makes the rule explicit and
   symmetric across whole and part chapters instead of an ad hoc special case.
3. **No per-series sync exclusion exists.** Every series on a user's watchlist is implicitly
   eligible for push/pull to every linked external provider. There is no way to track a title
   locally without it ever touching AniList (or a future provider).
4. **Conflict policy is a process-wide config value** (`sync.default_conflict_policy` env
   var), not a persisted, per-user setting — despite `design.md` §15 stating it is
   "user-selectable." It cannot actually be changed by a user at runtime today.
5. **"Automatic sync" is reactive-only.** `push_series` fires a best-effort local→remote push
   after a local write. Nothing ever pulls proactively, so a status/progress change made
   directly on AniList's own site is invisible to this system until the user manually clicks
   "Sync now." There is also no way to turn this reactive push off per account.
6. **AniList's integer `progress` is derived via `round(last_read_number)`**, not `floor`.
   This overstates progress for any part release (`152.6` rounds up to `153`, claiming a
   chapter the user hasn't read) and is inconsistent with the `floor()` convention used
   everywhere else in the read-tracking code.
7. **Conflict detection is two-way, not three-way.** `reconcile_progress` only compares the
   *current* local and remote values plus a timestamp. It has no memory of what was last
   agreed, so it cannot distinguish "only remote changed" from "both changed to the same
   value" from "both changed to different values" — every disagreement is resolved by the
   same timestamp/policy heuristic, even when one side simply never changed.

---

## 1. Goals / non-goals

**Goals**
- Track whole-chapter and part-release progress as two independent scalars, independent of
  any external service, so a part release can never be conflated with the whole chapter it
  belongs to.
- Correctly distinguish part releases from the whole chapter they belong to; never conflate
  the two in either direction.
- Let a user exclude a specific series from external sync entirely, by default off-by-opt-out
  rather than requiring an explicit per-title whitelist.
- Make automatic sync an explicit, per-account, persistent, toggleable policy — not an
  always-on side effect of writes.
- Make conflict resolution a real three-way merge (local-changed? remote-changed? both?) with
  a persisted, user-chosen policy, including a policy where genuine conflicts are queued for
  the user to resolve rather than silently auto-picked.
- Keep the design provider-agnostic: nothing here is AniList-specific; a second
  `ExternalProvider` (e.g. MyAnimeList) gets the same policy machinery for free.

**Non-goals**
- **Not** adding arbitrary non-contiguous per-chapter read/unread tracking (e.g. marking
  chapter 48 unread while chapter 50 stays read). Progress remains a monotonic frontier, same
  as the as-built system — this design fixes the whole/part conflation bug and makes the
  frontier's edit semantics explicit and symmetric; it deliberately does not add ledger-style
  per-chapter granularity, which would trade the simplicity of a scalar model for a feature
  nobody has asked for.
- No change to *how* series/chapters are scanned or canonicalised (§7–§10 of `design.md`
  stand as-is).
- No multi-device / offline conflict resolution *within* the local system itself — there is
  exactly one local source of truth per user; "conflict" in this doc always means
  local-vs-external.
- Not adding a second external provider — only making the policy layer generic enough that
  doing so later needs no further redesign.

---

## Part A — Local chapter read-tracking

### A.1 Core principle

Read state stays a **monotonic frontier** — the same shape as the as-built `read_progress`
table — but is split into **two independent scalars** so whole chapters and sub-chapter parts
can no longer corrupt each other:

```
last_read_whole_number  -- highest WHOLE chapter number read (integer-valued)
last_read_part_number   -- highest PART release number read, if any, ahead of the whole
                           frontier (nullable; always fractional, i.e. fract() != 0)
```

This directly fixes the confirmed bug (§0.1) — reading a part can only ever advance
`last_read_part_number`, never `last_read_whole_number` — while staying scalar: two numbers
per (user, series), not a row per chapter ever read, no ledger, no table growth proportional
to lifetime chapters read.

**Accepted trade-off, stated explicitly** (see Non-goals): a scalar frontier cannot remember
"chapter 50 is read but chapter 48 isn't." Marking a chapter read always means "read through
here"; marking a chapter *unread* always means "retreat the frontier to just before here,"
which also un-reads anything after it. This is exactly the existing behaviour's already
established mental model (today's UI "mark unread steps back to the previous row" workaround)
— this design makes it an explicit, documented, symmetric rule instead of an ad hoc special
case, rather than eliminating it with a full ledger.

**Invariant** relating the two scalars: `last_read_part_number`, whenever set, is only
meaningful when it is ahead of `last_read_whole_number`
(`floor(last_read_part_number) >= last_read_whole_number`). Whenever
`last_read_whole_number` advances to or past a stale `last_read_part_number`, the part scalar
is cleared to `NULL` — it carries no information once the whole-chapter frontier has caught up
past it.

### A.2 Schema

```sql
-- read_progress evolves in place: the existing scalar is renamed and split, not replaced.
ALTER TABLE read_progress
  RENAME COLUMN last_read_number TO last_read_whole_number;
ALTER TABLE read_progress
  ADD COLUMN last_read_part_number numeric(10,4);
-- Invariant (enforced in the repo layer at every write site, not a DB CHECK constraint,
-- since it depends on floor(last_read_part_number) vs. last_read_whole_number):
--   last_read_part_number IS NULL OR floor(last_read_part_number) >= last_read_whole_number
```

No new table. `read_progress.updated_at` (already present) continues to serve as the local
"last changed" timestamp for `NewestWins` conflict resolution (§B.3) — bumped on every write
to either scalar.

### A.3 Read/write semantics

**Is chapter `number` read?**
```
is_whole(number) := number == floor(number)

read(number) when is_whole(number):      number <= last_read_whole_number
read(number) when NOT is_whole(number):  floor(number) <= last_read_whole_number
                                          OR (last_read_part_number IS NOT NULL
                                              AND number <= last_read_part_number)
```

The first clause of the part case is what makes the whole rule self-consistent: a part is a
fragment shipped *ahead of* the chapter it floors to, so once that whole chapter is read the
part is read too — which is exactly why "mark read" below is a no-op there. Dropping the
clause would report such a part unread while refusing to mark it read: a dead toggle. One
implementation, `ReadProgress::covers`, owns this rule; SQL read models mirror both clauses
inline.

**Mark chapter `number` read** ("mark read to here" is exactly this rule applied to `N`):
```
if is_whole(number):
    last_read_whole_number = max(last_read_whole_number, number)
    if last_read_part_number is not null
       and floor(last_read_part_number) <= last_read_whole_number:
        last_read_part_number = NULL   -- now stale, superseded by whole-chapter progress
else:
    if floor(number) <= last_read_whole_number:
        -- already covered by whole-chapter progress; no-op
    else:
        last_read_part_number  = max(last_read_part_number, number)
        -- and the whole frontier catches up to everything below the chapter `number` is a
        -- part of: "mark read" means "read through here" (§A.1), so `46.1` asserts all of
        -- chapter 45 as well. Same catalogue-derived target as un-reading uses, so gaps and
        -- chapters that exist only as parts are honoured.
        last_read_whole_number = max(last_read_whole_number,
                                     the highest chapter number that exists for this series
                                      strictly below `floor(number)`)
```

The second assignment is not optional bookkeeping. Without it the two scalars can describe a
frontier that contradicts itself — with `last_read_whole_number = 40`, marking `46.1` read
reports `41`..`45` unread while `46.1` reads as read — and since §B.5 pushes
`last_read_whole_number` and nothing else, the external provider keeps receiving `40`. The part
frontier is for reading *ahead*; it is not a place to park whole chapters that were read.

**Marking read also tracks the series.** Reading a chapter is the statement that you follow
the title, so every write that advances a frontier — `PUT /v1/me/progress/:series_id`, marking
one chapter read, "mark read to here" — adds the series to the caller's watchlist when it is
not there yet, at the same defaults a manual add uses (`reading`, notify on). An entry that
already exists is left exactly as it stands: status, notify flag and sync exclusion survive,
so reading one more chapter of a `dropped` series does not resurrect it as `reading`. Marking
unread never removes an entry — untracking stays an explicit act. Without this, progress on a
series opened from Discover or Search was recorded against a title the reader could not find
anywhere in Library.

**Mark chapter `number` unread** (only sensible at or behind the current frontier):
```
if is_whole(number):
    last_read_whole_number = the previous whole chapter number that exists for this series
                              strictly below `number`
    last_read_part_number  = NULL
else:
    if floor(number) <= last_read_whole_number:
        -- the part is covered by the whole chapter that contains it, so un-reading it
        -- necessarily un-reads that chapter: the whole frontier retreats below it and the
        -- part frontier picks up whatever part is still read underneath.
        last_read_whole_number = the previous whole chapter number that exists for this
                                  series strictly below `floor(number)`
        last_read_part_number  = the previous part number strictly below `number` that is
                                  still > the new last_read_whole_number, or NULL if none
    elif number == last_read_part_number:
        last_read_part_number = NULL
    else:
        last_read_part_number = the previous part number strictly below `number` that is
                                 still > last_read_whole_number, or NULL if none
```

Unmarking a chapter that isn't the current frontier (an older, already-passed chapter)
retreats progress past it, un-reading everything after it too — an explicit, disclosed
consequence of staying scalar, not a silent side effect (§A.6 requires the client to confirm
with the user before calling this on a non-frontier chapter).

This is a mechanical rewrite of `crates/db/src/repo/tracking.rs`: `progress_set` gains the
whole/part branch above; `progress_get`, `progress_state`, `continue_reading`,
`watchlist_detailed`, `me_stats`, `feed` swap their `c.number <= last_read_number` /
`c.number > COALESCE(rp.last_read_number, 0)` comparisons for the two-scalar check above. The
existing `floor(c.number)`-based "distinct whole chapters" grouping for unread *counts*
(`continue_reading` et al., already correct per the chapter-parts work) is untouched — only
`progress_set`'s write-side bug is what this section fixes.

### A.4 Deriving a single "progress number" (for external sync and simple UI)

Trivial in the scalar model: **`last_read_whole_number` is already the number** an external
service like AniList understands (an integer count of completed chapters). There is no
derivation step and nothing to aggregate — one of the direct benefits of staying scalar. Part
releases never contribute to this number, by construction (§A.1's invariant), which is the
direct fix for the `round()` bug (§0.6) without needing any logic beyond reading the column.

### A.5 Per-manga sync exclusion (the "no whitelist" flag)

```sql
ALTER TABLE watchlist_entries
  ADD COLUMN sync_excluded boolean NOT NULL DEFAULT false;
```

Default `false` — every watchlisted series is included in sync by default. This is
deliberately an **opt-out (blacklist) model, not opt-in (whitelist)**: with potentially
hundreds of tracked titles, requiring an explicit per-title whitelist before anything syncs
doesn't scale and is hostile to new users. A single toggle on the series page
("Sync to external services: on") is enough for the common case.

For users who link more than one provider and want finer control (e.g. sync to AniList but
not to a future MyAnimeList link), an optional per-provider override:

```sql
CREATE TABLE series_sync_overrides (
  user_id   uuid NOT NULL REFERENCES users(id)   ON DELETE CASCADE,
  series_id uuid NOT NULL REFERENCES series(id)  ON DELETE CASCADE,
  provider  text NOT NULL,
  excluded  boolean NOT NULL,
  PRIMARY KEY (user_id, series_id, provider)
);
```

**Precedence** (evaluated by one function, `is_sync_excluded(user, series, provider)`, the
single choke point every sync path calls before touching a series):
1. A `series_sync_overrides` row for that exact provider, if present — wins outright.
2. Otherwise `watchlist_entries.sync_excluded` (the blanket flag).
3. Otherwise: included.

Exclusion is **absolute** — an excluded series is skipped by the reactive push, the scheduled
reconciliation, *and* a manual "Sync now," until the user clears the flag. This is what
actually "prevents whitelists": there is no separate consent list to maintain; the same flag
that governs automatic behaviour also governs manual triggers, so there is exactly one
mental model for "does this title touch AniList" per series.

### A.6 API surface (new / changed)

```
PUT    /v1/me/progress/:series_id/chapters/:number   { read: bool }
       -- applies the §A.3 mark-read/mark-unread rule for that one chapter number. Unmarking
       -- a non-frontier (older) chapter retreats progress past it too (§A.1/A.3) -- the API
       -- performs this exactly as specified; the client must confirm with the user first
       -- when `number` isn't the current frontier.
POST   /v1/me/progress/:series_id/mark-read-to       { number }
       -- equivalent to PUT .../chapters/:number { read: true }; kept as its own endpoint to
       -- match the existing "mark read to here" UI action's naming.
GET    /v1/me/progress/:series_id
       -> { last_read_whole_number, last_read_part_number }
PUT    /v1/me/watchlist/:series_id/sync                { excluded: bool }
PUT    /v1/me/watchlist/:series_id/sync/:provider      { excluded: bool }   -- override
```

`PUT /v1/me/progress/:series_id { last_read_number }` (the current endpoint) needs only a
field rename (`last_read_number` → `last_read_whole_number`) to keep its exact existing
semantics — no compatibility shim or migration window required, since it's the same
underlying value under a new name.

### A.7 Migration plan

1. `ALTER TABLE read_progress RENAME COLUMN last_read_number TO last_read_whole_number;`
   `ALTER TABLE read_progress ADD COLUMN last_read_part_number numeric(10,4);` — a single
   rename-and-add migration. Every existing row's whole-chapter progress is preserved
   exactly as-is; every user's part progress starts `NULL` (conservative — nobody is credited
   with part progress they may never have actually read, consistent with "safer to
   under-mark than over-mark").
2. Update `crates/db/src/repo/tracking.rs` per §A.3.
3. Update the API/frontend per §A.6 (a field rename plus the two new endpoints).

No backfill step, no dual-write period, no ledger to retire later — the whole migration is
one column rename plus one column add.

---

## Part B — External sync redesign

### B.1 Design principles

- **Opt-out, not opt-in**, for which series sync at all (§A.5) — consistent, single flag.
- **Automatic ≠ silent.** Automatic sync is a named, persistent, per-account setting the user
  can see and turn off, not an implicit side effect of every write.
- **Persistent, not env-configured.** Conflict policy lives in the database per linked
  account. The existing `sync.default_conflict_policy` env var becomes only the seed default
  applied when an account is first linked — never a live control.
- **Three-way merge, not two-way.** The engine remembers what was last agreed so it can tell
  "only local changed" / "only remote changed" / "both changed, agree" / "both changed,
  disagree" apart, and only the last case is a real conflict.
- **Auditable.** Every automatic decision (push, pull, conflict, skip) is recorded somewhere
  the user can see, not just inferred from side effects.
- **Provider-generic.** All of this hangs off the existing `ExternalProvider` trait and the
  `provider` slug column already used by `external_accounts`/`sync_mappings`; nothing here is
  AniList-specific.

### B.2 Schema

```sql
-- Per-account automatic-sync policy, persisted (replaces the env-only default).
ALTER TABLE external_accounts
  ADD COLUMN auto_sync_enabled boolean NOT NULL DEFAULT true,
  ADD COLUMN conflict_policy   text    NOT NULL DEFAULT 'newest_wins';

-- watchlist_entries needs its own change timestamp for newest_wins to compare status
-- changes fairly (today only added_at exists, which never changes after creation).
ALTER TABLE watchlist_entries
  ADD COLUMN updated_at timestamptz NOT NULL DEFAULT now();
-- (bump on every status/notify write; existing rows default to added_at's value)

-- The three-way merge "common ancestor": what both sides agreed on as of the last
-- successful reconciliation, so the engine can tell which side(s) actually changed since.
ALTER TABLE sync_mappings
  ADD COLUMN last_synced_local_progress  double precision,
  ADD COLUMN last_synced_remote_progress double precision,
  ADD COLUMN last_synced_local_status    text,
  ADD COLUMN last_synced_remote_status   text,
  ADD COLUMN last_synced_at              timestamptz;

-- A genuine, unresolved conflict awaiting user input under the 'ask_me' policy.
CREATE TABLE sync_conflicts (
  id           uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  user_id      uuid NOT NULL REFERENCES users(id)   ON DELETE CASCADE,
  series_id    uuid NOT NULL REFERENCES series(id)  ON DELETE CASCADE,
  provider     text NOT NULL,
  field        text NOT NULL,             -- 'progress' | 'status'
  local_value  text NOT NULL,
  remote_value text NOT NULL,
  detected_at  timestamptz NOT NULL DEFAULT now(),
  resolved_at  timestamptz,
  resolution   text                        -- 'local' | 'remote', NULL while pending
);
CREATE INDEX sync_conflicts_pending_idx ON sync_conflicts (user_id) WHERE resolved_at IS NULL;

-- User-facing sync history (distinct from the operator-facing audit_log): what the
-- automatic engine actually did, so "automatic" never means "invisible."
CREATE TABLE sync_history (
  id         uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  user_id    uuid NOT NULL REFERENCES users(id)  ON DELETE CASCADE,
  series_id  uuid NOT NULL REFERENCES series(id) ON DELETE CASCADE,
  provider   text NOT NULL,
  action     text NOT NULL,    -- 'push' | 'pull' | 'conflict_auto' | 'conflict_manual'
  detail     jsonb NOT NULL,   -- {"field": "progress", "from": 40, "to": 42, "policy": "newest_wins"}
  created_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX sync_history_user_idx ON sync_history (user_id, created_at DESC);
```

(IDs use `gen_random_uuid()` as the column default per the project's existing deviation from
the `design.md` UUIDv7-everywhere text — app code still generates and passes real v7 ids
explicitly, so the default only fires for rows inserted outside it.)

`sync_history` is expected to grow; it is a diagnostic/transparency log, not an audit-of-record
— an operational follow-up (not blocking this design) should prune rows older than e.g. 90
days per user.

### B.3 Conflict policy — four modes, three-way detection

```
ConflictPolicy = LocalWins | RemoteWins | NewestWins | AskMe
```

`AskMe` is new. Detection algorithm per mapped series, per field (`progress`, `status`),
replacing today's `reconcile_progress`:

```
local_changed  = current_local  != last_synced_local
remote_changed = current_remote != last_synced_remote

match (local_changed, remote_changed):
  (false, false) -> no-op
  (true,  false) -> push local -> remote                 (no conflict)
  (false, true)  -> pull remote -> local                 (no conflict)
  (true,  true):
    if current_local == current_remote -> converged, no-op, just update the snapshot
    else -> a REAL conflict:
        LocalWins  -> push local -> remote
        RemoteWins -> pull remote -> local
        NewestWins -> compare each side's own last-modified time
                      (read_progress.updated_at / watchlist_entries.updated_at vs.
                       the provider's own updatedAt), apply the newer side
        AskMe      -> do NOT touch either side; insert a sync_conflicts row;
                      leave last_synced_* as-is so the conflict is detected again next
                      run until resolved (idempotent — never double-queues once a
                      pending row already exists for that (user, series, provider, field))
```

After any resolution (auto or manual), `last_synced_local_*` / `last_synced_remote_*` /
`last_synced_at` are updated to the agreed values — this is what makes the *next* run able to
tell "changed since last time" apart from "still disagreeing from before."

An `AskMe` conflict blocks sync **only for that series**, not the whole account — every other
mapped series on the same linked account keeps reconciling normally each run.

### B.4 Automatic sync architecture

Two independent mechanisms, both gated by the same `external_accounts.auto_sync_enabled` flag
and the same `is_sync_excluded` check (§A.5):

1. **Reactive targeted push** (existing `push_series`, refined). Fires after a local write
   (`read_progress` change, `watchlist_entries` status change) via a best-effort
   `tokio::spawn`, same as today. Now additionally: skipped entirely if
   `auto_sync_enabled = false` or the series is excluded — previously neither check existed.
   Local-wins-by-construction (a direct user action is authoritative), so this path still
   bypasses the conflict machinery entirely, same as today — it's a fast path, not a
   reconciliation.

2. **Scheduled reconciliation** (new). A periodic loop, owned by the `sync` service (keeps
   sync concerns encapsulated where they already live), analogous in shape to
   `control-plane`'s existing cron scheduler (`design.md` §12): every interval (operator
   config, jittered per account to avoid a thundering herd against AniList's rate limits), for
   every `external_accounts` row with `auto_sync_enabled = true`:
   - fetch the remote list once (reusing the existing paced/retrying `fetch_list`),
   - for every `sync_mappings` row not excluded (§A.5), run the three-way merge (§B.3),
   - write `last_synced_*`, append `sync_history`, and any new `sync_conflicts` rows.

   This is what actually closes the reactive-push-only gap noted in §0: a status/progress
   change made directly on AniList's site now flows back automatically, instead of only being
   visible after a manual "Sync now."

   If the `sync` service is scaled to multiple replicas, guard each account against a
   double-run with the same Redis leader-election / lock pattern already built for
   `control-plane`'s singleton scheduler (`design.md` §12) — reuse, don't reinvent.

```
                        auto_sync_enabled?  ──false──▶  no automatic sync at all
                              │ true                     (manual pull/push still works)
                              ▼
                    is_sync_excluded(series)? ──true──▶  skipped, always
                              │ false
              ┌───────────────┴────────────────┐
              ▼                                 ▼
      local write happens              scheduled reconciliation tick
      (mark read / watchlist)          (per linked account, interval)
              │                                 │
      reactive targeted push          three-way merge per mapped series
      (local wins, no reconcile)      (push / pull / conflict per policy)
```

### B.5 Progress translation, local ↔ external

- **Push**: `last_read_whole_number` (§A.4) is sent directly as the external provider's
  integer progress — no derivation step. Replaces `round(last_read_number)` outright, and is
  never inflated by part progress (`last_read_part_number` is a separate field the push never
  reads). It does not *lose* part progress either: reading `46.1` has already advanced the
  whole frontier to `45` per §A.3, so the push sends `45` — the most the provider can be told
  truthfully. Deriving that at the push site instead would leave the local read models
  disagreeing with it.
- **Pull**: an external integer progress `N` is applied locally via the same "mark read" rule
  as §A.3 (`last_read_whole_number = max(last_read_whole_number, N)`, or set outright to `N`
  when the merge resolves in the remote's favour), clearing `last_read_part_number` if it is
  now stale (`floor(last_read_part_number) <= N`). A pull can never mark a part release read —
  AniList has no concept of parts. This asymmetry (locally you can be "ahead" via parts
  without AniList knowing) is expected and documented, not a bug.

### B.5a Linked series — one remote entry, several local ones

`sync_mappings` is keyed on `(series_id, provider)`, not on the pair, so **several local series
can map to one external id**. Catalogue duplicates routinely do: the matcher attaches one, an
operator attaches another, and the provider still keeps a single list entry for the work. Call
that set the **linked group**.

The engine treats a group as one unit on both sides:

- **Local side of the merge.** Progress is the **highest** `last_read_whole_number` any
  non-excluded member holds, with that member's `updated_at` as the change time; status comes
  from the member the mapping resolved to (statuses are unordered, so there is no maximum).
  Taking the maximum is what makes the write-back below safe — settling on a lower member's
  value would un-read chapters the reader had marked on another copy.
- **Ancestor.** The freshest `last_synced_*` snapshot across the group, not the driven member's,
  since the members' snapshots are written together and only diverge when a duplicate joins.
- **Write-back ("the mirror").** Once a value is settled — by a merge, by a first push, or by
  the targeted push a mark-read fires — every non-excluded member the reader actually holds
  something for adopts it, and each member's ancestor snapshot is refreshed to match.
- **The targeted push settles on the series the reader acted on, not on the group's maximum.**
  The two rules differ because the situations do: a reconciliation has no user action to go on,
  so a member behind the others is stale rather than a statement, and the maximum is the only
  reading that loses nothing; a targeted push *is* the statement, and marking a chapter unread
  has to be able to retreat the group. Neither can flip-flop against the other, because the push
  refreshes every member's ancestor snapshot to the value it pushed — the next reconciliation
  reads that as "neither side changed" rather than as a member to drag back up.
- **Exclusion still wins per series.** An excluded member is neither read nor written; a group
  whose members are all excluded is skipped whole. A member excluded while a sibling is not
  hands the group to the sibling rather than skipping it.
- **A member the reader never added is left alone.** The mirror keeps entries in step; it does
  not create watchlist entries, and it does not manufacture a progress row to record a
  zero frontier.
- **Nothing is fanned out while a field is in conflict**, because nothing is settled: the
  ancestor is deliberately not advanced (§B.3), and the group is left as it stands until the
  conflict resolves.

Without this, a duplicate drifted permanently. The remote-driven pass reconciled whichever
member the mapping resolved to and the local-driven pass skipped the rest as an already-handled
external id, so nothing ever revisited them — marking a chapter read on one copy left the other
showing it unread, with no run that would ever correct the difference. The same skip also let
two members of a group each *create* the remote entry from their own state during the
local-driven pass, the second clobbering the first.

### B.6 API surface (new / changed)

```
GET    /v1/me/sync/:provider/settings
PATCH  /v1/me/sync/:provider/settings     { auto_sync_enabled?, conflict_policy? }

GET    /v1/me/sync/conflicts                          -- pending, across all providers
POST   /v1/me/sync/conflicts/:id/resolve  { resolution: "local" | "remote" }

GET    /v1/me/sync/history?series_id=&provider=&page=
```

Existing `POST /v1/sync/{provider}/pull|push` (manual, full-reconciliation) and
`POST /v1/sync/push-series` (targeted) are unchanged in shape; `pull`/`push` now run the
three-way merge of §B.3 instead of the old two-way `reconcile_progress`.

### B.7 Admin visibility

The existing Console "Sync" tab (`admin/sync/accounts`, `admin/sync/mappings`) gains the new
per-account `auto_sync_enabled`/`conflict_policy` columns (read-only for operators — these are
user settings, not operator-overridable) and a count of each user's pending
`sync_conflicts`, for support/debugging visibility. No new operator actions are required.

### B.8 UX (Account panel / series page)

- **Account → Sync & integrations**, per linked provider: an "Automatic sync" toggle and a
  conflict-policy picker with plain-language labels ("Always keep my local progress" /
  "Always trust AniList" / "Use whichever changed most recently" / "Ask me when they
  disagree"), plus a conflicts badge ("3 need your review") linking to a resolution list — the
  same interaction shape as the operator merge-candidates queue, reused for a user-facing
  audience.
- **Series page**: a "Sync to external services" toggle next to the watchlist controls,
  shown only when the user has at least one linked provider; off by default is *not* the
  default (§A.5 — inclusion is the default), this toggle is how a user opts a specific title
  *out*.
- Sensible zero-config default: a new user who links AniList and touches nothing else gets
  automatic sync on, `newest_wins`, every series included — "just works," with every knob
  available but none required.

---

## 2. Rollout plan

1. **Schema** — the `read_progress` column rename + add (§A.2), the `sync_mappings` /
   `external_accounts` / `watchlist_entries` columns (§B.2), `sync_conflicts`, `sync_history`.
2. **Backend, Part A** — the two-scalar `progress_set`/read-side rewrite (§A.3),
   `is_sync_excluded` (§A.5), the renamed/new progress endpoints (§A.6).
3. **Backend, Part B** — three-way merge (§B.3) replacing `reconcile_progress`, the scheduled
   reconciliation loop (§B.4), progress translation (§B.5), new settings/conflict/history
   endpoints (§B.6).
4. **Frontend** — the explicit, symmetric per-chapter mark read/unread rule (§A.3, formalising
   today's "step back to previous row" workaround instead of hiding it), the sync-exclusion
   toggle, the Account sync-policy panel, the conflict-resolution inbox.
5. **Cleanup** — the env `default_conflict_policy` becomes purely the seed value for
   newly-linked accounts (no table to drop — `read_progress` is extended in place, not
   replaced).

Each step is independently shippable and backward-compatible with the step before it.

## 3. Explicitly deferred (not part of this design)

- A second `ExternalProvider` implementation (e.g. MyAnimeList) — this design makes adding one
  a matter of registering it, per the existing registry pattern; not built here.
- `sync_history` retention/pruning policy — operational detail, not a correctness concern.
- Arbitrary non-contiguous per-chapter read tracking (a true per-chapter ledger) — explicitly
  rejected in favour of staying scalar (§A.1, Non-goals); would only be reconsidered if a
  concrete product need for non-contiguous reading emerges.
