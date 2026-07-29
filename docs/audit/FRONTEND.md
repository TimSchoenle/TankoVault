# TankoVault frontend audit — `web/frontend/` (Dioxus 0.7 / WASM)

Scope: `web/frontend/**` (56 files, 16,121 LOC) plus `services/frontend/src/main.rs` for asset
delivery only. Read-only analysis. Judged against `docs/frontend/DESIGN_SPEC.md`,
`IMPLEMENTATION_PLAN.md`, `PROGRESS.md`, and
`docs/frontend/design_handoff_series_and_console/FRONTEND_AS_BUILT.md`.

---

## (a) Executive summary

**The premise needs adjusting.** The brief assumed "a lot of the frontend code could be
abstracted" and that the tiny `components/` directory was the core smell. That is half right,
and the wrong half matters:

- **Already excellent, do not touch:** DTOs are 100% generated from the OpenAPI schema
  (`models.rs` is pure re-export + presentation traits — the 2024 sync-contract-drift class of
  bug is structurally dead). i18n discipline is near-perfect (2 hardcoded English strings in
  16k LOC, plus a catalogue-parity test). No Tailwind class soup at all — there is a
  hand-authored `ik-*` layer and 158/158 referenced classes are defined. Session/capability
  state is genuinely well designed. Documentation density is above enterprise bar.
- **The real smell is not missing abstractions — it is *unadopted* abstractions.** The right
  components already exist and are bypassed. `async_view`/`async_list` were built specifically
  to guarantee "a failed fetch is always visible and always retryable" (asserted as a
  load-bearing invariant in `FRONTEND_AS_BUILT.md` §7). **35 of 59 `use_resource` sites (59%)
  bypass them**, and the invariant is already broken in at least four screens that render a
  failed fetch as muted grey text with no retry. `EmptyBox` exists but is not exported, so 28
  sites hand-roll it. `SkeletonBlock` exists and 12 sites hand-roll it identically.
- **The quality machinery is not gated.** CI runs `cargo check --target wasm32-unknown-unknown`
  and nothing else (`.github/workflows/ci.yml:126-139`). The crate declares a full
  `clippy::pedantic` lint set that never runs, and **41 unit tests that never run** — including
  `locales_define_the_same_keys`, which is the *only* missing-key check in the project.

### Proposed new/promoted components and hooks

| # | Name | Kind | Call sites replaced | Est. LOC removed |
|---|---|---|---|---|
| 1 | *(adopt existing)* `async_view` / `async_list` | helper | 35 hand-rolled resource triads | ~245 |
| 2 | `use_api_resource<T>` | hook | 59 `use_resource` prologues | ~180 |
| 3 | *(export existing)* `EmptyBox` | component | 28 hand-rolled `ik-empty` divs | ~55 |
| 4 | *(adopt existing)* `SkeletonBlock` | component | 12 hand-rolled `ik-skeleton` divs | ~12 |
| 5 | `Field` (label + input + `for`/`id`) | component | 14 `ik-field` + 17 `ik-kv` blocks | ~185 |
| 6 | `TabBar<T: TabKind>` | component | 4 tab strips | ~40 |
| 7 | `AsyncSection` (`section` + `h3` + async_view) | component | 10 console panel wrappers | ~70 |
| 8 | `DataTable` / `TableCard` | component | 4 `ik-tablewrap` blocks | ~45 |
| 9 | `Kpi` (unify the two rival definitions) | component | 12 sites, 2 defs | ~25 |
| 10 | `StatusPill<T>` (replaces stringly `HealthPill`) | component | ~20 pill sites | ~50 |
| 11 | `Paginator` (offset) | component | 2 (Discover, Users) | ~35 |
| 12 | `Avatar` / `MonoTile` | component | 6 | ~20 |
| 13 | `AuthGate` (wraps `SignInGate` + `caps.is_ready()`) | component | 5 | ~20 |
| 14 | Promote `views/console/shell.rs` → `components/` | move | 8 components, 3 importers | 0 (relocation) |
| 15 | `DiscoverFilters` context struct | state | `FilterPanel`(14 props) + `ActiveFilters`(8) + 3 chips | ~60 |
| 16 | Named style constants / CSS utilities for 488 inline `style:` | CSS | 488 attributes | ~60 |
| | **Total** | | | **~1,100 LOC (≈6.8%)** |

The LOC figure is the *lesser* prize. The prize is that items 1–4 restore a documented
correctness invariant, item 5 fixes seven inaccessible auth inputs, and the phase-0 CI work
makes the other 41 tests and the pedantic lint set actually load-bearing.

### Severity roll-up

| Severity | Count | Findings |
|---|---|---|
| High | 5 | F1, F2, F3, F11, F14 |
| Medium | 9 | F4, F5, F6, F7, F8, F9, F12, F13, F16 |
| Low | 4 | F10, F15, F17, F18 |

---

## (b) Findings

---

### F1 — 59% of data fetches bypass `async_view`, and the "always retryable" invariant is already broken

**Severity: High** · **Effort: M**

**Evidence.** 59 `use_resource` call sites; only 24 route through `async_view`/`async_list`.

| File | `use_resource` | via helper |
|---|---|---|
| `views/console/sync/queues.rs` | 6 | 0 |
| `views/console/users.rs` | 5 | 4 |
| `views/account/sync.rs` | 5 | 4 |
| `views/console/sync/inspector.rs` | 4 | 0 |
| `views/discover.rs` | 4 | 2 |
| `views/home.rs` | 4 | 3 |
| `views/series/tracking.rs` | 4 | 1 |
| `views/console/providers.rs` | 3 | 2 |
| `views/series/mod.rs` | 3 | 1 |
| `views/console/scans.rs` | 2 | 0 |
| `views/console/solver.rs` | 2 | 0 |
| `views/console/merge.rs` | 2 | 0 |
| `views/console/{audit,overview,stats,mod,flags,privacy}.rs` | 1 each | 0,0,0,0,1,1 |
| `views/console/sync/mod.rs` | 1 | 0 |
| `components/{shell,topbar}.rs` | 3 | 0 (background tasks — legitimately exempt) |

The drift the helper exists to prevent has already happened. `views/console/overview.rs:37-52`:

```rust
let body = match &*res.read_unchecked() {
    None | Some(None) => rsx! { div { class: "ik-skeleton", style: "height:104px;" } },
    Some(Some(Err(e))) => {
        rsx! {
            p { class: "ik-muted", style: "font-size:13px;",
                {i18n.args("console.overview.unavailable", &[("message", e)])}
            }
        }
    }
    Some(Some(Ok(s))) => { /* … */ }
};
```

Identical shape, identical defect, at `views/console/stats.rs:41-49` and
`views/console/audit.rs:39-47`: a failed fetch renders as **muted grey body text with no retry
affordance**. `FRONTEND_AS_BUILT.md` §7 states the opposite as a property of the app:

> Roughly thirty call sites used to open-code that match; **new data surfaces must go through
> these helpers** or the "a failed fetch is always visible and always retryable" property stops
> holding.

It has stopped holding. On the operator console specifically, "the stats endpoint is down"
currently looks like a low-priority note rather than a failure.

A second, distinct drift: `views/console/{overview,stats,audit,solver,scans}.rs` wrap the payload
in `Option<Result<…>>` to encode "signed out" (`if session.is_authenticated() { Some(…) } else
{ None }`), producing a triple-nested `Option<Option<Result<…>>>` match that no helper can
consume. That is why they bypass the helper — the workaround caused the bypass.

**Why it matters.** This is the single highest-value finding. The app has a correct, documented,
already-implemented answer to loading/error/empty, and 59% of surfaces do not use it. Every
bypass is a place a future failure mode ships silently. It is also the largest LOC win.

**Remediation.**

1. Delete the `Option<Option<…>>` idiom. `Api::client()` already subscribes to the token
   (`api/mod.rs:59-62`), so a signed-out fetch re-runs on sign-in without the manual guard. Where
   an explicit gate is wanted, use the `AuthGate` from F14 at the component boundary, not inside
   the resource.
2. Add the missing loading-state parameterisation so the console panels can adopt the helper:

```rust
// components/feedback.rs
/// `async_view` with a fixed-height skeleton — the shape every console panel wants.
pub(crate) fn async_block<T: 'static>(
    resource: &Resource<Result<T, String>>,
    reload: Reload,
    height: u32,
    content: impl FnOnce(&T) -> Element,
) -> Element {
    async_view(resource, reload, || rsx! { SkeletonBlock { height } }, content)
}
```

