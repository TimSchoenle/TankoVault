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
that way in `AdapterKind`, alongside three more (`mangathemesia`, `manganato`, `keyoapp`). A site
on a family theme is a config row carrying only its deviations, exactly like a Madara one.

That is where the leverage is, and the second half of the expansion (2026-08-11, thirty sites)
spent it: twenty-one of the thirty went in as a family row with **no** deviation at all, and the
nine that did not share one adapter between them. Four of those twenty-one have since needed a
`series` override — the last row of the table below. Deriving a family's defaults from one
site is what made the first half fragile — see
[Deriving a family's defaults](#deriving-a-familys-defaults).

## Managed presets

A provider row installed from a preset stays **linked** to it, and while that link is *locked*
the installer rewrites the row's preset-owned fields on every rollout. That is the mechanism by
which a selector fix in this repository reaches a deployment that already carries the provider —
before it existed, `seed-providers` was create-only and a fix reached new installations only.

| Field | Owner while locked | Why |
|---|---|---|
| `name`, `base_url`, `adapter`, `config` | the preset | They describe the *site's* layout, which is what this repository tracks and fixes. |
| `politeness` | always the operator | A crawl budget answers to the operator's infrastructure, robots policy and legal position. A rollout that restored a rate limit somebody had lowered would be a worse bug than a stale selector. |
| `state` (pause/blocklist) | always the operator | Same reason, and it is usually an incident response. |

Three things an operator can do with the link, all from **Console → Providers**:

- **Unlock to edit.** The preset-owned fields become editable and no rollout touches them again.
  The row keeps naming its preset so the console can still offer the reverse.
- **Follow the preset again.** Re-applies the shipped values immediately, discarding local edits
  to those four fields — behind a confirmation, because it is destructive to them. Politeness
  and pause state survive it.
- **Clone.** Opens the ordinary registration form filled in from the provider. A clone is never
  preset-managed: it is the way to run a *second* site on the same theme, and it starts at the
  default crawl budget rather than inheriting one tuned for a different host.

While locked, the API refuses a `PATCH` that would change a preset-owned field (409), so the
read-only inputs in the console are a courtesy rather than the enforcement. A politeness-only
`PATCH` is accepted whether the row is locked or not.

The catalogue itself is data: `bootstrap seed-providers` mirrors this build's presets into
`provider_presets` and the console reads them from there, because the api tier deliberately does
not link `tankovault-adapters`. A preset a later release stops shipping is deleted from that
mirror; providers installed from it keep running, unmanaged, and the console says so.

**Run `bootstrap seed-providers` on every rollout.** It is create-only for rows it does not
manage and idempotent for the ones it does; skipping it means new providers never arrive and
managed ones stop receiving fixes. The compose stack runs it automatically.

### What an upgrade does to rows that predate the link

The first run after upgrading adopts them, and the rule is deliberately conservative: a row is
locked only if every preset-owned field still equals the shipped definition exactly. Anything
else is labelled *customised*, linked for reference and left untouched — an operator's
hand-tuned config has no other copy, and no upgrade may overwrite it silently. Both outcomes
are named per provider in the job's output.

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
| Akaza Scans | `akazascans.org` | MangaThemesia | **Config only** | family + `table.infotable` + coin lock |
| Arena Scans | `arenascan.com` | MangaThemesia | **Config only** | family defaults |
| Rage Scans | `ragescans.com` | MangaThemesia | **Config only** | family defaults |
| Rokari Comics | `rokaricomics.com` | MangaThemesia | **Config only** | family + `table.infotable` + coin lock |
| King of Shojo | `kingofshojo.com` | MangaThemesia | **Config only** | family + `table.infotable` |
| Noxen Scans | `noxenscan.com` | MangaThemesia | **Config only** | family defaults |
| Manga Trend | `mangatrend.org` | MangaThemesia | **Config only** | family + `table.infotable` |
| Violet Scans | `violetscans.org` | MangaThemesia | **Config only** | family + `/comics/` |
| Thunder Scans | `en-thunderscans.com` | MangaThemesia | **Config only** | family + `/comics/` |
| Razure | `razure.org` | MangaThemesia | **Config only** | family + `/series/` |
| Asmodeus Scans | `asmotoon.com` | Keyoapp | **Config only** | family defaults |
| Genz Toons | `genztoons.org` | Keyoapp | **Config only** | family defaults |
| Timeless Toons | `timelesstoons.org` | Keyoapp | **Config only** | family defaults |
| Mist Scans | `mistscans.com` | Keyoapp | **Config only** | family defaults |
| Grim Scans | `grimscans.com` | Keyoapp | **Config only** | family defaults |
| Kewn Scans | `kewnscans.org` | Keyoapp | **Config only** | family defaults |
| Writer Scans | `writerscans.com` | Keyoapp | **Config only** | family defaults |
| Nyanu Kafe | `nyanukafe.com` | Keyoapp | **Config only** | family defaults |
| Yaksha Comics | `yakshacomics.com` | Madara | **Config only** | family defaults |
| NatoManga | `www.natomanga.com` | Manganato | **Config + code** | `ManganatoAdapter` |
| Mangakakalot | `www.mangakakalot.gg` | Manganato | **Config + code** | `ManganatoAdapter` |
| NeloManga | `www.nelomanga.net` | Manganato | **Config + code** | `ManganatoAdapter` |
| TCB Scans | `tcbonepiecechapters.com` | Bespoke | **Config only** | generic selectors |
| Weeb Central | `weebcentral.com` | Bespoke (htmx) | **Config only** | generic selectors |
| MangaPill | `mangapill.com` | Bespoke | **Config only** | generic, sitemap mode |
| MangaDex | `mangadex.org` | JSON API | **Custom code** | `MangaDexAdapter` |
| Omega Scans | `omegascans.org` | HeanCMS JSON | **Custom code** | `HeanCmsAdapter` |
| Asura Scans | `asurascans.com` | Astro islands | **Custom code** | `AstroIslandAdapter` |
| Hive Toons | `hivetoons.org` | Astro islands | **Custom code** | `AstroIslandAdapter` |
| Flame Comics | `flamecomics.xyz` | Next.js | **Custom code** | `FlameComicsAdapter` |
| WitchToons | `witchtoons.net` | Next.js App Router | **Custom code** | `WitchToonsAdapter` |
| WEBTOON | `www.webtoons.com` | Licensed, bespoke | **Custom code** | `WebtoonsAdapter` |
| Mgread | `mgread.io` | Init Manga (WordPress) | **Config + code** | `MgreadAdapter` |
| Vortex Scans | `vortexscans.org` | Iken JSON | **Custom code** | `IkenAdapter` |
| Magus Manga | `magustoon.org` | Iken JSON | **Custom code** | `IkenAdapter` |
| Nyx Scans | `nyxscans.com` | Iken JSON | **Custom code** | `IkenAdapter` |
| Ken Scans | `kencomics.com` | Iken JSON | **Custom code** | `IkenAdapter` |
| Sana Scans | `sanascans.com` | Iken JSON | **Custom code** | `IkenAdapter` |
| Orion Scans | `orion-scans.com` | Iken JSON | **Custom code** | `IkenAdapter` |
| Rena Scans | `renascans.net` | Iken JSON | **Custom code** | `IkenAdapter` |
| Kayn Scans | `kaynscan.org` | Iken JSON | **Custom code** | `IkenAdapter` |
| Hijala Scans | `en-hijala.com` | Iken JSON | **Custom code** | `IkenAdapter` |

## Early-access (paid) chapters

Several of these sites sell chapters before releasing them free. Such a chapter is stored like
any other — it has to be, or the row is re-discovered and re-dated when the timer expires, and
`discovered_at` is what the release feed orders by — but it is stored **as what it is**:
`chapters.access` is `early_access` and `chapters.unlocks_at` carries the stated unlock time.

Where each provider publishes the fact:

| Provider | Signal | Unlock time |
|---|---|---|
| Omega Scans | `price > 0` in the chapter JSON | `free_at` |
| Asura Scans | `is_premium` in the Astro island props, or an `early_access_until` still in the future — the site stamps that date on **every** chapter, expired windows included, so the date alone is not the signal | `unlock_time` / `early_access_until` |
| Hive Toons | `isLocked` / `isTimeLocked` in the series island | `unlockAt`, on the **chapter** page — fetched per locked chapter, because the listing omits it and the window is per-chapter configurable |
| Toonily and other Madara sites | `chapters.locked` selector in the provider config | `chapters.unlock`, where rendered |
| Keyoapp sites | the coin-price badge on the chapter card (`img[alt="Coin"]`) | none — the platform states a price, never a date, so such a chapter stays locked |
| Rokari Comics, Akaza Scans | the coin plugin's price badge on the chapter row (`span.text-gold`), which the stock MangaThemesia theme does not render | none — the badge states a price, never a date, so such a chapter stays locked |
| Iken sites | `isLocked` in the chapter JSON | `unlockAt`, null on a permanently paid chapter |
| WitchToons | `isLocked` in the flight payload's chapter row | `earlyAccessUntil` / `becomesFreeAt`; `becomesFreeOnNextRelease` is a rule, not a date, so it leaves the chapter locked |
| WEBTOON | none: Fast Pass episodes are not rendered to an anonymous visitor at all | n/a |

A locked chapter does **not** count as unread. It starts counting when either its stated unlock
time passes — no rescan needed, the predicate compares the stored timestamp against `now()` — or
the reader opts that provider in at `PUT /v1/me/source-preferences`
(`early_access_provider_ids`). The opt-in is per provider because paying one scanlator says
nothing about any other. A locked chapter with **no** announced unlock time stays locked: a
missing date is not a date in the past.

A locked chapter is also not *offered*, anywhere. The rule — free, or the stated unlock time has
passed, or the reader opted this provider in — is applied by every surface that can put a chapter
in front of someone:

| Surface | What it would otherwise do |
|---|---|
| unread counts, release feed, continue-reading card | count and offer a chapter that answers with a paywall |
| the series screen's chapter list | list it, and put the "next up" marker on it — the list is scoped to the viewer, so an anonymous caller sees what an anonymous visitor sees on the provider's site |
| watchlist card figures (`latest_chapter_number`, `total_chapters`, `latest_chapter_at`) | advertise it as the series' newest chapter and newest activity — which also orders the `released` sort and fills the "today" bucket |
| continue-reading's ordering | rank a series by a release the reader cannot open |
| `services/notifier` | announce it to every watcher, and post it to the external broadcast channels, which have no reader behind them to check |
| "Mark group read" | swallow it into the frontier, so it never appears as unread once its timer expires |

`ChapterDiscovered` carries `access`/`unlocks_at` for the notifier's decision; a consumer that
ignored them would send a reader to a paywall for a chapter their own unread count refuses to
show.

Unlocking is deliberately not a second announcement. The stated unlock time is compared against
`now()` by the read paths, so a chapter opens in the reader's counts the moment it opens — with
no rescan, and without a notification that would arrive days after the row was stored.

`crates/db/tests/repo_tracking.rs` pins all of that, including the two ways it opens and the
per-provider scoping of the opt-in.

## Novels are not comics

Several platforms sell prose novels from the same catalogue, under the same URL prefix, as their
comics — a "chapter" of one is text, so there are no pages to read and nothing to track. Each
adapter drops them at the source, where the medium is still stated:

| Provider | Signal |
|---|---|
| Iken sites | `isNovel`, or `seriesType == "NOVEL"` on rows that predate the flag |
| Hive Toons | `seriesType == "NOVEL"` in the catalogue island |
| Demonic Scans | the link prefix: the feed lists novels at `/novel/`, which the catalogue never yields and the site answers with a 404 |

Dropping them at the adapter rather than at ingest matters for the walk: a catalogue's `has_next`
is decided against the site's own collection total, and that total counts the novels. Paging on
the *filtered* row count under-counts what each page consumed and walks past the end.

## Deriving a family's defaults

A family default set derived from **one** site is a preset for that site wearing a family's name,
and the 2026-08-11 expansion found three of them the hard way. Each failed the same way: parsed
cleanly, returned nothing, reported no error.

| Default | Held on | Broke on | Now |
|---|---|---|---|
| Madara `catalog.next: a.nextpostslink` | nothing checked | every install checked | cleared: a populated page chains the next one, and `mangaread` went from 12 series to ~3 200 |
| MangaThemesia `latest` = the home page's `div.utao` slider | `rizzfables` | every other install, which drops that widget | the catalogue listing re-sorted (`?order=update`), so a working catalogue implies a working feed |
| MangaThemesia `chapters.link: div.eph-num a` | `rizzfables` | forks that wrap the row in the anchor instead | the row's first anchor, which both shapes place first |
| Keyoapp `latest.path: /latest/` | `asmotoon` | eight of the nine installs, in production only | the home page's `#latest` strip |
| MangaThemesia `series` = `div.imptdt` rows + `span.mgen` genres | `rizzfables` and every install still on the stock template | four installs whose series template renders `table.infotable` label/value rows and a `div.seriestugenre` list instead | a `series` override shared by those four (`infotable_series`), matching the table rows by label |

The Keyoapp one is worth reading twice, because it hid behind an environment. `/latest/` answers
`200` from a developer's network on every install and `404` from the production host on eight of
them, so no probe run from a workstation reproduces it — the console's grouped error feed is what
surfaced it, at 123 occurrences per provider. What the probe *did* show is that the route was
wrong on its own merits: `/latest/` renders the **entire** catalogue re-sorted by update time
(729 cards and about 3 MB on the largest install), so the fast scan enumerated the whole
catalogue and fanned out a child task per series on every cycle. The replacement is a dozen
cards, and `/` is the one path a site cannot answer with a 404.

A fourth, found by walking catalogues rather than sampling one page: **a host that answers its
404s with a challenge cannot use the yielded-items fallback at all.** `yakshacomics` enumerated
all 52 of its series correctly and then failed on the request that should have ended the walk —
loud rather than silent, but retried forever. Its preset names `link[rel=next]`, which that
install renders on every page but the last, so the walk stops without asking for a page past the
end.

The rule that follows: **verify a family default against three installs before shipping it**, and
prefer the selector that is true of the theme's *contract* (an id, a data attribute, the listing
template) over one that is true of one site's stylesheet. `cargo run -p tankovault-adapters
--example probe -- <slug>` is the tool — it runs a preset's fast scan and a bounded slice of its
full scan against the live site through the production fetch stack, and reports what each would
ingest. `--walk <n>` is the deeper form: it walks consecutive catalogue pages, counts how many
series each one *adds*, and then probes a page far past the end — which is what distinguishes a
walk that terminates from one that re-serves page 1 forever, and from one that stops on page 1
because its path template only works there. Neither is visible in a single-page sample, and
neither reports an error.

## An origin can answer differently per address family

The Keyoapp `/latest/` note above says the failure "hid behind an environment". It is now clear
what that environment was, and it is not the network the probe ran from — it is the **address
family** the fetch used.

Four of the Keyoapp origins (`asmotoon`, `kewnscans`, `writerscans`, `nyanukafe`) answer an IPv4
client with a bare nginx `404` on **every** route, `/` included, and serve the same request
normally over IPv6. It is reproducible, not a flap: same second, same Cloudflare colo, same
headers, `curl -4` against three different edge addresses versus `curl -6`. A sibling install on
the same platform answers both.

That failure is unusually expensive to diagnose because a `404` is not a bot-management signal.
Nothing escalates it to the solver, the circuit breaker does not read it as challenged, and it
reads to a human as "the site removed that page" — which is what sent two investigations looking
for a renamed route. `AdapterError::Unserved` exists for exactly this shape and names it, but it
still cannot say *why*.

Two things had to be true for the crawler to be pinned to IPv4, and both are fixed:

- **The container had no IPv6 address.** `deploy/docker-compose.yml` now sets `enable_ipv6` on the
  default network.
- **The resolver handed back IPv4 first.** A container on Docker's NAT66 has a *unique-local*
  source address, so RFC 6724's scope-match rule ranks a global IPv4 destination above a global
  IPv6 one and both glibc and musl return IPv4 first — the connector took it and never tried the
  rest. [`SsrfResolver`](../crates/fetch/src/ssrf.rs) now orders its filtered answer IPv6-first,
  which is what a browser does. It is a preference, not a requirement: the connector falls back,
  and a blackholed IPv6 route costs about 250 ms per connection.

The diagnostic that generalises: when a provider fails on every route including `/`, **probe the
same URL over each address family before concluding anything about the route.** A `curl -4` and a
`curl -6` a second apart is the whole test, and it separates a dead origin from an unreachable one
— which is how `sirenscans` was retired with confidence while its four siblings were kept.

## A listing row is not always a series

The three Manganato domains render a **sponsored card in the same container as every real row**
(`div.list-comic-item-wrap`), first on both the catalogue and the latest-updates feed. Its link is
a rotating `bit.ly` short URL, and
[`html::relativize`](../crates/adapters/src/html.rs) flattens a foreign host to its path on
purpose — a provider that changes domain must not need a data migration — so
`https://bit.ly/scrailadi` arrived as `/scrailadi`, which reads exactly like a series slug. Every
scan registered it: a series row named after the campaign, a source row under it, and a fetch that
404s on every fast scan afterwards. The rotation whose `href` the banner script had not rewritten
yet resolved to the *listing page itself*, which answers `200` and fails on the missing series
title instead — two error texts, one card.

