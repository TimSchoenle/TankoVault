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
(`tankovault_adapters::builtin_presets()`), seeded by `xtask seed`. Every selector below was
derived from **live markup fetched through the solver pipeline** and is pinned by fixture
tests, so a provider layout change fails a test rather than corrupting production data.

## Summary

| Provider | Domain | Layout | Onboarding | Adapter |
|---|---|---|---|---|
| Demonic Scans | `demonicscans.org` | Bespoke PHP | **Custom code** | `DemonicScansAdapter` |
| Manhuaus | `manhuaus.com` | Madara | **Config only** | Madara + overrides |
| KunManga | `www.kunmanga.co.uk` | Madara-ish hybrid | **Custom code** | `KunMangaAdapter` |

**manhuaus** is **just a config of the existing Madara parser**. **kunmanga** renders
Madara-shaped series HTML but serves chapters from a JSON API and cannot be enumerated
through its own listing, so it wraps the generic parser in a small custom adapter.
**demonicscans** required a full parser of its own.

## Manhuaus — Madara config only

Standard Madara; only two deviations from the defaults:

| Field | Default | Override | Why |
|---|---|---|---|
| `catalog.path` | `/manga/?page={page}` | `/manga/page/{page}/` | Site paginates on the path, not a query. |
| `catalog.next` | `a.nextpostslink` | `link[rel=next]` | Theme omits `nextpostslink`; the `<head>` rel=next link is the reliable has-next marker. |
| `series.cover` | `div.summary_image img@src` | `div.summary_image img@data-src` | Covers are lazy-loaded — `src` is a placeholder, the real URL is in `data-src`. |

Everything else (catalog item `div.page-item-detail`, title `h3 a`, series title/desc/
status/tags, chapters `li.wp-manga-chapter`) is the Madara default.

## KunManga — custom adapter over the generic parser

`www.kunmanga.co.uk` is Madara-shaped in its markup but deviates in two ways a selector
cannot express, so it ships [`KunMangaAdapter`](../crates/adapters/src/kunmanga.rs), which
delegates latest/series parsing to the generic config adapter and overrides the rest.

**1. Chapters come from a JSON API, not the HTML.** The `div.wp-manga-chapter` rows are
rendered client-side; a non-JS fetch of a series page yields zero chapters. `fetch_chapters`
pages `/api/comics/{slug}/chapters?page={n}&per_page=100&order=asc` instead, tolerating both
raw JSON and solver-wrapped (HTML-escaped) JSON.

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
`img@src` works unchanged. Only `series.release` is overridden (the year is only reliably
available as a link into the `manga-release` archive).

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

## Adding another provider

- **Madara-like site:** add a `ProviderPreset` (or create the row in the admin console) with
  `adapter = madara` and only the overriding selectors. Add a fixture + test if it deviates
  from the defaults. No code.
- **New layout:** add a `SourceAdapter` struct, register its slug in `factory.rs`, add a
  preset with `adapter = custom`, and add fixture tests. Prefer widening the config schema
  over a custom adapter when the difference is purely selectors.