`overview.rs` then becomes:

```rust
rsx! {
    section { style: "margin-bottom:18px;",
        {async_block(&res, tick_reload, 104, |s| rsx! { KpiGrid { stats: Signal::new(s.clone()) } })}
    }
}
```

3. Note `RefreshTick` (`views/console/mod.rs:77-89`) is structurally identical to `Reload`
   (`hooks.rs:14-27`) but not interchangeable, which is why the tick-driven panels cannot pass a
   `Reload` to `ErrorBox`. Unify: make `RefreshTick` a newtype wrapper that yields a `Reload`, or
   have `async_view` take `impl Fn()` for its retry action instead of a concrete `Reload`.

**Estimated LOC removed: ~245.**

---

### F2 — CI runs `cargo check` only: 41 tests and the entire pedantic lint set are dead

**Severity: High** · **Effort: S**

**Evidence.** `.github/workflows/ci.yml:126-139`:

```yaml
  frontend:
    name: frontend (wasm32)
    …
      - name: cargo check (wasm)
        working-directory: web/frontend
        run: cargo check --target wasm32-unknown-unknown
```

That is the entire frontend gate. Meanwhile:

- `web/frontend/Cargo.toml` declares `[lints.clippy] pedantic = warn` plus eight deliberate
  `allow`s, with a comment explaining the set "mirrors" the workspace lints. **Never executed.**
- 41 `#[test]` functions across 8 files (`api/error.rs`, `i18n.rs`, `models.rs`, `state/jwt.rs`,
  `util.rs`, `views/console/providers.rs`, `views/notifications.rs`, `views/series/model.rs`).
  **Never executed.**
- `i18n.rs:280-299` `locales_define_the_same_keys` is the project's only missing-key/unused-key
  check, and it exists precisely because (per its own doc comment, `i18n.rs:18-21`) `i18nrs`
  falls back to an arbitrary catalogue for a missing key and can render the literal string
  `Key '…' not found` to users. **This safety net is not wired to anything.**
- No `cargo fmt --check`.
- No `npm run css:build` check, so `assets/main.css` can drift from `input.css` unnoticed (see
  F11).

**Why it matters.** The repo has invested heavily in tests and lints that provide zero
regression protection. A locale key can be deleted from `de.json` today and ship. This is the
cheapest high-value fix in the audit.

**Remediation.** Replace the job body:

```yaml
  frontend:
    name: frontend (wasm32)
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
        with:
          targets: wasm32-unknown-unknown
          components: clippy, rustfmt
      - uses: Swatinem/rust-cache@v2
        with: { workspaces: web/frontend }
      - name: fmt
        working-directory: web/frontend
        run: cargo fmt --check
      - name: clippy (wasm)
        working-directory: web/frontend
        run: cargo clippy --target wasm32-unknown-unknown -- -D warnings
      - name: unit tests (host target)
        working-directory: web/frontend
        run: cargo test
      - name: css is in sync with input.css
        working-directory: web/frontend
        run: |
          npm ci
          npm run css:build
          git diff --exit-code assets/main.css
```

The tests are written to run on the host target — `util.rs`, `api/error.rs` and
`views/series/model.rs` deliberately split the browser-clock call away from the rule under test
("split out so the boundary is testable on the host target"). Verify `cargo test` compiles on
host before pinning this; if a stray `js_sys` call blocks it, gate that module behind
`#[cfg(target_arch = "wasm32")]` rather than dropping the job.

---

### F3 — Seven auth/password inputs have no programmatic label

**Severity: High** · **Effort: S**

**Evidence.** Of 14 `ik-field` blocks, 7 pair a bare `<label>` with an `<input>` and no
`for`/`id` link:

| Site | Field |
|---|---|
| `views/auth.rs:175-183` | Email (register) |
| `views/auth.rs:184-191` | Username (register) |
| `views/auth.rs:193-200` | Email or username (sign in) |
| `views/auth.rs:202-215` | Password |
| `views/password.rs:72-85` | Email (forgot password) |
| `views/password.rs:182-190` | New password |
| `views/password.rs:191-199` | Confirm password |

```rust
// views/auth.rs:175
div { class: "ik-field",
    label { {i18n.t("auth.field.email")} }     // no r#for
    input {                                     // no id
        class: "ik-input",
        r#type: "email",
        value: "{email}",
        oninput: move |e| email.set(e.value()),
    }
}
```

The *console* does this correctly everywhere — `views/console/providers.rs:611-619`,
`views/console/users.rs:607-615`, `views/account/profile.rs:69`,
`views/account/privacy.rs:177` all carry `r#for` + matching `id`. So the codebase knows the
right pattern; the auth screens — the one surface every single user must pass through — are the
exception.

**Why it matters.** An unlabelled input is announced by a screen reader as "edit text, blank".
There is no wrapping `<label>` either (the label is a *sibling*, not an ancestor), so there is
no implicit association to fall back on. Also: no `<form>` element and no `autocomplete`
attributes anywhere in the auth flow, so password managers cannot reliably fill or save
credentials, and Enter-to-submit is hand-wired per input (`auth.rs:209`, `password.rs:79`,
`password.rs:198`) instead of coming free from form semantics.

**Remediation.** Extract the field component that already implicitly exists, and give it the
attributes the console version has:

```rust
// components/form.rs
#[component]
pub(crate) fn Field(
    id: String,
    label: String,
    #[props(default = "text".to_string())] kind: String,
    #[props(default)] autocomplete: Option<String>,
    value: Signal<String>,
    #[props(default = false)] disabled: bool,
    #[props(default)] on_enter: Option<EventHandler<()>>,
) -> Element {
    let mut value = value;
    rsx! {
        div { class: "ik-field",
            label { r#for: "{id}", "{label}" }
            input {
                id: "{id}",
                class: "ik-input",
                r#type: "{kind}",
                autocomplete: autocomplete.unwrap_or_default(),
                disabled,
                value: "{value}",
                oninput: move |e| value.set(e.value()),
                onkeydown: move |e| {
                    if e.key() == Key::Enter {
                        if let Some(handler) = &on_enter { handler.call(()); }
                    }
                },
            }
        }
    }
}
```

Call sites collapse from 8-14 lines to 1-6, and `autocomplete: "current-password"` /
`"new-password"` / `"username"` / `"email"` becomes possible for the first time. Replaces 14
`ik-field` blocks; a `KvField` sibling replaces the 17 `ik-kv` blocks in
`views/console/{providers,users}.rs` which repeat
`style: "font-size:12.5px;padding:9px 11px;"` six times verbatim.

**Estimated LOC removed: ~185.**

---

### F4 — `components/` is thin because ~285 LOC of genuinely shared components live inside view modules

**Severity: Medium** · **Effort: M**

**Evidence.** `components/` is 6 files / ~665 LOC. Meanwhile:

| Component | Currently lives in | Visibility | Consumed by |
|---|---|---|---|
| `Section`, `SegControl`, `SliderRow`, `InlineConfirm`, `TypeToConfirm`, `ListFooter`, `NoSelection`, `ListSearch` | `views/console/shell.rs` (229 LOC) | `pub(super)` | `console/{users,providers}.rs` |
| `PanelCard` | `views/account/mod.rs:156` | private | account panels only |
| `Kpi` | `views/console/overview.rs:137` | `pub(super)` | overview |
| `Kpi` (**a second, different one**) | `views/console/providers.rs:943` | private | providers Coverage tab |
| `HealthPill` | `views/console/providers.rs:289` | `pub(super)` | `console/stats.rs:112`, `console/solver.rs:142` |

Two smells here beyond mere placement:

1. **Two rival `Kpi` components.** `overview.rs:137` is `Kpi(label, value, sub, accent)`;
   `providers.rs:943` is `Kpi(label, value)` with a hardcoded `style: "font-size:24px;"`. Same
   name, same visual role, sibling modules, divergent APIs. `views/console/users.rs:746-765`
   then hand-rolls a *third* variant inline (three `div.ik-kpi` blocks with the same
   `style: "font-size:24px;"`).
2. **A view imports a component out of a sibling view.** `views/console/stats.rs:8` and
   `views/console/solver.rs:12` both `use crate::views::console::providers::HealthPill;`. That
   is a layering inversion — `views/` should depend on `components/`, never on each other.

