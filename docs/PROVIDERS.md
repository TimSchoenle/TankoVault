# Providers — config vs. custom code

TankoVault onboards a source site in one of two ways (design §7):

1. **Config only** — the site runs a layout TankoVault already has a parser for (today: the
   Madara WordPress theme). Onboarding is **data**: one `providers` row whose `config`
   JSON carries only the selector overrides where the site deviates from the theme
   defaults ([`madara_default_config`](../crates/adapters/src/madara.rs)). No Rust code.
2. **Custom adapter** — the site has a bespoke layout, or needs per-field transforms that
   selectors can't express (splitting a synopsis out of boilerplate, reading label/value
   rows). Onboarding is **code**: a small struct implementing `SourceAdapter`, dispatched
   by slug in [`factory.rs`](../crates/adapters/src/factory.rs).

The shipped presets live in [`presets.rs`](../crates/adapters/src/presets.rs)
(`tankovault_adapters::builtin_presets()`), installed by `bootstrap seed-providers` (or
`xtask seed` from a checkout). Every selector below was
derived from **live markup fetched through the solver pipeline** and is pinned by fixture
tests, so a provider layout change fails a test rather than corrupting production data.

A third onboarding route was added with the 2026-08 source expansion: a **family**. Madara was
always this in spirit — one theme, many sites, one shared selector set — and it is now spelled
that way in `AdapterKind`, alongside two more (`mangathemesia`, `manganato`). A site on a family
theme is a config row carrying only its deviations, exactly like a Madara one.

## Summary

| Provider | Domain | Layout | Onboarding | Adapter |
|---|---|---|---|---|
| Demonic Scans | `demonicscans.org` | Bespoke PHP | **Custom code** | `DemonicScansAdapter` |
| Manhuaus | `manhuaus.com` | Madara | **Config only** | Madara + overrides |
| KunManga | `www.kunmanga.co.uk` | Madara-ish hybrid | **Custom code** | `KunMangaAdapter` |
| Toonily | `toonily.com` | Madara | **Config only** | Madara + overrides |
| MangaRead | `www.mangaread.org` | Madara | **Config only** | Madara + overrides |
| Manhua Plus | `manhuaplus.com` | Madara | **Config only** | Madara + overrides |
| Rizz Fables | `rizzfables.com` | MangaThemesia | **Config only** | family defaults |
| NatoManga | `www.natomanga.com` | Manganato | **Config + code** | `ManganatoAdapter` |
| Mangakakalot | `www.mangakakalot.gg` | Manganato | **Config + code** | `ManganatoAdapter` |
| NeloManga | `www.nelomanga.net` | Manganato | **Config + code** | `ManganatoAdapter` |
| TCB Scans | `tcbonepiecechapters.com` | Bespoke | **Config only** | generic selectors |
| Weeb Central | `weebcentral.com` | Bespoke (htmx) | **Config only** | generic selectors |
| MangaPill | `mangapill.com` | Bespoke | **Config only** | generic, sitemap mode |
| MangaDex | `mangadex.org` | JSON API | **Custom code** | `MangaDexAdapter` |
| ComicK | `comick.dev` | JSON API | **Custom code** | `ComickAdapter` |
| Omega Scans | `omegascans.org` | HeanCMS JSON | **Custom code** | `HeanCmsAdapter` |
| Asura Scans | `asurascans.com` | Astro islands | **Custom code** | `AstroIslandAdapter` |
| Hive Toons | `hivetoons.org` | Astro islands | **Custom code** | `AstroIslandAdapter` |
| Flame Comics | `flamecomics.xyz` | Next.js | **Custom code** | `FlameComicsAdapter` |
| WEBTOON | `www.webtoons.com` | Licensed, bespoke | **Custom code** | `WebtoonsAdapter` |

## Early-access (paid) chapters

Several of these sites sell chapters before releasing them free. Such a chapter is stored like
any other — it has to be, or the row is re-discovered and re-dated when the timer expires, and
`discovered_at` is what the release feed orders by — but it is stored **as what it is**:
`chapters.access` is `early_access` and `chapters.unlocks_at` carries the stated unlock time.

Where each provider publishes the fact:

| Provider | Signal | Unlock time |
|---|---|---|
| Omega Scans | `price > 0` in the chapter JSON | `free_at` |
| Asura Scans | `is_locked` in the Astro island props | `unlock_time` / `early_access_until` |
| Hive Toons | `isLocked` / `isTimeLocked` in the series island | `unlockAt`, on the **chapter** page — fetched per locked chapter, because the listing omits it and the window is per-chapter configurable |
| Toonily and other Madara sites | `chapters.locked` selector in the provider config | `chapters.unlock`, where rendered |
| WEBTOON | none: Fast Pass episodes are not rendered to an anonymous visitor at all | n/a |

