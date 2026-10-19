# Matching overhaul — implementation plan

**Audience:** the engineer or agent implementing this. **Status:** WP0, WP1 and WP2 are
implemented (see [§9](#9-implementation-notes) for where the code departs from the letter of this
plan); WP3–WP8 are planned. **Source:** an analysis of the production catalogue (2026-10-19) against the code
at `5cad031`. Every production number below came from a query in [Appendix A](#appendix-a--production-evidence);
re-run them before acting on any number that matters to a decision.

Read [`CLAUDE.md`](../CLAUDE.md) and [`ENGINEERING_GUIDE.md`](ENGINEERING_GUIDE.md) first. Their
rules apply to every work package here and are not repeated except where a package is likely to
trip one.

---

## 1. Summary

The automatic merge system (`crates/matcher`, `crates/db/src/repo/matching`,
`services/control-plane/src/dedupe.rs`) and the ingest attach path
(`crates/db/src/repo/catalog/{series,sources,ingest}.rs`) both merge works that are different and
miss works that are the same. The production evidence says the causes are, in order of damage:

1. **Unreliable metadata decides matches.** Author, release year and content type are wrong or
   inconsistent across providers most of the time, yet the scorer adds bonuses for them
   (`type_agreement` +0.08, `year_agreement` +0.06, `shared_author` +0.10) and those bonuses are
   exactly what lifts an alternative-title hit (deliberately discounted to 0.80) back over the
   0.85 attach threshold. As guards they were so noisy that prod switched three of the four off,
   after which 442,900 of 480,209 automatic merges ran with only the numeric guard.
2. **An alternative title alone counts as identity.** `MatchSignals::is_structural` includes
   `alias_identity`, so a shared junk alias (`unknown`, `status`, `sorry`, `x`, `life`) or a
   franchise-wide synonym (`sao`, `fairy tail`, `tokyo ghoul`) merges different works. Every merge
   copies the absorbed series' aliases onto the survivor, so one bad merge pulls in the next: a
   dozen "hub" survivors absorbed 20–100 series each.
3. **Mirror providers create duplicate series.** About half of the 236k sources come from mirror
   families that list the same catalogue. A mirror listing only matches a series that AniList has
   renamed through an alias, which is discounted below the attach threshold, so registration
   creates a new series that the sweep then merges — a churn that also spreads every bad alias.
4. **The numeric guard can be bypassed.** `numeric_signatures_agree` passes when *any* title of
   the candidate carries the same numbers, so once a sequel's alias lands on a series every
   further sequel attaches (Douluo Dalu 4 merged into 5).
5. **AniList enrichment trusts its top search hit and accepts light novels.** `Media(search:,
   type: MANGA)` includes `format: NOVEL`; nothing checks the hit against the series' titles.

The trusted signals left are: primary titles (the canonical title and every source's own listing
title), first-party chapter timing (`chapters.discovered_at`) for ongoing works, and an AniList id
once WP4 makes it trustworthy. This plan rebuilds the decision on those, then repairs the existing
damage. Image comparison (covers or pages) was evaluated and rejected on cost — see
[§8](#8-rejected-alternatives).

Total estimate: **2.5–3.5 engineer-weeks**, sliced into the PRs in [§6](#6-sequencing-and-pr-slicing).

---

## 2. Production evidence, condensed

| Finding | Number | Implication |
|---|---|---|
| Series / sources | 76k / 236k | — |
| Series with no source (orphans) | 23,500; 22,696 have a sourced twin with the same normalized title | Leftovers of a fixed bug (`f686b3b`, 2026-09-30); cleanup only (WP6) |
| Orphans inside AniList-id clusters of >16 series | 22,967 of 24,661 clustered | The clusters are copies of one title, kept apart by `MAX_KEY_FANOUT` |
| Automatic merges total | 480,209 (10 reverted, 0 flagged) | No operator labels exist |
| …of which absorbed side had no sources | 387,033 | Copy cleanup, correct |
| …of which absorbed side had sources | 93,177 (50,151 exact title, 42,297 alias) | The population that can be wrong |
| Merges run with type/year/author guards off | 442,900 | Guards were switched off at runtime via tunables |
| Sourced merges where a disabled guard fired | 13,295, of which 12,356 via alias | Alias route dominates the suspicious set |
| Sourced merges with AniList id on both sides | 26,385; 1,828 disagree (1,296 via alias) | Many exact-title disagreements are manga-vs-novel AniList entries (consecutive ids) — correct merges |
| Alias merges checkable by live chapter timing | 3,773 of 42,294 (9%): 3,465 agree (median gap 4.2 h), 297 unclear, 11 contradict | Live timing is a strong confirmation where it exists; coverage grows weekly |
| Provider `published_at` gap on shared chapters | mean 1,621 days on "contradicting" pairs | Provider dates are unreliable; only `discovered_at` is usable |
| Deferred-merge journal rows | 1,800,193 | Journal bloat (WP7) |
| Mirror families | manganato (~53k sources across 6 slugs), kunmanga (~62k across 3), mangahere/fanfox/mangatown (~15k) | Mirror churn (WP3) |

Hub survivors confirmed contaminated (absorbed different works), from Appendix A query A6:
`01a0b9c3-0cbb-7671-bfc4-1b728557f95e` (100 absorbed), `01a0b9c7-6dcf-73ea-9466-615f8cf09ef5`,
`01a0b9ca-1366-7425-b9a0-f4efe66709c9`, `01a0b9f7-4f2e-7715-acfe-4b7d57461927`,
`01a0b9c6-8f45-7221-9588-52ca27685135`, `019ffff3-396e-7063-a334-cfe9c62484da`,
`01a0b9c7-99b7-713a-833d-44e0825a8498`, `01a0b9ce-15af-70ca-8dcf-3318be8bb335`,
`01a0b9c5-2fe8-70dc-a3d6-ed5e6c1c5f8d`, `01a0b9c5-00b1-7225-901f-bda35a1c3a79`,
`01a000c1-6325-7b90-8049-6467f8d4822a`, `01a0b9f2-6edc-701c-ada6-fa3981fcbe55`,
`01a0b9f7-b0b5-776a-9e64-dcc6ee9e8153`. Most other large survivors (Skeleton Soldier, +99 Wooden
Stick, Star-Embracing Swordmaster, …) are one work absorbing its own copies and are correct.

---

## 3. Target decision model

### 3.1 Signal trust

| Signal | Trust | Use |
|---|---|---|
| Canonical title, exact or compact (`exact_title`, `compact_identity`) | High | Identity |
| Another source's listing title, exact or compact (new: `source_title_identity`) | High | Identity |
| Alternative title (`alias_identity`) | Low | Evidence only; needs corroboration |
| Trigram / token / edit similarity | Low | Candidate retrieval and review ordering |
| Numbers in titles (`numeric_conflict`) | High as a veto | Veto, computed against each side's **own** titles |
| Edition/arc markers (new: `edition_conflict`) | High as a veto | Veto |
| AniList id after WP4 (new: `external_id_agreement` / `external_id_conflict`) | High | Corroboration / veto |
| Live chapter timing (new: `live_timing_agreement` / `live_timing_conflict`) | High where present | Corroboration / veto |
| Author, release year, content type, tags | **Untrusted** | Recorded and displayed only; never moves a score or a verdict |

### 3.2 Decision rules

Ingest (`decide`, used by `resolve_canonical_series` and the sync resolver):

| Condition | Decision |
|---|---|
| Any veto fires | `Create` (numeric) or `Ambiguous` (others) — never `Attach` |
| Primary identity (`exact_title`, `compact_identity`, `source_title_identity`) | `Attach` |
| Alias identity only | `Ambiguous` |
| Fuzzy score ≥ `high` with no identity | `Ambiguous` (was `Attach`) |
| Fuzzy score in `[low, high)` | `Ambiguous` |
| Below `low` | `Create` |

Standing sweep (`adjudicate`):

| Condition | Verdict |
|---|---|
| `numeric_conflict` or `edition_conflict` | `Distinct` |
| `external_id_conflict` or `live_timing_conflict` | `Review` (reason names the veto) |
| Primary identity and score ≥ `auto_merge` | `Auto` |
| Alias identity **and** (`external_id_agreement` or `live_timing_agreement`) | `Auto` |
| Alias identity only | `Review` (`alias_uncorroborated`) |
| No identity | `Review` / `Distinct` by score, as today |

The fuzzy-score attach at ingest is removed on purpose: the trigram/edit path is what attached
"Onikakushi-hen" to "Onisarashi-hen" and "October" to "November". It now files an `Ambiguous`
candidate and the sweep decides with full facts.

---

## 4. Work packages

Each package lists files, the change, the tests that must pin it, the gates to run, and the
acceptance check. Per `CLAUDE.md` rule 8, every regression test's doc comment names the
production bug it pins (quote the titles from this document).

### WP0 — Offline replay harness (do first; every later package is measured with it)

**Why.** `merge_decisions.evidence` stores both sides' full facts for every sweep decision
(`services/control-plane/src/dedupe.rs::evidence_json`). That is enough to re-adjudicate every
historical merge offline with a changed matcher, without a database.

**Change.**
- `xtask/src/` — new subcommand `matching-replay --input <jsonl> [--out <jsonl>]`. Reads rows
  exported by Appendix A query A10, rebuilds both `SeriesMatchFacts`-equivalents from
  `evidence.left` / `evidence.right`, runs the same `best_reading` logic as the sweep (lift
  `best_reading`, `query_of`, `candidate_of` out of `services/control-plane/src/dedupe.rs` into
  `crates/matcher` as a pure `adjudicate_pair(left, right, trigram, thresholds)` so the harness
  and the sweep share one implementation), and reports old verdict → new verdict counts plus the
  changed rows.
- Fixture sets, committed under `crates/matcher/tests/fixtures/` (titles and facts only, no ids
  that identify users):
  - `confirmed_same.jsonl` — the 3,465 `live_agree` merges (query A9 with
    `verdict = 'live_agree'`). Must stay `Auto` after every package.
  - `confirmed_wrong.jsonl` — hand-checked wrong merges from this analysis (hub absorptions,
    `unknown`/`status`/`sao`/`fairy tail` keys, Douluo Dalu 4/5, Higurashi arcs, Kaiji parts,
    A Year of Marvels months). Must not be `Auto` after WP1+WP2.
- `crates/matcher/tests/replay.rs` runs both fixture sets as an ordinary test.

**Gates.** `cargo check -p xtask -p tankovault-matcher`; `cargo test -p tankovault-matcher --test replay`.

**Acceptance.** The harness reproduces the stored verdict for ≥ 99% of an unmodified-matcher
replay of the export (differences come only from facts that changed since; list them).

### WP1 — Untrusted metadata stops deciding; aliases need corroboration

**Files.** `crates/matcher/src/assess.rs`, `crates/matcher/src/decide.rs`,
`crates/matcher/src/types.rs`, `crates/domain/src/matching.rs`, `crates/matcher/src/tests.rs`,
`crates/matcher/tests/prop_scoring.rs`, `crates/config/src/matching.rs` (doc examples).

**Change.**
1. In `assess_with`, the `type_agreement`, `type_conflict`, `year_agreement`, `year_conflict`,
   `tag_overlap` and `shared_author` terms keep setting their `MatchSignals` fields and keep
   writing a `ScoreTerm` (so explanations still show them) but with `delta = 0.0`. Do not delete
   the signals: `MatchSignals::labels` slugs are persisted and rendered.
2. `MatchSignals`: add `source_title_identity`, `edition_conflict`, `external_id_agreement`,
   `external_id_conflict`, `live_timing_agreement`, `live_timing_conflict`. **Append** their slugs
   to `labels()` (order is persisted). Replace `is_structural()` with two methods:
   `is_primary_identity()` (`exact_title || compact_identity || source_title_identity`) and
   `is_corroborated_alias()` (`alias_identity && (external_id_agreement || live_timing_agreement)`).
   Grep every caller of `is_structural` (`dedupe.rs::best_reading` ranks by it) and decide per
   call site; `best_reading` should rank primary identity, then corroborated alias, then score.
3. `decide`: implement §3.2 ingest table. `Attach` requires primary identity and no veto.
4. `adjudicate`: implement §3.2 sweep table. New reason slugs: `alias_uncorroborated`,
   `external_id_conflict`, `live_timing_conflict`, `edition_conflict`.
5. `MergeGuards` and the four `block_auto_merge_on_*` tunables: leave the keys and defaults alone
   in this package. With metadata out of the score, `author/year/type` guards only matter if an
   operator turns them back on; deprecating them is a separate config-surface change (needs
   `docs/CONFIGURATION.md` + `just regenerate`) and is out of scope here.

**Tests that change meaning.** `an_alias_hit_that_agrees_on_the_medium_still_attaches` and
`shared_author_lifts_an_ambiguous_match` pin the behaviour this package removes. Do not delete
them silently (rule 5): invert them and rewrite their doc comments to state why — cite the hub
survivors and the 12,356 alias merges where a metadata guard fired. The `MatchingConfig` doctest
bands (`scoring(0.9)` attaches) also change: a fuzzy 0.9 with no identity is now `Ambiguous`.

**Knock-on.** `services/sync/src/engine/resolve.rs` scores remote tracker entries through the
same matcher. Expect fewer automatic tracker mappings for entries that only matched on fuzzy
score + metadata. Run `cargo test -p tankovault-sync` scoped to `reconcile_tests` and report the
change.

**Gates.** `cargo check -p tankovault-matcher -p tankovault-config -p tankovault-control-plane
-p tankovault-sync`; `cargo test -p tankovault-matcher`; WP0 replay. Doc tests in
`tankovault-matcher` and `tankovault-config` change, so run `cargo test --doc -p
tankovault-matcher -p tankovault-config`.

**Acceptance.** Replay: `confirmed_same` all `Auto` *only where* their stored signals include a
primary identity; alias-only `confirmed_same` rows become `Review` until WP4/WP5 add
corroboration (expected and reported, not a failure). `confirmed_wrong` none `Auto`.

### WP2 — Title rules: numbers, editions, junk aliases, franchise names

**Files.** `crates/matcher/src/similarity.rs`, `crates/matcher/src/title.rs`,
`crates/matcher/src/assess.rs`, new `crates/matcher/src/edition.rs`,
`crates/domain/src/normalize.rs` (only if a normalization rule changes — see note).

**Change.**
1. **Numeric check against own titles.** `numeric_signatures_agree` currently returns true if
   *any* candidate title matches the query's numbers. Change to: compare the query title against
   the candidate title the comparison was actually scored against (`TitleMatch::matched`) **and**
   against the candidate's canonical title; a conflict on the canonical title is a conflict even if
   an alias agrees. Add roman numerals (`ii`…`x` as standalone tokens) and spelled ordinals
   (`second`, `third`, `2nd`, `season 2`) to the signature.
2. **Edition/arc markers** (`edition_conflict`). A token present on one side and absent on the
   other, from a fixed list: `gaiden`, `side story`, `after story`, `extra`, `anthology`,
   `4-koma`/`yonkoma`, `oneshot`, `pilot`, `colored`, `remake`, `re`, `zero`, `ova`, `movie`,
   `doujinshi`, `-hen` / `hen` arc suffixes (compare the arc word before `hen`), `part`, `arc`,
   `season`, single trailing capital letters on the raw title (`Railgun S/T`, `Symphogear G/GX`).
   Keep the list in one `const` with each entry's reason, as `NOISE_WORDS` does.
3. **Alias distinctiveness.** A function `alias_is_distinctive(normalized: &str) -> bool`:
   false when the compact key is shorter than 8 characters, when it is a single token shorter than
   10 characters, or when it is on a deny-list (`unknown`, `status`, `none`, `untitled`,
   `no alternative titles`, `sorry`, `please`, `delete`, `season 2`, `manhwa`, `manga`, `fantasy`,
   `encode`, `right`, `nihongo` language-name strings). An alias hit on a non-distinctive alias
   does not set `alias_identity`. Apply the same predicate at **write time** in
   `crates/db/src/repo/catalog/enrichment.rs::add_series_titles` so junk never reaches
   `series_titles` again.
4. **Franchise names.** When the matched alias is a strict token subset of the *other* side's
   canonical title (`sao` against `Sword Art Online: Fairy Dance`, `fairy tail` against
   `Fairy Tail: 100 Years Quest`), it does not set `alias_identity`; it sets `containment` only.

Note on normalization: none of the above changes `normalize_title`. If you do change it (for
example to fold `×` to `x`), `normalized_title` is a persisted key — you must also run the
`rebuild_normalized_keys` operator action after deploy and say so in the PR.

**Tests.** One per rule, each naming the production case: Douluo Dalu 4 vs 5 via alias
`douluo dalu 4 zhongji douluo`; `Overlord II`; Higurashi `Onikakushi-hen` vs `Onisarashi-hen`;
`Railgun S` vs `T`; alias `unknown` (11 merges into 9 survivors); alias `sao`; `Tokyo Ghoul:re`
vs `Tokyo Ghoul`. Add property tests to `tests/prop_scoring.rs`: an edition token present on one
side only never yields `Auto`.

**Gates.** `cargo check -p tankovault-matcher -p tankovault-db`; `cargo test -p
tankovault-matcher`; WP0 replay.

**Acceptance.** Replay: every `confirmed_wrong` case that is a numeric, edition, junk-alias or
franchise case is `Distinct` or `Review`; `confirmed_same` unchanged versus WP1.

### WP3 — Ingest: listing titles as identity, alias filing, mirror groups

**3a. Source listing title as a match key.**
- Migration `migrations/0066_source_title_key.up.sql` (+ `.down.sql`): add
  `series_sources.provider_title_normalized text` and an expression index on its compact form
  (`replace(provider_title_normalized, ' ', '')`), mirroring `series_compact_title_idx` from
  `0023_merge_queue.up.sql`. No SQL backfill: normalization lives in Rust
  (`tankovault_domain::normalize_title`) and an SQL twin would drift. Instead extend
  `crates/db/src/repo/matching/keys.rs::rebuild_normalized_keys` to fill and refresh the new
  column, and run that operator action once after deploy.
- `crates/db/src/repo/catalog/sources.rs::upsert_source` and `upsert_sources` write the column
  from Rust.
- `crates/db/src/repo/matching/candidates.rs::find_candidates` and `find_candidates_multi`: a
  third `UNION ALL` branch matching `series_sources` on the compact key with equality, fan-out
  capped by `MAX_KEY_FANOUT` exactly like the alias branch, and reported with a flag so the
  candidate knows it came from a listing title. Add the matched listing titles to
  `tankovault_domain::matching::Candidate` (new field `source_titles: Vec<String>`) and score them
  as primary titles in `best_title_match` (sets `source_title_identity`, not `alias_identity`).
- `crates/db/src/repo/matching/pairs.rs::find_duplicate_pairs`: a fourth blocking branch on the
  same key, and `series_match_facts` loads `source_titles` so the sweep sees them too.

These are new/changed `query!` invocations: run `cargo run -p xtask -- sqlx-prepare` against a
migrated database, and the Docker suite `repo_query_plans` (its EXPLAIN cost ceiling covers
`find_candidates`) plus `repo_matching` and `repo_ingest`.

**3b. Do not file aliases on a weak attach.** `crates/db/src/repo/catalog/ingest.rs::try_ingest`
calls `add_series_titles` for every attach. Make `resolve_canonical_series` return the basis it
attached on (extend `Decision::Attach` to carry the `MatchSignals`, update the sync resolver's
match accordingly) and file the source's alternative titles only when the series was created by
this ingest or attached on primary identity. Also add `series_titles.origin text NOT NULL DEFAULT
'adapter'` (`'adapter' | 'anilist' | 'merge'`) in the same migration and set it on every write
path (`add_series_titles`, `apply_enrichment`, `merge_series`), so future cleanup can target a
provenance.

**3c. Mirror groups (conditional).** 3a already covers mirrors, because mirrors list identical
titles. Only build this if, after 3a ships, query A11 still shows new-series creation from mirror
providers. If needed: `providers.config.mirror_group` (string) plus a per-adapter-family
`path_id` extractor in `crates/adapters`; a new source whose extracted id matches a source in the
same group attaches to that source's series without scoring. Verify first that ids actually
agree across the family with query A12 — do not assume.

**Tests.** `crates/db/tests/repo_ingest.rs`: a stub listing under an AniList-renamed series
attaches via another source's listing title (name the `legend of star general` churn: 37 merges
from one key); an alias-only attach does not file the incoming source's aliases.

**Gates.** `cargo check -p tankovault-db -p tankovault-domain -p tankovault-worker -p
tankovault-sync`; `sqlx-prepare`; `cargo test -p tankovault-db --features integration --test
repo_ingest --test repo_matching --test repo_query_plans`.

**Acceptance.** Query A11 run one week after deploy: new series created from mirror providers
that the sweep later merges drops by > 80% versus the week before.

### WP4 — AniList enrichment that can be trusted

**Files.** `services/sync/src/providers/anilist/graphql.rs` (`METADATA_QUERY`),
`services/sync/src/providers/anilist/parse.rs`, `services/sync/src/engine/metadata.rs`,
`services/sync/src/engine/enrich.rs`, `crates/db/src/repo/matching/pairs.rs`
(`SeriesMatchFacts`).

**Change.**
1. `METADATA_QUERY`: add `format_in: [MANGA, ONE_SHOT]` to the `Media` selection used for title
   search. Keep `format` in the response and reject `NOVEL` defensively in `parse.rs` too.
2. Verify the hit before `apply`: score the returned titles (romaji, english, native, synonyms)
   against the series' canonical title and its sources' listing titles with the matcher; require
   `exact_title`, `compact_identity` or `source_title_identity` on at least one pair. Otherwise
   stamp `mark_checked` and write nothing. Lookups by cached id (`fetch_public_metadata_by_id`)
   are unaffected.
3. One-off operator re-verification of existing mappings (an admin action next to the existing
   enrichment controls, or a `services/sync` job): re-fetch by id, apply rule 2, delete mappings
   that fail. Report the count; expect the light-novel mappings (consecutive-id pairs) among them.
4. `SeriesMatchFacts` gains `anilist_id: Option<String>` (from `sync_mappings`, provider
   `anilist`); `Candidate` gains the same; `assess` sets `external_id_agreement` /
   `external_id_conflict` when both are present.

A new admin endpoint for step 3 must get a row in `services/api/tests/admin_access_matrix.rs`
and an `openapi.json` regeneration (`cargo run -p xtask -- openapi`), per `CLAUDE.md`.

**Tests.** A parse test that a `NOVEL` result is rejected; a metadata-apply test that a search hit
whose titles do not match the series writes nothing (name the light-novel/manga consecutive-id
case, e.g. 150193/150194).

**Gates.** `cargo check -p tankovault-sync -p tankovault-db -p tankovault-matcher`; `cargo test
-p tankovault-sync`; Docker `cargo test -p tankovault-sync --features integration` if any
repository query changed.

### WP5 — Live chapter timing as corroboration and veto

**Why.** For chapters released after both sources were already being scanned, the same work shows
up on both within hours (prod median 4.2 h); different works do not. It needs no new fetching.
It only applies to ongoing works and to chapters seen since scanning started, so absence of the
signal means nothing.

**Change.**
- `crates/db/src/repo/matching/` new `timing.rs`: `live_timing(exec, left: SeriesId, right:
  SeriesId) -> LiveTiming { shared_live_chapters: i64, best_pair_median_gap_hours: Option<f64> }`,
  implementing the per-source-pair logic of query A9: chapters whose `discovered_at` on both
  sources is more than one day after the later of the two sources' first chapter discovery,
  joined on whole chapter number, best (smallest median gap) source pair with ≥ 3 shared live
  chapters. Indexes: `chapters_source_disc_access_idx (series_source_id, discovered_at DESC)`
  already exists; confirm the plan in `repo_query_plans`.
- `services/control-plane/src/dedupe.rs::judge`: call it for pairs whose best reading is an alias
  hit or a review candidate (not for every pair — it is the expensive part), map ≤ 72 h to
  `live_timing_agreement`, > 14 days to `live_timing_conflict`, store the numbers in `evidence`.
- Do not use `chapters.published_at` anywhere in matching: prod showed a mean 1,621-day gap on
  pairs that are the same work, because mirrors stamp their own upload date.

**Tests.** `crates/db/tests/repo_matching.rs`: two sources of one work with chapters discovered
within hours → agreement; two works with overlapping chapter numbers 1–100 discovered weeks apart
→ conflict; bulk-imported back catalogue only → no signal.

**Gates.** `cargo check -p tankovault-db -p tankovault-control-plane`; `sqlx-prepare`; Docker
`repo_matching`, `repo_query_plans`.

### WP6 — Orphan cleanup and the registration race

1. **Twin rule in the sweep.** A series with zero sources whose normalized title equals a sourced
   series' title, and which shares its AniList id when both have one, merges into that series
   without the `MAX_KEY_FANOUT` cap. Implement as a dedicated shortlist in `pairs.rs` (not by
   lifting the cap globally — the cap is what kept 24.7k copies from collapsing on junk keys).
   Expected: ~22,700 merges; run it from the console's whole-catalogue sweep, not the scheduled
   one, so the 200-per-run ceiling does not apply.
2. **Delete the rest.** Remaining zero-source series with no watchlist entry, progress, sync
   mapping or sync state (query A3 showed 3 watched orphans in total) are deleted with
   `crates/db/src/repo/catalog/maintenance.rs::delete_series`.
3. **Fix the registration race.** `crates/db/src/repo/catalog/sources.rs::register_chunk`
   creates series for "fresh" paths, then `upsert_sources` keeps the existing `series_id` on
   conflict without comparing, leaking the created series when a concurrent scan registered the
   path first. Return `(source_path, series_id)` from the upsert, and in the same transaction
   delete each series this chunk created whose source was filed elsewhere. Regression test in
   `repo_ingest.rs` with two concurrent registrations of one path.

**Gates.** `cargo check -p tankovault-db -p tankovault-control-plane`; `sqlx-prepare`; Docker
`repo_ingest`, `repo_matching`.

### WP7 — Merge journal hygiene

`dedupe.rs` writes a `merge_decisions` row with outcome `deferred` every time the per-run merge
ceiling defers a pair, so the same pair is journalled on every run (1,800,193 rows). Change it to
journal a deferral only when the pair has no deferred row newer than its last scoring, and add a
retention delete for `outcome = 'deferred'` rows older than 14 days (a maintenance task next to
the existing purge). Keep `merged`, `queued`, `distinct` rows indefinitely — they are the undo
journals and the replay corpus.

### WP8 — Targeted undo of existing wrong merges

**Order.** After WP1–WP2 are deployed (otherwise the next sweep re-merges what you revert).

**8a. Make chained reverts safe first.** `revert_merge` re-points `moved_sources` by id (correct
even after later merges) but deletes `inserted_titles`, tags, authors and other union rows keyed
on the *original* survivor id. When that survivor was itself absorbed later, those rows now live
on the newer survivor and are not removed, so the shared names that caused the merge survive the
revert. Before any bulk revert:
- add a test in `crates/db/tests/repo_matching.rs`: merge A→B, then B→C, then revert A→B; assert
  A's sources and the names A brought are off C;
- fix by resolving the current survivor through `series_merges` (`resolve_merged_series`) and
  applying the union-row deletes there, or refuse with `DbError::Conflict` and require reverting
  the later merge first. Document which in the function's `# Errors`.

**8b. Batch revert action.** An admin endpoint taking a list of decision ids plus a dry-run flag,
looping over the existing `revert_merge_decision` (which already reverts, suppresses the pair and
disentangles). Newest decision first within a survivor. Needs an `admin_access_matrix.rs` row and
`openapi.json` regeneration.

**8c. Selection.** In this order, each list reviewed by a human before the batch runs:
1. The contaminated hubs in §2: query A13 lists every merge into them; keep the merges whose
   absorbed title is the hub's own work, revert the rest, newest first.
2. Query A14: alias merges through a junk or too-short key, excluding `live_agree`.
3. `live_contradict` (11) and `live_unclear` (297) from query A9, by hand.
4. Franchise-key merges (`sao`, `fairy tail`, `tokyo ghoul`, `douluo dalu`, `soul land`,
   `kaiji`, `rezero`, `my hero academia`, `boruto`, `steins`, `xxxholic`): from query A14 with the
   key list swapped in.
5. After WP0 exists: every sourced merge the new matcher replays as `Review`/`Distinct` and that
   is not in `confirmed_same`. Expect low thousands; review a sample of 50 before batching.

**Acceptance.** Re-run query A1 (title disagreement within a series) and A6 (hubs): the listed
hubs no longer hold other works.

---

## 5. Cross-cutting rules for this work

- Every matching change must be replayed with WP0 before it ships; the PR description quotes the
  verdict-change table.
- `MatchSignals::labels` and `Adjudication::reason` slugs are persisted and rendered: append,
  never rename or reorder.
- The `scanning.auto_merge` feature flag and `TANKOVAULT_SCHEDULER__MERGE_SWEEP_MAX_AUTO_MERGES`
  stay as they are. During the rollout of WP1–WP3, consider pausing the scheduled sweep with the
  flag so the new rules are not mixed with a backlog of old-rule `deferred` pairs; re-enable after
  WP7.
- New `query!` → `sqlx-prepare` + the Docker suites that cover it (`repo_query_plans` for any
  candidate/pair query). New endpoint → access-matrix row + `openapi.json`. Config surface change
  → `docs/CONFIGURATION.md` + `just regenerate`. New dependency → `Cargo.lock` moved →
  `cargo run -p xtask -- notices`.
- Commit messages: Conventional Commits with the owning scope (`matcher`, `db`, `sync`,
  `control-plane`, `worker`). WP1 changes automatic-merge behaviour operators rely on; describe
  that in the commit body, but it is not a breaking API change.

---

## 6. Sequencing and PR slicing

| PR | Packages | Depends on | Size |
|---|---|---|---|
| 1 | WP0 replay harness + fixtures | — | 2–3 days |
| 2 | WP1 + WP2 (matcher only) | 1 | 3–4 days |
| 3 | WP7 journal hygiene | — | 1 day |
| 4 | WP6 orphan twin rule + registration race | — | 2 days |
| 5 | WP3a + WP3b (migration, ingest, candidates) | 2 | 3–4 days |
| 6 | WP4 AniList | 2 | 2–3 days |
| 7 | WP5 live timing | 2 | 3–4 days |
| 8 | WP8a chained-revert fix + 8b batch endpoint | — | 2–3 days |
| — | WP8c undo campaign (operational, not code) | 2, 5, 8 deployed | 2–3 days |
| 9 | WP3c mirror groups | only if A11 says so | 2–3 days |

PRs 3, 4 and 8 are independent of the matcher work and can go in parallel.

---

## 7. Open questions for the owner

1. Pause the scheduled sweep (feature flag) during the WP1–WP3 rollout? Recommended yes.
2. Deprecate the `author/year/type` guard tunables after WP1, or keep them as inert switches?
3. The `webtoons` provider has 2,221 sources but 1,095 distinct titles. Investigate separately
   (language variants or duplicate paths) before WP3c.
4. Many sources are stale (kaliscan 7,146 of 7,881 not scanned in 14 days; mangadex 4,986 of
   6,343). Not a matching problem, but it starves WP5 of live data and likely feeds the ~8k series
   with sources but no chapters.

---

## 8. Rejected alternatives

- **Fixing the metadata guards** (name-order-insensitive authors, AniList-only years, splitting
  format from origin). Rejected: the owner reports author, year and type are unreliable most of
  the time; better comparison of bad data still yields bad decisions.
- **Chapter-number overlap as a signal.** Prod query showed containment ≈ 1.00 in every class:
  unrelated works with similar chapter counts overlap fully. Only timing discriminates.
- **Provider `published_at` timing.** Mean gap 1,621 days on same-work pairs; mirrors stamp their
  own upload date.
- **Cover image perceptual hashing.** About 1–1.5 weeks, 10–35 GB one-off; useful as a
  title-independent blocking key. Deferred by the owner on cost; revisit if the review queue stays
  large after this plan. The fetcher returns text only (`FetchResponse.body: String`), so binary
  fetching is new work either way.
- **Chapter page image comparison.** 4–6 weeks for on-demand sampling of the ~10 adapter
  families, 80–240 GB for the current review set, webtoon slice alignment, copyright exposure from
  downloading page images, and ban risk on scraping sites. Rejected.

---

## 9. Implementation notes

Decisions taken while implementing, where the plan left a choice open or its letter would have
done harm. Each is in the code with its reason.

**WP0.**
- The pair reading lives in `crates/matcher/src/pair.rs` (`read_pair`, `adjudicate_pair`,
  `PairSide`); the sweep calls it. The JSONL reader is `tankovault_matcher::replay`, behind the
  matcher's `replay` feature so no service links it.
- `tests/fixtures/*.jsonl` are hand-built stand-ins in the A10 shape; the real exports have not
  been taken. `crates/matcher/tests/fixtures/README.md` says how to replace them. The ≥ 99 %
  reproduction check against a production export is still to be run, on the WP0 commit.

**WP1.**
- `decide` still requires `score >= high` for an attach, so `high` keeps a meaning (above 1.0 it
  switches automatic attach off). A primary identity scores 1.0, so this changes nothing at the
  defaults.
- `decide`: a numeric conflict is `Create`; any other veto is `Ambiguous` at or above `low` and
  `Create` below it, so a veto never queues a pair the score would not.
- `best_assessment` ranks a primary identity above a higher-scoring fuzzy candidate, because a
  reordered title reaches the same clamped 1.0 as an identical one and only the identical one may
  attach.
- `adjudicate`: `below_review_floor` is checked before the two review vetoes. A corroborated alias
  merges with no score bar (the alias discount caps it at 0.80 by design) under the new reason
  slug `corroborated_alias`; enabled guards still hold it.
- `read_pair` ranks primary identity, corroborated alias, **plain alias**, then score. The plain
  alias tier keeps the bug pinned by `an_alias_identity_is_not_lost_to_the_trigram_it_produced`
  fixed: without it a canonical reading lifted to 1.0 by the alias's own trigram outranks the
  alias reading and the row loses its alias badge.
- `services/sync/src/engine/resolve.rs` is unchanged: it still maps at `score >= high`. With
  metadata out of the score, fewer tracker entries reach `high` on a fuzzy title.

**WP2.**
- Roman numerals are `ii`–`ix`; `i`, `v` and `x` are left out (pronoun, `versus`, the `×` of
  `Spy × Family`). Spelled ordinals are `second`–`tenth`; `first` is left out.
- Numbers and edition markers are compared against the matched title and the candidate's canonical
  title, and — in the sweep — between the two canonical titles. A cross-language pair whose one
  canonical title carries a number (`The Second Coming of Gluttony` against its romanised title)
  therefore reads as distinct. Accepted as the conservative failure.
- `alias_is_distinctive` lives in `tankovault_domain::normalize`, because `crates/db` applies it at
  write time and cannot link the matcher. Its Latin length floors would refuse most native CJK
  titles, so a key in a non-Latin script needs three characters instead.
- The franchise rule applies to aliases of at most four words. A longer alias contained in a
  longer title is the short form of one long light-novel title, which is the same work.
- Write time applies a narrower predicate than match time. `add_series_titles` drops only
  placeholder text (`alias_is_placeholder`: the deny-list, and empty keys); the length rules apply
  only when matching (`alias_is_distinctive`). `series_titles` also feeds catalogue search, and
  the length rules would have dropped real short titles from it — `Kureimoa` for *Claymore*, which
  `repo_tracking::the_search_matches_canonical_titles_alt_titles_tags_and_authors` pins.

---

## Appendix A — Production evidence

Queries as run on 2026-10-19 with their results. All are read-only.

**A1. Series whose sources disagree on the title** (top 200 inspected; ~70 were different works:
Higurashi arcs, Kaiji parts, Kindaichi series, Marvel tie-in months, `Anime –` entries).

```sql
SELECT s.id, s.canonical_title, count(*) AS sources,
       count(DISTINCT replace(lower(ss.provider_title), ' ', '')) AS distinct_titles,
       array_agg(DISTINCT ss.provider_title) AS titles
FROM series s JOIN series_sources ss ON ss.series_id = s.id
GROUP BY s.id HAVING count(*) >= 3
ORDER BY count(DISTINCT replace(lower(ss.provider_title), ' ', ''))::float / count(*) DESC, count(*) DESC
LIMIT 200;
```

**A2. AniList ids shared by several series.** 545 ids over 24,714 series; sizes 2 (×25), 3 (×1),
then 17–256 (×520). Every large cluster is one title copied N times.

```sql
SELECT provider, external_id, count(*) AS series
FROM sync_mappings GROUP BY 1, 2 HAVING count(*) > 1 ORDER BY 3 DESC;
```

**A3. Orphans by creation week.** 199 / 13,161 / 10,140 in the weeks of 09-14 / 09-21 / 09-28;
3 watched; ~99% mapped.

```sql
SELECT date_trunc('week', s.created_at) AS week, count(*) AS orphans,
       count(*) FILTER (WHERE EXISTS (SELECT 1 FROM watchlist_entries w WHERE w.series_id = s.id)) AS watched,
       count(*) FILTER (WHERE EXISTS (SELECT 1 FROM sync_mappings m WHERE m.series_id = s.id)) AS mapped
FROM series s
WHERE NOT EXISTS (SELECT 1 FROM series_sources ss WHERE ss.series_id = s.id)
GROUP BY 1 ORDER BY 1;
```

**A4. Orphans with a sourced twin.** 22,696 of 23,500.

```sql
SELECT count(*) AS orphans,
       count(*) FILTER (WHERE EXISTS (
         SELECT 1 FROM series s2 JOIN series_sources ss ON ss.series_id = s2.id
         WHERE s2.normalized_title = o.normalized_title AND s2.id <> o.id)) AS has_sourced_twin
FROM series o WHERE NOT EXISTS (SELECT 1 FROM series_sources ss WHERE ss.series_id = o.id);
```

**A5. Merges by absorbed-side sources.** No sources: 387,033 (307,320 exact, 79,450 alias).
With sources: 93,178 (50,151 exact, 42,297 alias).

```sql
SELECT (evidence->'survivor_choice'->'absorbed_weight'->>'sources')::int > 0 AS absorbed_had_sources,
       count(*) FILTER (WHERE 'exact_title' = ANY(signals))    AS exact,
       count(*) FILTER (WHERE 'alias_identity' = ANY(signals)) AS alias,
       count(*) AS merged
FROM merge_decisions WHERE outcome = 'merged' GROUP BY 1;
```

**A6. Hubs.**

```sql
SELECT survivor_id, count(*) AS merges, count(DISTINCT absorbed_id) AS absorbed,
       (array_agg(DISTINCT right_title))[1:6] AS sample
FROM merge_decisions
WHERE outcome = 'merged' AND reverted_at IS NULL
  AND (evidence->'survivor_choice'->'absorbed_weight'->>'sources')::int > 0
GROUP BY 1 ORDER BY 2 DESC LIMIT 50;
```

**A7. Policy in force per merge.** Guards off (numeric only): 442,900; all on: 36,861; year off:
450.

```sql
SELECT policy->'guards' AS guards, trigger, count(*)
FROM merge_decisions WHERE outcome = 'merged' GROUP BY 1, 2 ORDER BY 3 DESC;
```

**A8. Sourced merges where a disabled guard fired.** 93,177 / 13,295 / 12,356 via alias.

```sql
SELECT count(*) AS merges,
       count(*) FILTER (WHERE signals && ARRAY['type_conflict','year_conflict','author_conflict']) AS would_have_been_held,
       count(*) FILTER (WHERE signals && ARRAY['type_conflict','year_conflict','author_conflict']
                         AND 'alias_identity' = ANY(signals)) AS held_and_via_alias
FROM merge_decisions
WHERE outcome = 'merged' AND reverted_at IS NULL
  AND (evidence->'survivor_choice'->'absorbed_weight'->>'sources')::int > 0;
```

**A9. Live chapter timing per alias merge.** `no_live_signal` 38,521; `live_agree` 3,465 (4.8
chapters, 4.2 h); `live_unclear` 297 (151 h); `live_contradict` 11 (372 h). Heavy: run on a
replica. Prefix with `CREATE TEMP TABLE live_cls AS` and stop at `cls` to reuse in A14.

```sql
WITH d AS (
  SELECT id, survivor_id,
         ARRAY(SELECT jsonb_array_elements_text(undo->'moved_sources')::uuid) AS moved
  FROM merge_decisions
  WHERE outcome = 'merged' AND reverted_at IS NULL
    AND 'alias_identity' = ANY(signals)
    AND (evidence->'survivor_choice'->'absorbed_weight'->>'sources')::int > 0
), src AS (
  SELECT d.id, 'a' AS side, m AS source_id FROM d, unnest(d.moved) m
  UNION ALL
  SELECT d.id, 's', ss.id
  FROM d JOIN series_sources ss ON ss.series_id = d.survivor_id AND NOT (ss.id = ANY(d.moved))
), ch AS (
  SELECT src.id, src.side, src.source_id, c.number_milli / 10000 AS n, c.discovered_at,
         min(c.discovered_at) OVER (PARTITION BY src.source_id) AS first_seen
  FROM src JOIN chapters c ON c.series_source_id = src.source_id
), pair AS (
  SELECT a.id, a.source_id AS sa, s.source_id AS ss,
         count(*) AS live_n,
         percentile_cont(0.5) WITHIN GROUP (
           ORDER BY abs(extract(epoch FROM a.discovered_at - s.discovered_at)) / 3600) AS gap_h
  FROM ch a
  JOIN ch s ON s.id = a.id AND s.side = 's' AND s.n = a.n
  WHERE a.side = 'a'
    AND a.discovered_at > greatest(a.first_seen, s.first_seen) + interval '1 day'
    AND s.discovered_at > greatest(a.first_seen, s.first_seen) + interval '1 day'
  GROUP BY 1, 2, 3
), best AS (
  SELECT DISTINCT ON (id) id, live_n, gap_h FROM pair WHERE live_n >= 3 ORDER BY id, gap_h
), cls AS (
  SELECT d.id,
         CASE
           WHEN b.id IS NULL THEN 'no_live_signal'
           WHEN b.gap_h <= 72 THEN 'live_agree'
           WHEN b.gap_h > 24 * 14 THEN 'live_contradict'
           ELSE 'live_unclear'
         END AS verdict,
         b.live_n, b.gap_h
  FROM d LEFT JOIN best b ON b.id = d.id
)
SELECT verdict, count(*), round(avg(live_n)::numeric, 1) AS avg_live_chapters,
       round(avg(gap_h)::numeric, 1) AS avg_gap_hours
FROM cls GROUP BY 1 ORDER BY 2 DESC;
```

**A10. Replay export for WP0.** One JSON object per line.

```sql
COPY (
  SELECT jsonb_build_object(
           'id', id, 'trigger', trigger, 'verdict', verdict, 'reason', reason,
           'outcome', outcome, 'score', score, 'signals', signals,
           'evidence', evidence, 'policy', policy)
  FROM merge_decisions
  WHERE outcome IN ('merged', 'queued', 'distinct')
) TO STDOUT;
```

**A11. Mirror churn: new series from a provider that the sweep later absorbed** (run weekly
around WP3).

```sql
SELECT p.slug, date_trunc('week', md.decided_at) AS week, count(*) AS absorbed_new_series
FROM merge_decisions md
JOIN series_sources ss ON ss.id = ANY(ARRAY(SELECT jsonb_array_elements_text(md.undo->'moved_sources')::uuid))
JOIN providers p ON p.id = ss.provider_id
WHERE md.outcome = 'merged' AND md.decided_at > now() - interval '8 weeks'
GROUP BY 1, 2 ORDER BY 2 DESC, 3 DESC;
```

**A12. Do mirror families share path ids?** Replace the slugs; compare the trailing path segment.

```sql
SELECT regexp_replace(ss.source_path, '^.*/([^/]+)/?$', '\1') AS path_id,
       array_agg(DISTINCT p.slug) AS providers, count(DISTINCT ss.series_id) AS series
FROM series_sources ss JOIN providers p ON p.id = ss.provider_id
WHERE p.slug IN ('mangakakalot', 'mangakakalove', 'mangabats', 'manganatogg', 'nelomanga', 'natomanga')
GROUP BY 1 HAVING count(DISTINCT p.slug) > 1
ORDER BY 3 DESC LIMIT 100;
```

**A13. Every merge into a hub, newest first.**

```sql
SELECT id, decided_at, left_title, right_title,
       evidence->>'matched_candidate_title' AS via, signals
FROM merge_decisions
WHERE outcome = 'merged' AND reverted_at IS NULL AND survivor_id = $1
ORDER BY decided_at DESC;
```

**A14. Alias merges through junk or too-short keys, excluding confirmed-correct ones.** Needs
`live_cls` from A9.

```sql
WITH m AS (
  SELECT md.id, md.survivor_id, md.decided_at, md.left_title, md.right_title,
         md.evidence->>'matched_candidate_title' AS via
  FROM merge_decisions md
  WHERE md.outcome = 'merged' AND md.reverted_at IS NULL
    AND 'alias_identity' = ANY(md.signals)
    AND (md.evidence->'survivor_choice'->'absorbed_weight'->>'sources')::int > 0
)
SELECT m.*, l.verdict
FROM m LEFT JOIN live_cls l USING (id)
WHERE coalesce(l.verdict, '') <> 'live_agree'
  AND (l.verdict IN ('live_contradict', 'live_unclear')
       OR m.via IN ('unknown', 'status', 'none', 'untitled', 'no alternative titles',
                    'sorry', 'please', 'delete', 'season 2', 'manhwa', 'fantasy')
       OR length(replace(m.via, ' ', '')) < 6
       OR (m.via !~ ' ' AND length(m.via) < 10))
ORDER BY m.survivor_id, m.decided_at DESC;
```

---

## Appendix B — Code map

| Concern | Location |
|---|---|
| Scoring terms and signals | `crates/matcher/src/assess.rs` (`assess_with`, `ALIAS_EVIDENCE_WEIGHT`) |
| Title comparison | `crates/matcher/src/title.rs` (`best_title_match`) |
| Numeric signature, edit ratio, token set | `crates/matcher/src/similarity.rs` |
| Ingest decision, sweep verdict | `crates/matcher/src/decide.rs` (`decide`, `adjudicate`) |
| Thresholds, guards | `crates/matcher/src/types.rs`; config `crates/config/src/matching.rs`; runtime tunables `crates/domain/src/tunables/mod.rs`, `services/control-plane/src/policy.rs` |
| Signal vocabulary | `crates/domain/src/matching.rs` (`MatchSignals`, `Decision`, `MergeVerdict`, `Candidate`) |
| Title normalization | `crates/domain/src/normalize.rs` |
| Candidate retrieval | `crates/db/src/repo/matching/candidates.rs` (`MAX_KEY_FANOUT` in `mod.rs`) |
| Duplicate shortlist, pair facts | `crates/db/src/repo/matching/pairs.rs` |
| Merge, undo, disentangle, journal | `crates/db/src/repo/matching/{merge,undo,disentangle,decisions}.rs` |
| Sweep | `services/control-plane/src/dedupe.rs` (`best_reading`, `judge`, `evidence_json`) |
| Ingest attach and alias filing | `crates/db/src/repo/catalog/{ingest,series,sources}.rs`, `enrichment.rs::add_series_titles` |
| Catalogue registration from scans | `services/worker/src/engine/{scans,tasks}.rs` → `register_source_stubs` |
| AniList enrichment | `services/sync/src/engine/{enrich,metadata}.rs`, `services/sync/src/providers/anilist/{graphql,parse}.rs` |
| Tracker matching (shares the matcher) | `services/sync/src/engine/resolve.rs` |
| Tests | `crates/matcher/src/tests.rs`, `crates/matcher/tests/prop_scoring.rs`, `crates/db/tests/repo_{matching,ingest,query_plans}.rs` |