`HealthPill` is also stringly typed (`fn HealthPill(state: String)`, `providers.rs:289`) with a
string `match` on `"active" | "degraded" | …` at line 298-304, while a `ProviderState` enum and a
`provider_state_token()` converter both exist two functions away (`providers.rs:311-320`). Two
of three call sites round-trip enum → `&'static str` → `String` for nothing; the third
(`stats.rs:112`) passes a raw `String` straight off the wire, so the two paths cannot be
type-checked against each other.

**Why it matters.** The asymmetry the brief noticed is real but its cause is placement, not
absence. Anything in `views/console/shell.rs` is unreachable from `views/account/` or
`views/series/` — which is why `views/series/tracking.rs` and `views/account/sync.rs` re-derive
card and confirm chrome from scratch.

**Remediation.**

- Move `views/console/shell.rs` → `components/console.rs` (or split: `components/forms.rs` for
  `SegControl`/`SliderRow`/`ListSearch`, `components/confirm.rs` for `InlineConfirm`/
  `TypeToConfirm`, `components/layout.rs` for `Section`/`ListFooter`/`NoSelection`). Export
  `pub(crate)`.
- Promote `PanelCard` to `components/layout.rs`; it is `ik-sidebar-card` chrome, and
  `ik-sidebar-card` already appears at 7 sites across 3 view trees
  (`account/{mod,profile,sync}.rs`, `series/{mod,tracking}.rs`).
- Delete both `Kpi`s; keep one in `components/data.rs`:
  `Kpi { label, value, #[props(default)] sub: String, #[props(default)] accent: String }`.
- Retype `HealthPill` to take `ProviderState`. For `stats.rs`, the wire type is a `String`
  (`ProviderStat::state`) — parse once at the boundary rather than pushing the string into the
  view layer.
- Export `EmptyBox` and `Brush` from `components/mod.rs` (see F5).

---

### F5 — `EmptyBox` and `SkeletonBlock` exist but 40 sites hand-roll them

**Severity: Medium** · **Effort: S**

**Evidence.** `components/feedback.rs:69-73` defines `EmptyBox`, and `feedback.rs:89-93` defines
`Brush`. Neither is listed in `components/mod.rs:10-13`:

```rust
pub(crate) use feedback::{
    async_list, async_view, ErrorBox, ErrorLine, OutcomeLine, SignInGate, SkeletonBlock,
    SkeletonGrid, SkeletonRows,
};
```

`EmptyBox` is therefore reachable only from inside `async_list` (`feedback.rs:153`). Consequence:
**30 hand-rolled `div { class: "ik-empty", … }`** across 17 files —
`console/sync/inspector.rs` ×4, `console/providers.rs` ×3, `series/mod.rs` ×3,
`console/{merge,solver,sync/queues}.rs` ×2 each, and 11 more.

`Brush` — the design's "one signature device" per `FRONTEND_AS_BUILT.md` §1 — has **zero call
sites**. It is dead code that the as-built record claims is in use.

Same story for skeletons: `SkeletonBlock(height)` renders exactly
`div { class: "ik-skeleton", style: "height:{height}px;" }` (`feedback.rs:40-44`), and 12 sites
write that literal markup instead:

```
views/console/audit.rs:40           height:80px
views/console/merge.rs:33, :213     height:60px, 120px
views/console/overview.rs:38        height:104px
views/console/solver.rs:46, :179    height:100px, 60px
views/console/stats.rs:42           height:120px
views/console/sync/inspector.rs:86  height:40px
views/console/sync/mod.rs:43        height:60px
views/console/sync/queues.rs:62, :235, :397   height:60px ×2, 40px
```

(A further 5 in `series/mod.rs:156-158` and `watchlist.rs:122-123` are legitimately bespoke
multi-line skeletons — those argue for a `SkeletonLines` variant, not for inlining.)

**Why it matters.** Every hand-rolled empty state is a place the empty-state styling can drift,
and — worse — several of them (`console/merge.rs:33`, `sync/queues.rs:62`) sit inside the
hand-rolled triads from F1, so the empty and error states are visually distinguishable only by
accident.

**Remediation.** Export `EmptyBox`; sweep the 28 non-helper sites. Either delete `Brush` or use
it and correct the as-built record. Sweep the 12 identical `ik-skeleton` divs to `SkeletonBlock`.
Add `SkeletonLines { widths: Vec<u32> }` for the 5 bespoke ones.

**Estimated LOC removed: ~67.**

---

### F6 — Four hand-rolled tab strips, none with tab semantics

**Severity: Medium** · **Effort: S**

**Evidence.** Byte-for-byte equivalent markup at four sites:

| Site | Enum | Notes |
|---|---|---|
| `views/account/mod.rs:130-139` | `Panel` (6) | `class: "ik-tabs"` |
| `views/notifications.rs:224-233` | `Tab` (4) | `class: "ik-tabs"` |
| `views/console/providers.rs:590-599` | `Tab` (5) | `class: "ik-tabs flush"` + `style: "margin-top:14px;"` |
| `views/console/users.rs:581-590` | `Tab` (5) | `class: "ik-tabs flush"` + `style: "margin-top:14px;"` |

```rust
// views/console/users.rs:581 — and the other three, modulo the enum name
div { class: "ik-tabs flush", style: "margin-top:14px;",
    for entry in Tab::ALL {
        button {
            key: "{entry.label_key()}",
            class: if *tab.read() == entry { "ik-tab active" } else { "ik-tab" },
            onclick: move |_| tab.set(entry),
            {i18n.t(entry.label_key())}
        }
    }
}
```

**None of the four** carries `role="tablist"`, `role="tab"`, `aria-selected`, or `aria-controls`.
There is no arrow-key navigation and no roving `tabindex`. All four enums independently
implement the same `const ALL: [Self; N]` + `fn label_key(self) -> &'static str` shape.

**Why it matters.** Twenty tab controls announce as plain buttons with no indication of which is
current or what group they belong to. This is also the clearest single case of "the same rsx!
shape four times" the brief asked for.

**Remediation.**

```rust
// components/tabs.rs
pub(crate) trait TabKind: Copy + PartialEq + 'static {
    fn all() -> &'static [Self] where Self: Sized;
    fn label_key(self) -> &'static str;
}

#[component]
pub(crate) fn TabBar<T: TabKind + Clone>(
    selected: Signal<T>,
    /// Restrict to a subset — Account and the Console rail both hide unavailable entries.
    #[props(default)] visible: Option<Vec<T>>,
    #[props(default)] flush: bool,
) -> Element {
    let i18n = use_i18n();
    let mut selected = selected;
    let entries = visible.unwrap_or_else(|| T::all().to_vec());
    rsx! {
        div {
            class: if flush { "ik-tabs flush" } else { "ik-tabs" },
            role: "tablist",
            for entry in entries {
                button {
                    key: "{entry.label_key()}",
                    class: if *selected.read() == entry { "ik-tab active" } else { "ik-tab" },
                    role: "tab",
                    "aria-selected": if *selected.read() == entry { "true" } else { "false" },
                    onclick: move |_| selected.set(entry),
                    {i18n.t(entry.label_key())}
                }
            }
        }
    }
}
```

The four `impl Tab { const ALL; fn label_key }` blocks become `impl TabKind for Tab`, which is
the same code with a trait name on it. The console entity rail
(`views/console/mod.rs:408-422`) is the same shape again with `aria-current` instead — it
already does the a11y correctly and can share the trait if not the component.

**Estimated LOC removed: ~40.**

---

### F7 — Zero `use_memo` in 16k LOC; derived state is recomputed and re-cloned every render

**Severity: Medium** · **Effort: M**

**Evidence.** `grep -c use_memo` over `web/frontend/src` returns **0**. Every derived value is
computed in the render body, including ones that clone whole collections.

`views/console/users.rs:140-160` — runs on every keystroke in the search box, every chip toggle,
every tab switch inside the inspector:

```rust
let loaded = directory.read_unchecked().clone();          // clones the whole page
let (rows, total) = match &loaded {
    Some(Ok(page_data)) => {
        let filtered: Vec<DirectoryRow> = page_data
            .users
            .iter()
            .filter(|row| status.read().accepts(row))
            .filter(|row| !*staff_only.read() || row.permission_count > 0)
            .cloned()                                      // clones every surviving row again
            .collect();
        (filtered, page_data.total)
    }
    _ => (Vec::new(), 0),
};
```