A locked chapter does **not** count as unread. It starts counting when either its stated unlock
time passes — no rescan needed, the predicate compares the stored timestamp against `now()` — or
the reader opts that provider in at `PUT /v1/me/source-preferences`
(`early_access_provider_ids`). The opt-in is per provider because paying one scanlator says
nothing about any other. A locked chapter with **no** announced unlock time stays locked: a
missing date is not a date in the past.

`crates/db/tests/repo_tracking.rs` pins all of that, including the two ways it opens and the
per-provider scoping of the opt-in.

**manhuaus** is **just a config of the existing Madara parser**. **kunmanga** renders
Madara-shaped series HTML but serves chapters from a JSON API and cannot be enumerated
through its own listing, so it wraps the generic parser in a small custom adapter.
**demonicscans** required a full parser of its own.

## Manhuaus — Madara config only

Standard Madara; only three deviations from the defaults:

| Field | Default | Override | Why |
|---|---|---|---|
| `catalog.path` | `/manga/?page={page}` | `/manga/page/{page}/` | Site paginates on the path, not a query. |
| `catalog.next` | `a.nextpostslink` | `null` (cleared) | The theme has no next-page marker at all — it paginates through an AJAX "LOAD MORE" button that is rendered on the last page too, and emits no `<head>` rel=next. Cleared, so `has_next` falls back to "this page yielded items"; past the last page WordPress answers `200` with an `error404` shell and zero items, which ends the walk exactly. |
| `series.cover` | `div.summary_image img@src` | `div.summary_image img@data-src` | Covers are lazy-loaded — `src` is a placeholder, the real URL is in `data-src`. |

Everything else (catalog item `div.page-item-detail`, title `h3 a`, series title/desc/
status/tags, chapters `li.wp-manga-chapter`) is the Madara default.

### Why `series.alt` is not a selector

The Madara default for alternative titles is a **labelled row**, not a CSS selector:

```json
"alt": { "row": "div.post-content_item", "label": "div.summary-heading h5",
         "match": "Alternative", "value": "div.summary-content" }
```

The theme renders Alternative, Author(s), Artist(s) and Genre(s) as structurally identical
`div.post-content_item` rows whose only distinguishing feature is the heading text, and CSS
cannot select on text. The default was the plain selector `div.summary-heading`, which matched
every row's **label** — so every series scanned from manhuaus or kunmanga was stored with
"Alternative", "Author(s)", "Genre(s)" and "Status" as alternative titles. Those rows go into
`series_titles`, whose `normalized` column the trigram matcher and the catalogue search both
score against, so the effect reached matching and search rather than stopping at display.

## KunManga — custom adapter over the generic parser