`generic::is_series_link` now drops a listing row whose link points at another host (ignoring a
leading `www.`, and comparing against the URL *after* redirects) or back at the page it was found
on. Neither is something a real card does, and both are conditions no selector can express:
tightening the item selector would only work until the ad network rotates its class names, which
on these sites are hashed and rotate already. Migration `0054` removes what the scans before it
wrote.

## A limitation worth stating: Madara's AJAX chapter list

Most live Madara installs no longer render `li.wp-manga-chapter` on the series page at all; the
list arrives from `POST {series}/ajax/chapters/`, and a `GET` of that path returns the series page
again. The fetch stack is GET-only by construction (`FetchRequest`), so such a site parses a
perfect catalogue, a perfect feed, and then ingests **zero chapters** — silently, because an empty
list is a valid answer.

Seven otherwise-healthy Madara candidates were left out of the expansion for this reason. Adding
them means giving the fetch stack a POST it does not have today, which widens provider egress and
is a policy decision rather than a patch; until then, a Madara site has to be checked for a
server-rendered chapter list before it is worth a preset.

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

This preset change reaches an existing deployment on its next `bootstrap seed-providers` run,
because the `kunmanga` row follows its preset — see [Managed presets](#managed-presets). It
did **not**, before that link existed: seeding was create-only, so the fix reached new
installations and nothing else.

> **Note on body size.** A shard is ~2.9 MB as raw XML, comfortably under the fetch stack's
> 8 MB cap. When a fetch has to be solved, the headless browser returns an XML *viewer* page
> that embeds the document twice (~16 MB); that body arrives via the solver path, which the
> cap does not apply to. `sitemap_locs` handles both shapes. Once a solved session is cached
> and replayed, subsequent shard fetches are plain requests returning the small raw XML.

## Mgread — custom adapter over the generic parser

`mgread.io` runs the **Init Manga** WordPress theme. Catalogue, feed and metadata are ordinary
markup and stay in the preset config; only the chapter list forces
[`MgreadAdapter`](../crates/adapters/src/mgread.rs).

**The series page renders only its newest 24 chapter rows.** The rest arrive from
`GET /wp-json/initmanga/v1/chapters?manga_id={id}&per_page=50&paged={n}`, whose envelope carries
`items` and `total_pages`. `per_page` is clamped at 50 server-side — asking for more returns 50
and recomputes `total_pages` for 50, so overshooting reads as a *shorter* series rather than
failing.

That endpoint is keyed by the numeric post id, which the adapter resolves through the WordPress
core route `GET /wp-json/wp/v2/manga?slug={slug}&_fields=id,date,date_gmt` — about a hundred
bytes, and it carries the site's UTC offset as the gap between its two timestamps. The series
page states both facts as well (`h1#manga-title[data-id]`, and any chapter row's `<time
datetime>`), and the adapter falls back to it when the core route is locked down, as WordPress
deployments commonly do; that path costs 130 KB per series on top of the copy `fetch_series` has
already read, which is why it is the fallback and not the rule.

Two things about the config are worth knowing before editing it:

- **`catalog.next` is `link[rel=next]`,** not the visible paginator. The theme renders a "Next
  page" control on the last listing as well, and the page after the last answers `404` — so the
  yielded-items fallback would end every full scan on an error instead of on a signal.
- **`series.tags` is anchored on the genre archive** (`a[href*="/genre/"]`). The last pill in
  `#genre-tags` is the theme's estimated reading time (`4h 52m to finish`), an `href="#"` button
  wearing the genres' classes; interned as a tag it becomes a Discover facet and a recommender
  feature, one per distinct duration.

**Dates.** The chapter endpoint serves WordPress's stored *site-local* time
(`2026-08-15 14:44:29`) with no offset on it. Applying the site's own offset — from the pair
above, or from a chapter row's RFC 3339 `datetime` on the fallback path — is what keeps releases
where the site puts them: reading them as UTC would move every one of them seven hours on this
site, and hard-coding `+07:00` would be wrong the day the operator moves it.

## Demonic Scans — custom adapter

`demonicscans.org` is not Madara, and two fields need transforms a selector can't express,
so it ships [`DemonicScansAdapter`](../crates/adapters/src/demonicscans.rs):

- **Catalogue:** `/advanced.php?list={page}`; items `div.advanced-element`; the series link
  is the item anchor, and its full title is the anchor's `title` attribute (the visible
  `<h1>` is ellipsis-truncated). Next page = an explicit anchor whose text is "Next".
- **Latest feed:** the home page (`/`); items `div.updates-element`; title from `h2 a`;
  newest chapter from the first `a.chplinks`. Links outside `/manga/` are dropped: the feed also
  lists the site's text novels, which the catalogue never yields and the site itself answers
  with a 404.
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

### Three refusals that look like ordinary answers

Each of these arrives wearing a status that means something else, and each was diagnosed as the
wrong problem until it was classified on its own terms.

- **The site is not being served at all.** An origin whose application is down answers from its
  web server's *built-in* error document — a few hundred bytes of bare nginx or Apache — and it
  does so for every route, `/` included. Read as the `404` it wears, that says "the feed moved",
  which is why the first investigation went looking for a renamed route.
  [`default_error_page_server`](../crates/solver/src/detection.rs) recognises the compiled-in
  markup under a size cap, and `AdapterError::Unserved` says what it means. A site's *own* 404 —
  its theme, its navigation, tens of kilobytes — is untouched by this and stays a plain `404`,
  because there the path really is gone.

  It is deliberately **not** transient. A dead origin does recover, but not within a run and not
  on the delivery ladder's timescale, so three deliveries only triple the requests it is already
  ignoring. Coming back later is the scheduler's decision, taken per provider with a growing
  cooldown (docs/OPERATIONS.md §7).
- **Our own solver was not there.** A saturated browser pool or a restarting solver replica is
  this deployment's transient, not the provider's answer — but both used to arrive as
  `SolveError::Unsolved`, whose message reads *"solver could not bypass the challenge"*. Two
  providers spent a day reported as hostile while their APIs answered `200` to a plain request.
  `SolveError::Unavailable` is now separate, the `/v1/solve` contract carries the distinction in
  its status (`5xx` unavailable, `4xx` unsolved, so a gateway's own `502` cannot be mistaken for
  a verdict), and `RetryingSolver` repeats a solve the tier could not serve — never one the
  provider defeated, which would re-run a full browser solve for the same result.
- **The origin refused our headers.** A solver hands back a *browser's* cookie jar, which on a
  site with an analytics stack accumulates cookies the site never reads. Replayed in full it can
  outgrow the origin's header buffer, and nginx answers `400 Request Header Or Cookie Too Large`
  — from its own error page, so it is invisible unless something reads it. The replayed `Cookie`
  header is now capped, keeping the clearance and session cookies when something has to go, and a
  session the origin refuses on those grounds is dropped and the request retried without it.
  Expiry alone could not recover: the refused session is replayed on every request until its TTL
  lapses.

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