Same pattern: `views/console/providers.rs:105-124` (clone + lowercase + filter + clone),
`views/console/users.rs:415-429` (**the same `BTreeSet<Permission>` is built twice in
consecutive statements** — once as the `use_signal` seed, once as `granted_now`, each doing a
`serde_json::from_value` round-trip per grant), `views/discover.rs:213-217`,
`views/series/tracking.rs:164-167`.

`views/console/mod.rs:315-326` rebuilds the entire rail group structure on every render
including the 4s refresh tick.

**Why it matters.** Not primarily throughput — these are small collections. The problem is that
Dioxus cannot skip work it cannot see is unchanged: because these are plain `let` bindings, the
whole subtree re-diffs on any signal read in the component, and `users.rs` re-clones the
directory page on each of the 25 rows' hover-state changes. `users.rs:415-429` is also a
correctness smell: two independently-computed sets that are compared for dirtiness
(`grants_dirty` at line 433) is exactly the shape where a future edit to one and not the other
produces a phantom dirty state.

**Remediation.** Wrap collection-producing derivations in `use_memo`:

```rust
let rows = use_memo(move || {
    let Some(Ok(page)) = &*directory.read() else { return Vec::new() };
    let (status, staff_only) = (*status.read(), *staff_only.read());
    page.users.iter()
        .filter(|row| status.accepts(row))
        .filter(|row| !staff_only || row.permission_count > 0)
        .cloned()
        .collect::<Vec<_>>()
});
```

For `users.rs:415-429`, compute `granted_now` once and seed `chosen` from it:

```rust
let granted_now = use_memo(move || known_permissions(&data.permissions));
let chosen = use_signal(|| granted_now.read().clone());
```

Prioritise: `console/users.rs`, `console/providers.rs`, `console/mod.rs`,
`series/tracking.rs`, `discover.rs`. UNVERIFIED: I did not profile; the argument is structural
(unnecessary clones + unmemoised derivations), not measured.

---

### F8 — 488 inline `style:` attributes bypass the design-token layer

**Severity: Medium** · **Effort: L**

**Evidence.** 488 `style: "…"` attributes across `src/`, concentrated in the console:

```
console/users.rs      53      series/tracking.rs    28      console/merge.rs      23
console/providers.rs  50      console/sync/queues   28      account/sync.rs       23
console/scans.rs      29      series/mod.rs         22      console/stats.rs      22
```

Most-repeated exact strings:

| Count | String |
|---|---|
| 29 | `font-size:12px;` |
| 16 | `font-size:13px;` |
| 13 | `font-weight:600;` |
| 10 | `text-align:right;` |
| 10 | `font-size:11px;` |
| 9 | `font-size:13px;margin-top:0;` |
| 8 | `margin-bottom:18px;` |
| 7 | `font-weight:600;font-size:13px;` |
| 6 | `font-size:12.5px;padding:9px 11px;` |
| 5 | `padding:12px;margin:14px 0;text-align:left;` |
| 5 | `font-size:9.5px;` |
| 4 | `grid-column:1 / -1;max-width:620px;` |

Note what is *absent*: there is essentially no Tailwind utility usage in `rsx!` at all. The
non-`ik-` classes in use are 22 × `grow`, 18 × `k`, 8 × `lbl`, and a handful of BEM-ish
element classes (`ttl`, `why`, `val`, `sub`, `nm`) — all hand-authored in `input.css`. So the
brief's "repeated Tailwind class strings" finding does not exist; **the inline `style:` strings
are its analogue and they are worse**, because they are invisible to the stylesheet, cannot
respond to the density knob, cannot be themed, and re-ship on every render.

`FRONTEND_AS_BUILT.md` §7 sanctions this ("inline `style:` strings are used for local tweaks"),
but 488 is not "local tweaks" — it is a parallel, undocumented style system roughly the size of
`input.css`'s component layer.

Directly relevant: `views/console/users.rs:749`, `:756`, `:763` and
`views/console/providers.rs:947` all hardcode `font-size:24px;` on `.ik-kpi-value` — i.e. four
sites overriding the same design-token-driven class the same way, which is the definition of a
missing variant.

**Why it matters.** The density knob (`data-density` → `--card`, `--gap`) and the light theme
cannot reach any of these 488 values. The as-built record already flags that the light block
"does not override `--faint-2`, `--icon-off`, `--surface-unread` sub-tints — check contrast
before adding new light-mode surfaces"; 488 hardcoded sizes and colours are exactly the surface
that check cannot cover.

**Remediation.** Not a big-bang rewrite. Three targeted passes, each independently shippable:

1. **Typography scale** (~120 sites). Add utilities to `input.css` below the Tailwind import:
   `.ik-t-xs{font-size:11px}` `.ik-t-sm{font-size:12px}` `.ik-t-md{font-size:12.5px}`
   `.ik-t-base{font-size:13px}` `.ik-t-mono-xs{font-size:10.5px}` `.ik-t-pill{font-size:9.5px}`.
   Replace `style: "font-size:Npx;"` with a class. This alone covers the top 6 offenders.
2. **KPI size variant** (4 sites): add `.ik-kpi-value.lg{font-size:24px}` and delete the
   overrides.
3. **Card/panel widths** (`max-width:620px` ×4, `max-width:560px` ×4): `.ik-panel-narrow`,
   `.ik-panel-wide`.

Leave genuinely computed styles inline (`width:{percent}%`, the `color-mix` tile at
`notifications.rs:271-274`). **Estimated LOC removed: ~60** — the value here is theming
correctness, not line count.

---

### F9 — `views/console/users.rs` (1,395) and `views/console/providers.rs` (1,385) are god files

**Severity: Medium** · **Effort: M**

**Evidence.** Both are single modules holding a list pane, an inspector shell, five tab bodies,
free-standing mutation functions, and sub-components. `users.rs` holds 9 components + 2 free
functions + 2 enums; `providers.rs` holds 9 components + 5 free functions + 1 enum + a test
module.

`discover.rs` (913) holds the Discover screen, the filter panel, 3 chip components, the active
filter bar, pagination (with its own `page_window` algorithm + `jump_to_page`), **and** the
entirely separate `Search` screen (`discover.rs:851-913`) which shares only `CoverCard`.

**Remediation — `views/console/users/`:**

| New file | Moves | ~LOC |
|---|---|---|
| `mod.rs` | `UsersEntity`, `StatusFilter`, `Tab`, `PAGE_SIZE` | 180 |
| `row.rs` | `UserRow` | 50 |
| `inspector.rs` | `UserInspector`, `UserEditor` header + tab dispatch | 300 |
| `identity.rs` | Identity tab body, `VerifyEmailAction` | 180 |
| `grants.rs` | `PermissionGrants`, `PresetPicker`, `GrantGroup`, `GrantRowView`, `PERMISSION_GROUPS` | 250 |
| `sync.rs` | `ExternalSync`, `SyncLinkRow` | 180 |
| `activity.rs` | `RecentActions` | 65 |
| `actions.rs` | `erase`, `revoke_all`, and the `save` closure lifted to a free fn | 150 |

**`views/console/providers/`:**

| New file | Moves | ~LOC |
|---|---|---|
| `mod.rs` | `ProvidersEntity`, `Tab`, `healthy_percent` | 180 |
| `row.rs` | `ProviderRow`, `HealthPill`†, `provider_state_token` | 120 |
| `inspector.rs` | `ProviderInspector` header + tab dispatch | 320 |
| `config.rs` | Config tab, `DryRunResult`, `parsed_count` + its tests | 180 |
| `politeness.rs` | Politeness tab | 110 |
| `coverage.rs` | `CoverageTab` | 60 |
| `runs.rs` | `RunsTab` | 70 |
| `danger.rs` | `DangerTab` | 130 |
| `create.rs` | `CreateProviderForm` | 145 |
| `test.rs` | `AdapterTestPanel` | 55 |

† `HealthPill` should move to `components/` per F4, not to `row.rs`.

**`views/discover.rs` → `views/discover/`:**