`www.kunmanga.co.uk` is Madara-shaped in its markup but deviates in two ways a selector
cannot express, so it ships [`KunMangaAdapter`](../crates/adapters/src/kunmanga.rs), which
delegates latest/series parsing to the generic config adapter and overrides the rest. A third
deviation *is* expressible as selectors and stays in the preset config — see
[the latest feed](#the-latest-feed-is-not-madara-shaped) below.

**1. Chapters come from a JSON API, not the HTML.** The `div.wp-manga-chapter` rows are
rendered client-side; a non-JS fetch of a series page yields zero chapters. `fetch_chapters`
pages `/api/comics/{slug}/chapters?page={n}&per_page=100&order=asc` instead, sending the same
`Accept: application/json` / `X-Requested-With` / `Referer` headers the site's own front-end
sends — the endpoint sits behind the same bot management as the pages, and a request shaped
like a document fetch is more likely to be challenged.

The response body is read by [`json::parse_json_body`](../crates/adapters/src/json.rs), which
accepts every shape the endpoint can arrive in and distinguishes the ways it can fail:

| Body | Outcome |
|---|---|
| Raw JSON | parsed directly |
| Solver-rendered: JSON in a `<pre>` block, entity-escaped or not | payload extracted, then parsed |
| Solver-rendered: JSON pretty-printed into per-token elements by a browser JSON viewer | text content stripped of markup, then parsed |
| `{"success": false, "message": …}` with no `data` | `Missing`, quoting the API's own message |
| A Cloudflare interstitial the solver could not clear | `Challenged` — retryable, **not** a parse error |
| A rate-limit notice that arrived labelled `200` | `Throttled` — retryable, **not** a parse error |
| Anything else | `Parse`, quoting url, status, content type, size and a bounded body prefix |

That taxonomy is the point: before it, all six collapsed into
`failed to parse provider response: kunmanga chapters API returned no parseable JSON`, which
named neither the cause nor the response. An unsolved challenge is now reported as one and
the scan task is redelivered (see [Failure handling](#failure-handling)) rather than spent.

**2. The catalogue is not enumerable through the site's own listing.** `/manga/page/{n}` is
**clamped server-side at page 100** — requesting page 101, 500 or 20000 all return the
page-100 body, which still reports `Page 100 of ~6900` — while the `a[aria-label="Next"]`
control is rendered unconditionally, so a has-next-driven walk never terminates. Walking the
listing therefore loops forever and can only ever reach ~1.1k of the site's ~88k series.

`list_catalog` walks the **sitemap** instead (advertised in the site's `robots.txt`):

| Step | Document | Content |
|---|---|---|
| index | `/sitemap.xml` | one entry per shard; the series shards are `sitemap-comic-{n}.xml` |
| page `n` | `sitemap-comic-{n}.xml` | ~20k `<loc>` series URLs (~2.9 MB raw) |

One sitemap shard = one `CatalogPage`, and `has_next` comes from the shard count in the
index, so the walk terminates on the site's own data rather than a heuristic. The sitemap
carries no titles, so each entry gets a provisional slug-derived title; because the slug is
itself a normalised form of the real title it collapses to the same matching key, and the
per-series enrichment task then overwrites it with the real one.

Covers use a plain `src` on a separate CDN host (`cdn.zinmanga1.com`), so the default
`img@src` works unchanged. `series.release` is overridden because the year is only reliably
available as a link into the `manga-release` archive.

### The latest feed is not Madara-shaped

The home page renders no `div.page-item-detail`. Inheriting the Madara `latest` defaults
therefore selected nothing, `list_latest` returned an empty feed, and **every fast scan
silently did no work** — an empty feed is a valid answer, so nothing failed and nothing was
logged. Full scans still ran, so the symptom was new chapters arriving late rather than not
at all, which is why it survived until an adapter dry run showed `"latest": {"items": []}`.

The site's updates are the home page's "Manga Updates!" slider, and the preset selects it:

| Field | Value | Why |
|---|---|---|
| `latest.item` | `div.manga-item` | one per slider entry |
| `latest.title` | `img@alt` | the anchor wraps only the cover image, so there is no link text |
| `latest.chapter` | `null` | the slider carries no chapter label |

`null` is deliberate rather than omitted: the config merges onto the Madara defaults, so
leaving the key out keeps the inherited `span.chapter a` — a rule that reads as live but can
never match. The resulting `latest_chapter` of `0.0` costs nothing, because both callers of
`list_latest` (`TaskKind::LatestFeed` and `run_fast_scan_inline`) re-ingest by `path` and
read neither the title nor the chapter number.

Because `seed-providers` is create-only, this preset change reaches **new installations
only**. An existing deployment keeps the config already in its `providers` row until an
operator updates it from the admin console.

> **Note on body size.** A shard is ~2.9 MB as raw XML, comfortably under the fetch stack's
> 8 MB cap. When a fetch has to be solved, the headless browser returns an XML *viewer* page
> that embeds the document twice (~16 MB); that body arrives via the solver path, which the
> cap does not apply to. `sitemap_locs` handles both shapes. Once a solved session is cached
> and replayed, subsequent shard fetches are plain requests returning the small raw XML.

## Demonic Scans — custom adapter

`demonicscans.org` is not Madara, and two fields need transforms a selector can't express,
so it ships [`DemonicScansAdapter`](../crates/adapters/src/demonicscans.rs):

- **Catalogue:** `/advanced.php?list={page}`; items `div.advanced-element`; the series link
  is the item anchor, and its full title is the anchor's `title` attribute (the visible
  `<h1>` is ellipsis-truncated). Next page = an explicit anchor whose text is "Next".
- **Latest feed:** the home page (`/`); items `div.updates-element`; title from `h2 a`;
  newest chapter from the first `a.chplinks`.
- **Series:** title `h1.big-fat-titles`; cover `#manga-page img.border-box@src`; genres
  `div.genres-list li`. **Description** comes from `div.white-font` with the SEO boilerplate
  before the `"The Summary is"` marker stripped. **Status/Alternatives** are read from the
  `#manga-info-stats` label/value rows (`stat_value`); alternatives are then split on `,`/`;`.
- **Chapters:** on the same series page — `#chapters-list li`; link `a.chplinks`; the ISO
  `YYYY-MM-DD` release date (`span`) is parsed to a timestamp.

## Failure handling

Adapter failures are classified, because "the scan failed" is two very different situations.
[`AdapterError::is_transient`](../crates/adapters/src/error.rs) marks the failures a later
attempt can clear — an unsolved challenge, a solver outage, a `429`, a provider `5xx` — and
separates them from the ones that will fail identically on replay (a selector that no longer
matches, a malformed body, a bad config).

The worker acts on that split:

- **Transient** — the task is negatively acked with a delay (1 min, then 5, then 15) and
  redelivered, up to three deliveries. It stays uncounted and unsettled meanwhile, so the run
  does not finalise without it, and the idempotent ingest makes the replay a no-op for
  whatever the first attempt did manage.
- **Permanent** — the task is failed with its error recorded, exactly as before.

### A failure has to be readable from the console

A scan is watched from a log, so a failure answers three questions on one line: what was
being scanned, what the provider did about it, and whether anything will try again.

- **What was scanned.** Every adapter error names the URL it was raised about.
  `Ctx::fetch` attaches it to transport failures (the transport reports "request timed out",
  never *where*), and [`AdapterError::from_response`](../crates/adapters/src/error.rs) attaches
  it — with the status, its reason phrase, the content type, the body size and a bounded,
  whitespace-collapsed prefix of the body — to a non-success status. A bare
  `provider returned HTTP 404` cannot distinguish a stale path from a block page wearing a
  `404`; the quoted body can. The same treatment covers a missing required element, which
  reports the selector that matched nothing alongside the page that lacked it.
- **What happens next.** The worker logs a `next=` field on every failure, saying what it
  actually did: requeued with the delivery number and the delay, given up on after the
  delivery cap, or recorded as failed because a replay would fail identically. The inline
  scans say the same for a skipped series and for a catalogue walk cut short.
- **Which task.** `provider`, `scan`, `task` (spelled as `scan_tasks.kind`), `target`
  (the path, or `catalog page N`), `run_id` and `task_id` are structured fields, so the
  console line and the database row are the same vocabulary.

Body prefixes are capped at 240 characters: one failure is one line, never a page dumped
into the logs.

### Rate limiting is a conversation, not a setting

A provider's real limits are unpublished, and its HTML pages and its JSON API rarely share
one — KunManga serves pages happily at the configured 2 rps/process while
`/api/comics/*/chapters` answers a rendered **429 "Too Many Requests"** well below that. A
fixed budget cannot discover this, so a large scan spends itself re-offering a rate the
provider is already refusing.

Four layers now cooperate on that signal, and all of them depend on the `429` surviving the
trip. It previously did not, in three separate places:

1. **Detection.** `429` and `503` are rate-limit statuses *and* challenge statuses, and this
   origin sits behind Cloudflare — so a Laravel throttle notice matched the
   managed-challenge fallback and bought a solve. `detect_challenge` now recognises a
   rendered rate-limit page and declines it: it is the origin answering, not an interstitial
   in front of it. The plain `429`, with its `Retry-After`, reaches the layers below intact.
2. **Solving.** When a solve does happen, `SolvingFetcher` no longer synthesises `200` — it
   reports the solver's status and headers. But **the page outranks the report**:
   a solver reports `200` for any navigation that completed, throttle notice included, and a
   provider routinely serves one as a rendered page under a success status — so a body whose
   `<title>` is "Too Many Requests" is read as `429` regardless of what the back-end claims.
   (The `render` back-end reports no status at all, and relies on the same rule.)
3. **Backoff.** `BackoffFetcher` waits out the individual request, preferring `Retry-After`.
4. **Pace.** `RateLimitedFetcher` widens its own spacing for **every subsequent request** to
   that provider: +500 ms on the first signal, doubling per signal to a ceiling of 8 s,
   halving after each throttle-free minute. The crawl settles just under whatever the provider
   will serve rather than re-tripping the limit for the rest of the run.

As a backstop, an adapter that is still handed a throttle notice reports
`AdapterError::Throttled` (retryable) rather than a parse failure — a fourth layer repeating
the same misdiagnosis helps nobody. The backstop covers all three ways such a page can
arrive: as a body that will not parse, as a page whose required element is absent, and as a
non-success status whose body is the interstitial itself. That last one also corrects the
verdict — a `403` carrying a challenge page is retryable, where the bare `403` it used to be
reported as was not.

The penalty is per provider, not per path, so an API limit also slows that provider's page
fetches. That is the conservative direction, and simpler than modelling one budget per route.

Two related guarantees sit underneath this:

- A solver that hands back the interstitial instead of the page is treated as an unsolved
  challenge ([`detect_challenge_body`](../crates/solver/src/detection.rs)), not as a 200. It
  used to reach the adapter as content, where the only available verdict was "malformed".
- Chapters the API returns without a usable number are counted and logged per series rather
  than dropped in silence — a provider format change should surface as a warning, not as data
  that quietly stops arriving.

## Adding another provider

- **Madara-like site:** add a `ProviderPreset` (or create the row in the admin console) with
  `adapter = madara` and only the overriding selectors. Add a fixture + test if it deviates
  from the defaults. No code.
- **New layout:** add a `SourceAdapter` struct, register its slug in `factory.rs`, add a
  preset with `adapter = custom`, and add fixture tests. Prefer widening the config schema
  over a custom adapter when the difference is purely selectors.