| New file | Moves |
|---|---|
| `mod.rs` | `Discover`, `Sort`, `clear_all`, the fetch builder |
| `filters.rs` | `FilterPanel`, `TypeChip`, `StatusChip`, `TagChip` |
| `active.rs` | `ActiveFilters` |
| `../components/pagination.rs` | `Pagination`, `page_window`, `jump_to_page` + tests (also used by Users, see F13) |
| `views/search.rs` | `Search` — a different route, move it out entirely |

Do this **after** F1/F3/F5/F6, so the sweeps land in the small files rather than being
re-applied across a split.

---

### F10 — Frontend DTOs: no drift, and the fix is already structural (positive finding, with one gap)

**Severity: Low (informational)** · **Effort: —**

**Evidence.** The brief asked whether frontend structs hand-mirror `crates/contracts`. They do
not, and cannot. `web/frontend/src/wire.rs` is three lines:

```rust
pub(crate) mod types {
    pub(crate) use tankovault_api_client::types::*;
}
```

`models.rs` (426 LOC) is a re-export surface plus presentation traits. Its module doc
(`models.rs:8-13`) records the exact history the brief flagged:

> Nothing here hand-mirrors a payload. It used to, for the `/v1/me/sync/*` endpoints the API
> service proxies verbatim, and those mirrors drifted silently … Those shapes now live in
> `tankovault_contracts::sync`, are returned by the producing service, are declared on the API's
> own routes, and arrive here generated — so that class of drift cannot recur.

`views/series/model.rs` (361 LOC) is **not** a DTO module despite its name — it is a view model
(`MergedChapter`, `RankedSource`, `ChapterGroup`, `merge_chapters`, `rank_sources`,
`group_chapters`, `next_unread`) with its own tests. Correctly placed.

Answer to "should `web/frontend` depend on `crates/contracts` directly": **no.** The current
chain (`contracts` → `utoipa` schema → `xtask openapi` → `progenitor` → `tankovault-api-client`)
is strictly better, because it guarantees the frontend sees what the *API actually serves*
rather than what a shared crate declares. A direct dependency would also drag `time` and the
full `utoipa` derive machinery into the wasm bundle for no benefit. `crates/contracts` is not
`no_std`/wasm-audited and there is no reason to make it so.

**The one gap.** Three types are still stringly typed or locally enumerated where the wire is
loose, each documented as deliberate but each a latent drift point:

- `models.rs:355-399` `ConflictPolicy` — "the wire carries a bare string (the sync service
  validates it), so this is the frontend's closed enumeration". Guarded by a round-trip test
  (`models.rs:412-417`), but nothing links it to the sync service's accepted set.
- `models.rs:208-221` `RequestKindExt::needs_export` — explicitly "Mirrors
  `RequestKind::needs_export` on the server. Duplicated rather than shared."
- `views/console/mod.rs:224-237` `ADAPTER_KINDS` + `adapter_token` — "Mirrors `AdapterKind`",
  a `&[(&str, &str)]` table beside a real enum. `providers.rs:1195-1199` then parses the string
  back into `AdapterKind` with a `_ => AdapterKind::Custom` fallback, so a typo in the table
  silently registers every provider as `Custom`.

**Remediation.** For `ADAPTER_KINDS`, derive the token from the enum
(`adapter_token(AdapterKind::…)` already exists) rather than maintaining a parallel table — a
`const ALL: [AdapterKind; 3]` plus `adapter_token` covers it and makes the round-trip
total. For the other two, add a `#[test]` asserting the frontend's set matches a constant
re-exported from the generated client where one exists; where none does, file it as an API gap
the way `users.rs:13-20` already files its two `TODO(api)` items.

---

### F11 — `assets/main.css` is Tailwind output, but `.gitignore` and the README call it hand-authored

**Severity: High** · **Effort: S**

**Evidence.** `.gitignore:24-27`:

```
# NOTE: `web/frontend/assets/main.css` is a hand-authored, self-contained
# stylesheet (see web/frontend/README.md) and MUST stay tracked. Only ignore
# CSS regenerated by the optional Tailwind CLI to a different file.
web/frontend/assets/tailwind.css
```

The first line of `web/frontend/assets/main.css`:

```
/*! tailwindcss v4.3.3 | MIT License | https://tailwindcss.com */
```

And `package.json`:

```json
"css:build": "tailwindcss -i ./input.css -o ./assets/main.css --minify"
```

The file is a 54 KB single-line minified Tailwind v4 build of `input.css` (975 lines). It is
**generated**, not hand-authored. `assets/tailwind.css` — the file `.gitignore` says the CLI
writes to — does not exist and no script produces it. `FRONTEND_AS_BUILT.md` §"Styling entry
point" gets this right; `.gitignore` and (per its own cross-reference) `README.md` do not.

**Why it matters.** This is an actively harmful instruction. A contributor who believes
`main.css` is hand-authored will edit it directly — the file is minified to one line, so the
edit is painful but possible — and the next `npm run css:build` silently destroys it. There is
also no CI check that `main.css` is in sync with `input.css` (see F2), so the reverse failure
— someone edits `input.css` and forgets to rebuild — ships a stylesheet that does not match
its source with no signal.

**Remediation.** Correct `.gitignore:24-27` and `web/frontend/README.md` to state that
`assets/main.css` is generated by `npm run css:build` from `input.css`, is committed
deliberately so `dx build` needs no Node toolchain, and must never be edited directly. Add the
`git diff --exit-code assets/main.css` CI step from F2. Delete the stale
`web/frontend/assets/tailwind.css` ignore line.

---

### F12 — 13 `ik-*` classes are defined and shipped but never referenced

**Severity: Medium** · **Effort: S**

**Evidence.** 172 `ik-*` classes defined in `input.css`; 158 referenced from `.rs`. Nothing is
referenced-but-missing (good). Unreferenced:

```
ik-alt          ik-btn-icon     ik-chapter-list   ik-checkline    ik-cons-bulk
ik-notebox      ik-notes-grid   ik-part-pill      ik-source-card  ik-sources
ik-tags         ik-tiles        ik-toast
```

Five of these are listed in `FRONTEND_AS_BUILT.md` §7 as part of the live inventory:
`ik-tiles`, `ik-part-pill`, `ik-source-card`, `ik-checkline`, `ik-toast`. §7 also states the
series page renders "`Read on` source cards" using `.ik-source-card` — it does not.

`ik-toast` is the notable one: the as-built record lists it under **States**, implying a toast
system. There is none — every transient message goes through `OutcomeLine`
(`feedback.rs:100-110`) or `ErrorLine`. That is arguably the better design, but the CSS and the
doc both claim otherwise.

Because these classes sit below the Tailwind import specifically so the content scanner cannot
purge them (`FRONTEND_AS_BUILT.md` §7), dead ones ship forever.

**Why it matters.** Small byte cost; larger comprehension cost. A designer reading §7 will
assume `ik-source-card` styling is live and safe to change, and a developer will assume a toast
primitive exists.

**Remediation.** Delete the 13 unreferenced rules, or implement the surfaces they describe.
Add a CI script (~15 lines) asserting the two sets match in both directions — the reverse
direction (used-but-undefined) is currently clean and worth keeping that way. Update
`FRONTEND_AS_BUILT.md` §7 either way.

---

### F13 — Pagination is implemented twice with different affordances

**Severity: Medium** · **Effort: S**

**Evidence.**

- `views/discover.rs:784-843` — full `Pagination`: prev/next, a collapsed page-number window
  (`page_window`, `discover.rs:742-772`, with its own gap-filling rule), an ellipsis, and a
  jump-to-page box with Enter handling (`jump_to_page`, `discover.rs:775-782`).
- `views/console/users.rs:235-265` — a bespoke footer: a range sentence plus bare
  Previous/Next buttons. No page numbers, no jump, no ellipsis.

Both compute the same offset arithmetic independently (`users.rs:158-160` vs
`discover.rs:225-228`), and `users.rs:160` derives `has_next` from the row count *after
client-side filtering* (`rows.len()`), while `total` comes from the server — so applying the
"staff only" chip on a full page can make Next incorrectly appear enabled. UNVERIFIED at
runtime, but the arithmetic at `users.rs:158-160` is:

```rust
let offset = *page.read() * PAGE_SIZE;
let has_prev = offset > 0;
let has_next = offset + i64::try_from(rows.len()).unwrap_or(0) < total;
```

`rows` is the *filtered* vector (`users.rs:143-149`), not the page size, so filtering out rows
shrinks the left-hand side and makes `has_next` true more often than it should be. The range
sentence at `users.rs:237-249` has the same bug — it reports "showing 1–N of TOTAL" where N is
the filtered count.

**Remediation.** Extract `components/pagination.rs` with `Pagination` + `page_window` +
`jump_to_page` and their tests. Give it a `compact: bool` prop for the console footer. Fix the
`has_next`/range arithmetic to use the *unfiltered* page length, or move the two chips
server-side alongside the existing `search` parameter.

**Estimated LOC removed: ~35** plus one behavioural bug fixed.

---

### F14 — A signed-out visit to `/console` shows a permanent loading skeleton

**Severity: High** · **Effort: S**

**Evidence.** `views/console/mod.rs:246-252`:

```rust
// Held back until the capability fetch lands: rendering "operators only" first and the
// console a moment later reads as a permission error to anyone who blinks.
if !caps.is_ready() {
    return rsx! {
        h1 { class: "ik-page-title", {i18n.t("nav.console")} }
        crate::components::SkeletonBlock { height: 220 }
    };
}
```

`CapabilitySet::is_ready()` (`state/capabilities.rs:69-71`) is true only in
`CapabilityState::Ready`. `components/shell.rs:137-150`:

```rust
use_resource(move || {
    let signed_in = session.is_authenticated();
    async move {
        if !signed_in {
            capabilities.clear();      // → CapabilityState::Loading
            return;
        }
        …
    }
});
```

So while signed out, capabilities are permanently `Loading` → `is_ready()` is permanently false
→ `/console` renders a skeleton forever. There is no timeout, no error state, and no
`SignInGate`.

Every other protected route handles this correctly and identically:
`views/account/mod.rs:84-89`, `views/home.rs:102-106`, `views/notifications.rs:172-176`,
`views/watchlist.rs:54-58` all early-return `SignInGate {}` on `!session.is_authenticated()`.
The Console is the only protected surface that does not.

Reachability: the route is public (`app.rs:50-51`, no guard), the rail link is hidden while
signed out (`nav.rs:27`, `caps.is_staff()`), but a bookmark, a shared link, a session expiry
while the page is open, or a sign-out from `/console` all land here.

**Why it matters.** A permanent skeleton is the worst failure mode available: it looks like the
app is working, so the user waits instead of signing in. It also breaks the "loading is never
indefinite" contract implicit in `FRONTEND_AS_BUILT.md` §8.

**Remediation.** Add the missing gate, and factor it so the fifth copy is the last:

```rust
// components/feedback.rs
/// Renders `children` only for a signed-in reader whose capabilities have landed.
#[component]
pub(crate) fn AuthGate(
    title: String,
    /// Also wait for the capability fetch — for surfaces gated on a permission.
    #[props(default = false)] needs_capabilities: bool,
    children: Element,
) -> Element {
    let session = use_session();
    let caps = use_capabilities();
    if !session.is_authenticated() {
        return rsx! { h1 { class: "ik-page-title", "{title}" } SignInGate {} };
    }
    if needs_capabilities && !caps.is_ready() {
        return rsx! { h1 { class: "ik-page-title", "{title}" } SkeletonBlock { height: 220 } };
    }
    rsx! { {children} }
}
```

`Console` wraps in `AuthGate { title: i18n.t("nav.console"), needs_capabilities: true, … }`;
the four existing sites drop their hand-rolled guard. Consider also bounding the capability
fetch: if a signed-in reader's `/v1/me/capabilities` call fails, `shell.rs:148` clears to
`Loading`, producing the same permanent skeleton for a *signed-in* operator during an API
outage. That path should surface an `ErrorBox` with retry.

---

### F15 — Icons: 14 of 53 variants unused behind a blanket `#[allow(dead_code)]` whose justification has expired

**Severity: Low** · **Effort: S**

**Evidence.** `icons.rs:10-12`:

```rust
/// … The full inventory is vendored up front; not every glyph is referenced yet (later screens
/// use the rest), so the unused variants are allowed until F2–F5 land.
#[allow(dead_code)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Icon {
```

F2–F5 have landed — `PROGRESS.md` and `FRONTEND_AS_BUILT.md` describe all eleven console tabs,
six account panels and every reader screen as built. 14 variants (26%) remain unreferenced:

```
Star  Radar  Merge  Group  History  Code  Dashboard  Public  Block  Person  Palette  Mail  Devices  Flag
```

Several are ironic: `Dashboard` is unused because `Icon::Console | Icon::Dashboard` share one
arm and callers picked `Console`; `Person` is unused for the same reason (`Account | Person`).
`Merge`, `Group`, `History`, `Flag`, `ShieldLock` were clearly intended for the console entity
rail, which ships with **no icons at all** (`views/console/mod.rs:411-420` renders a bare
`span` per entity) — a visible gap against `DESIGN_SPEC` §6-7.

**On the other questions the brief raised:** sizing and colour are consistent and correct — one
`Ic { icon, size }` component, `currentColor` stroke, `stroke-width: 2`, `aria-hidden="true"`,
uniform 24×24 viewBox. Macro generation would not help; a `match` returning `&'static str` is
already minimal and the paths are the only per-variant data. `dangerous_inner_html`
(`icons.rs:93`) is safe here because `path_for` returns `&'static str` from a closed `match` —
worth a one-line comment saying so, since the attribute name invites a security review every
time someone reads the file.

**Remediation.** Delete the unused variants (or wire the console rail icons, which is the
better fix and closes a design gap), then remove `#[allow(dead_code)]` so the next unused glyph
is caught. Once F2's clippy job exists, the allow is what stands between the codebase and that
signal.

---

### F16 — Accessibility: 27 ARIA attributes against 134 click handlers; the reader-facing surfaces are the weak ones

**Severity: Medium** · **Effort: M**

**Evidence.** Totals across `web/frontend/src`: 134 `onclick`, 27 `aria-*`, 4 `role`, 6
`onkeydown`. Breakdown of the 27: `aria-label` ×11, `aria-pressed` ×8, `aria-current` ×3,
`aria-hidden` ×2, `aria-checked` ×2, `aria-expanded` ×1.

The console is markedly better than the reader app — `console/mod.rs:408` labels its `nav`,
`console/mod.rs:415` sets `aria-current`, `console/shell.rs:47-53` gives `SegControl` a proper
`radiogroup`/`radio`/`aria-checked`, `console/shell.rs:164` labels the type-to-confirm input,
`console/users.rs:647-658` and `:1036` are correct. Concrete violations, in priority order:

| # | Site | Violation |
|---|---|---|
| 1 | `views/series/chapters.rs:275-288` | `div { class: "ik-chapter-toggle", onclick: … }` — the part-releases disclosure is a `div`. Not focusable, not keyboard-operable, no `role="button"`, no `aria-expanded`. **The only way to reach sub-chapter releases is a mouse click.** |
| 2 | `components/nav.rs:30` | The main `nav` has no `aria-label`. Two `<nav>` landmarks exist (rail + console rail); only the console one is named (`console/mod.rs:408`). |
| 3 | `components/nav.rs:102-115` | `NavLink` signals the active route with a CSS class only — no `aria-current="page"`. The console rail does it correctly one file over. |
| 4 | F3 (7 auth/password inputs) | No label association. |
| 5 | F6 (4 tab strips) | No `role="tablist"`/`role="tab"`/`aria-selected`. |
| 6 | `components/topbar.rs:51-67` | The global search input has `placeholder` but no `aria-label`; the placeholder disappears on input. `ListSearch` (`console/shell.rs:222`) does this correctly. |
| 7 | `components/topbar.rs:83-88` | The bell's unread count renders as a bare `span { class: "dot", "{unread}" }` inside a link labelled only "Notifications". Announced as "Notifications 3" with no unit, and no `aria-live` so a pushed update is silent. |
| 8 | `components/shell.rs:33-41` | No skip-to-content link. The rail is ~10 focusable items before the content on every route. `index.html` contains no skip link either. |
| 9 | `views/discover.rs:469-492` | The release-year "dual range" is two independent `<input type=range>` with no labels and no `aria-label`; `min`/`max` are hardcoded `"1970"`/`"2026"` string literals rather than the `YEAR_MIN`/`YEAR_MAX` constants declared at `discover.rs:28-29`. |
| 10 | `views/discover.rs:501-511` | The min-chapters slider is unlabelled (the visible `div.lbl` at `:497` is not a `<label>` and has no `for`). |

**Good news the audit should record.** There are **no modals or dialogs anywhere** — every
destructive action is inline (`InlineConfirm`, `TypeToConfirm`) — so the focus-trap and
focus-restoration class of bug the brief asked about does not exist. `TypeToConfirm`
(`shell.rs:171`) genuinely `disabled`s the button rather than dimming a live one, and says so.
Drag-and-drop on the Watchlist (`watchlist.rs:210-223`, `:330-335`) has a `<select>` keyboard
equivalent. Covers carry real `alt` text (`components/cover.rs:20`). `prefers-reduced-motion` is
honoured wholesale. `:focus-visible` outlines are global.

**Remediation.** Item 1 first — it is a functional lockout, not a degradation:

```rust
// views/series/chapters.rs:275
button {
    class: "ik-chapter-toggle",
    r#type: "button",
    "aria-expanded": if *expanded.read() { "true" } else { "false" },
    onclick: move |_| { let next = !*expanded.read(); expanded.set(next); },
    Ic { icon: Icon::ChevronRight, size: 14,
         class: if *expanded.read() { "ik-chevron open" } else { "ik-chevron" } }
    span { "{toggle_label}" }
}
```

(`.ik-chapter-toggle` will need `background:none;border:0;width:100%;text-align:left;` in
`input.css`.) Items 2, 3, 6, 7 are one-line additions. Item 8 is a `<a class="ik-skip"
href="#ik-content">` in `Shell` plus an `id` on the content `section`. Items 9-10 fold into the
`Field` work from F3.

---

### F17 — Two hardcoded English strings, and one is user-visible

**Severity: Low** · **Effort: S**

**Evidence.** i18n discipline is otherwise exemplary — every `rsx!` string child resolves
through `i18n.t`/`args`/`plural`, catalogues are `include_str!`-baked so a malformed one is a
build failure (`i18n.rs:50-59`), unknown placeholders render visibly rather than silently
(`i18n.rs:207-235`), and `locales_define_the_same_keys` (`i18n.rs:280-299`) enforces structural
parity between `en.json` and `de.json`. Two exceptions:

`util.rs:29` — user-visible:

```rust
pub(crate) fn save_text_file(filename: &str, mime: &str, contents: &str) -> Result<(), String> {
    let failed = || "your browser would not accept the download".to_owned();
```

That `String` is surfaced verbatim to the reader via `views/account/privacy.rs:64` and
`views/console/privacy.rs:341`. It directly contradicts the module's own contract at
`util.rs:4-6`: *"Anything with words in it resolves through `crate::i18n` rather than baking
English into the formatter."*

`views/console/providers.rs:1236-1256` — three English placeholder examples (`"acme-scans"`,
`"Acme Scans"`, `"https://acmescans.example"`). Defensible as illustrative slugs, but "Acme
Scans" is a display *name* example and reads as untranslated copy to a German operator.

**On missing/unused key detection:** the missing-key half is covered by
`locales_define_the_same_keys` — but only structurally (en ≡ de), and only if CI runs it (F2).
There is **no unused-key check**: a catalogue entry whose `i18n.t("…")` call site is deleted
stays in both locales forever, and translators keep translating it.

**Remediation.** Change `save_text_file` to return a catalogue *key* (`Result<(), &'static
str>`) and let the two call sites resolve it — they both already hold a `Translator`. This
matches the pattern `politeness_json` already uses (`console/mod.rs:512-527` returns
`Err(&'static str)` catalogue keys). For the placeholders, either accept them explicitly with a
comment or route them through the catalogue. Add an unused-key test alongside the parity one:
walk `en.json`'s leaf paths and assert each appears in a `grep` of `src/` — feasible because
every key is a string literal at its call site except the two computed ones
(`providers.rs:301` `format!("console.providerState.{state}")` and `i18n.rs:181`
`format!("{key}.{form}")`), which can be allow-listed by prefix.

---

### F18 — Asset delivery: no cache headers and no CSP

**Severity: Low** · **Effort: S** · *(in-scope portion of `services/frontend/src/main.rs` only)*

**Evidence.** `services/frontend/src/main.rs:143-160` serves the bundle via
`ServeDir::new(static_dir).fallback(ServeFile::new(index))` wrapped in three
`SetResponseHeaderLayer`s: `X-Content-Type-Options: nosniff`, `Referrer-Policy: no-referrer`,
`X-Frame-Options: DENY`. Routing itself is correct — SPA fallback to `index.html` is tested
(`unknown_paths_fall_back_to_the_app_shell`), hardening headers are scoped to the static branch
only so proxied `/v1/*` responses keep the API's own headers, and the `/v1/*` proxy streams
unbuffered for SSE.

Two gaps:

1. **No `Cache-Control`.** `tower-http`'s `ServeDir` sets none by default. The Dioxus/manganis
   pipeline content-hashes assets (`app.rs:89-96` uses `asset!()` precisely so the `.woff2`
   files are hashed) — hashed filenames are the whole point of
   `Cache-Control: public, max-age=31536000, immutable`, and none is sent. Conversely
   `index.html` needs `no-cache` or a stale shell will keep loading a deleted hashed bundle
   after a deploy.
2. **No `Content-Security-Policy`.** `X-Frame-Options: DENY` is set but not its modern
   equivalent `frame-ancestors 'none'`. More relevant to this frontend specifically: the app
   uses `document::eval` in five places (`state/prefs.rs:71`, `:101`, `i18n.rs:127`,
   `console/mod.rs:481`) and `dangerous_inner_html` in `icons.rs:93`. All are safe on
   inspection — eval scripts are `const`-built with `serde_json::to_string` escaping for any
   dynamic value, and the icon HTML is `&'static str`. But a CSP is exactly the defence-in-depth
   for the case where that stops being true, and the app's own threat model
   (`state/mod.rs:3-4`: the access token is memory-only "so an XSS foothold cannot exfiltrate
   it") shows XSS is already a considered risk.

**Remediation.** Split the static service into two `ServeDir` branches, or add a small layer
that sets `Cache-Control` by path (`immutable` for hashed assets, `no-cache` for
`index.html`). Add a CSP compatible with WASM and the inline boot script in `index.html` —
which will need `'wasm-unsafe-eval'` for the WASM instantiation and either a nonce or a hash
for the theme-flash-prevention script:

```rust
.layer(SetResponseHeaderLayer::if_not_present(
    header::CONTENT_SECURITY_POLICY,
    HeaderValue::from_static(
        "default-src 'self'; script-src 'self' 'wasm-unsafe-eval'; \
         style-src 'self' 'unsafe-inline'; img-src 'self' https: data:; \
         connect-src 'self'; frame-ancestors 'none'; base-uri 'none'",
    ),
))
```

`img-src https:` is required — covers are remote provider URLs (`components/cover.rs:19`).
Verify against the inline `index.html` script before enabling; `style-src 'unsafe-inline'` is
unavoidable while 488 inline `style:` attributes exist (F8), which is a further argument for
that finding. Backend/security agents own the CSP value itself — flagged here because the
delivery path is frontend-owned.

---

## (c) Phased refactor plan

Each phase is independently shippable and independently valuable. Phases 0-2 are the ones that
actually matter; 3-5 are cleanup that becomes cheap once 0-2 land.

---

### Phase 0 — Make the existing quality machinery load-bearing *(≈1 day)*

No production code changes. Everything after this phase is protected by it.

1. **F2** — replace the CI frontend job with fmt + clippy(`-D warnings`) + `cargo test` +
   css-in-sync. Expect an initial clippy backlog; land the job with `continue-on-error: true`
   for clippy only, fix the backlog, then flip it.
2. **F11** — correct `.gitignore:24-27` and `web/frontend/README.md` about `main.css`'s
   provenance. Delete the stale `assets/tailwind.css` ignore.
3. **F12** — add the two-way `ik-*` class check as a CI script (used-but-undefined must stay at
   zero; defined-but-unused starts at 13, so allow-list those and burn them down in Phase 5).
4. **F17** — add the unused-catalogue-key test alongside `locales_define_the_same_keys`.

**Exit criterion:** a deleted `de.json` key, an unformatted file, a pedantic-lint regression, a
stale `main.css`, and an unreferenced CSS class all fail CI.

---

### Phase 1 — Correctness: fix what is actually broken *(≈1-2 days)*

Small, high-severity, no structural churn.

1. **F14** — add `AuthGate`; wire `Console` through it. Fixes the permanent-skeleton lockout.
   Convert the four existing hand-rolled gates in the same PR.
2. **F16 item 1** — `views/series/chapters.rs:275` `div` → `button` + `aria-expanded` + the CSS
   reset. Fixes a keyboard lockout on the sub-chapter disclosure.
3. **F3** — extract `components/form.rs::Field`; convert the 7 unlabelled auth/password inputs.
   Add `autocomplete` attributes while there.
4. **F13** — fix the `has_next`/range arithmetic in `views/console/users.rs:158-160` and
   `:237-249` (use unfiltered page length).
5. **F17** — `save_text_file` returns a catalogue key.

**Exit criterion:** no keyboard-unreachable controls, no unlabelled auth inputs, no route that
can hang on a skeleton.

---

### Phase 2 — Restore the `async_view` invariant *(≈3-4 days — the largest single win)*

1. Unify `RefreshTick` (`console/mod.rs:77-89`) with `Reload` (`hooks.rs:14-27`), or widen
   `async_view`'s retry parameter to `impl Fn()`. **Prerequisite for everything below.**
2. Delete the `Option<Option<Result<…>>>` signed-out idiom from `console/{overview, stats,
   audit, solver, scans}.rs` — `Api::client()` already handles token changes.
3. Add `async_block(resource, reload, height, content)` to `components/feedback.rs`.
4. Export `EmptyBox`; sweep the 28 hand-rolled `ik-empty` divs (**F5**).
5. Sweep the 12 identical hand-rolled `ik-skeleton` divs to `SkeletonBlock`; add
   `SkeletonLines` for the 5 bespoke ones (**F5**).
6. Convert the 35 bypassing `use_resource` sites (**F1**), heaviest first:
   `console/sync/queues.rs` (6) → `console/sync/inspector.rs` (4) → `series/tracking.rs` (3
   remaining) → `console/{scans,solver,merge}.rs` (2 each) → the singletons.
7. Introduce `use_api_resource<T>` to collapse the repeated prologue:

```rust
// hooks.rs
/// A resource that fetches through the API handle and turns failures into a reader-facing
/// sentence. Subscribes to `reload` and to the session token automatically.
pub(crate) fn use_api_resource<T, F, Fut>(reload: Reload, fetch: F) -> Resource<Result<T, String>>
where
    T: 'static,
    F: Fn(Client) -> Fut + Copy + 'static,
    Fut: Future<Output = Result<ResponseValue<T>, ApiOpError<...>>> + 'static,
{
    let api = api::use_api();
    let i18n = use_i18n();
    use_resource(move || {
        reload.track();
        let client = api.client();
        async move {
            fetch(client).await.map(ResponseValue::into_inner).map_err(|e| api::friendly_error(i18n, e))
        }
    })
}
```

   UNVERIFIED: the exact generic bound depends on `progenitor`'s per-operation error type; if a
   single signature does not cover all 59 operations, a macro (`api_resource!`) achieves the
   same collapse. The repeated body is 8 lines × 59 sites either way.
8. Delete `Brush` or use it; correct `FRONTEND_AS_BUILT.md` §7 (**F12**).

**Exit criterion:** every `use_resource` in `views/` renders through `async_view`/`async_list`/
`async_block`. Add a CI grep asserting `read_unchecked()` appears only in `components/feedback.rs`
and the handful of legitimately-derived reads, so the invariant becomes mechanically enforced
rather than documented.

**Estimated LOC removed in this phase alone: ~500.**

---

### Phase 3 — Consolidate the component layer *(≈2-3 days)*

Now that call sites are uniform, extraction is mechanical.

1. **F4** — move `views/console/shell.rs` → `components/{layout,forms,confirm}.rs`, `pub(crate)`.
   Promote `PanelCard`. Unify the two `Kpi`s (+ the third inline one at `users.rs:746-765`).
   Retype `HealthPill` to `ProviderState` and move it to `components/`; delete the two
   sibling-view imports (`stats.rs:8`, `solver.rs:12`).
2. **F6** — `TabBar<T: TabKind>`; convert the 4 strips, with `role="tablist"` / `role="tab"` /
   `aria-selected`. Add arrow-key navigation once, in one place.
3. **F13** — extract `components/pagination.rs` (move `page_window` + `jump_to_page` + tests);
   add `compact` variant; use it in Users.
4. **F3 (continued)** — `KvField` for the 17 `ik-kv` blocks in `console/{providers,users}.rs`.
5. `AsyncSection`, `DataTable`/`TableCard`, `Avatar`/`MonoTile`, `StatusPill<T>` — extract as the
   Phase-4 splits expose them, not speculatively.
6. **F16 items 2, 3, 6, 7, 8** — nav `aria-label`, `aria-current`, search `aria-label`, bell
   count `aria-label` + `aria-live`, skip link.

---

### Phase 4 — Split the god files *(≈2-3 days)*

Deliberately **after** Phases 2-3, so the sweeps land once rather than being reapplied across
eight new files each.

1. `views/console/users.rs` (1,395) → 8 modules per the F9 table.
2. `views/console/providers.rs` (1,385) → 10 modules per the F9 table.
3. `views/discover.rs` (913) → `views/discover/{mod,filters,active}.rs` +
   `views/search.rs` + `components/pagination.rs` (already moved in Phase 3).
4. **F15** — wire the console entity-rail icons (`views/console/mod.rs:411-420`), which consumes
   `Merge`/`Group`/`History`/`Flag`/`ShieldLock`/`Radar`; delete whatever remains unused; drop
   `#[allow(dead_code)]` from `icons.rs:10`.
5. Consider `views/series/{mod,chapters,tracking}.rs` (519/674/701) next — `ChapterRow` (10
   props) and `TrackingCard` (11 props) are the remaining prop-drilling hotspots.

---

### Phase 5 — Styling and performance polish *(≈2-3 days, lowest urgency)*

1. **F8** — the three targeted passes: typography scale utilities (~120 sites), KPI size variant
   (4 sites), panel width classes (8 sites). Leave computed styles inline.
2. **F7** — `use_memo` for the collection-producing derivations in `console/users.rs`,
   `console/providers.rs`, `console/mod.rs`, `series/tracking.rs`, `discover.rs`. Fix the
   double-built `BTreeSet<Permission>` at `users.rs:415-429`.
3. **F15/F12** — burn down the CSS allow-list from Phase 0 to zero.
4. **F18** — `Cache-Control` by path and a CSP in `services/frontend/src/main.rs` (coordinate
   with the security-owning agent on the CSP value; note `style-src 'unsafe-inline'` can only be
   dropped after F8 completes).
5. **F10** — derive `ADAPTER_KINDS` from `AdapterKind` rather than maintaining the parallel
   table; add parity tests for `ConflictPolicy` and `RequestKindExt::needs_export`.
6. **F15** — a one-line comment at `icons.rs:93` explaining why `dangerous_inner_html` is safe
   there, so future reviewers stop re-deriving it.

---

## Appendix — measurements

| Metric | Value |
|---|---|
| Rust LOC (`src/`, excl. `target/`) | 16,121 |
| `use_resource` call sites | 59 |
| …routed through `async_view`/`async_list` | 24 (41%) |
| `read_unchecked()` outside `components/feedback.rs` | 42 |
| Inline `style:` attributes | 488 |
| `class:` attributes with a literal (no interpolation) | ~640 |
| Tailwind utility classes used in `rsx!` | 0 |
| `ik-*` classes defined in `input.css` | 172 |
| …referenced from `.rs` | 158 (0 undefined, 13 unused) |
| `onclick` handlers | 134 |
| `aria-*` attributes | 27 |
| `role` attributes | 4 |
| `onkeydown` handlers | 6 |
| Modals / dialogs | 0 (by design) |
| `use_memo` calls | 0 |
| `#[component]` fns with ≥6 props | 15 (max: `FilterPanel`, 14) |
| `#[test]` functions | 41 (0 run in CI) |
| Hardcoded English strings in `rsx!`/user-facing paths | 2 |
| Locales | 2 (`en`, `de`), structurally parity-tested |
| `Icon` enum variants | 53 (14 unused, 26%) |
| Frontend CI steps | 1 (`cargo check`) |
| `assets/main.css` | 54 KB, 1 line, Tailwind v4.3.3 output |
