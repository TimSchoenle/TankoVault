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
| KunManga | `www.kunmanga.co.uk` | Madara | **Config only** | Madara + overrides |

Two of the three requested sites — **manhuaus** and **kunmanga** — are **just a config of
the existing Madara parser**. Only **demonicscans** required a new parser.

## Manhuaus — Madara config only

Standard Madara; only two deviations from the defaults:

| Field | Default | Override | Why |
|---|---|---|---|
| `catalog.path` | `/manga/?page={page}` | `/manga/page/{page}/` | Site paginates on the path, not a query. |
| `catalog.next` | `a.nextpostslink` | `link[rel=next]` | Theme omits `nextpostslink`; the `<head>` rel=next link is the reliable has-next marker. |
| `series.cover` | `div.summary_image img@src` | `div.summary_image img@data-src` | Covers are lazy-loaded — `src` is a placeholder, the real URL is in `data-src`. |

Everything else (catalog item `div.page-item-detail`, title `h3 a`, series title/desc/
status/tags, chapters `li.wp-manga-chapter`) is the Madara default.

## KunManga — Madara config only

Madara with an ad-injected catalogue and Bootstrap pagination:

| Field | Default | Override | Why |
|---|---|---|---|
| `catalog.path` | `/manga/?page={page}` | `/manga/page/{page}` | Path pagination, no trailing slash. |
| `catalog.item` | `div.page-item-detail` | `div.page-item-detail:not(.custom-item-ad)` | Skip injected advertisement tiles (they link off-site to `dub.sh`). |
| `catalog.next` | `a.nextpostslink` | `a[aria-label="Next"]` | Bootstrap paginator; the aria-labelled Next control marks additional pages. |
| `chapters.container` | `li.wp-manga-chapter` | `div.wp-manga-chapter` | Chapter rows are `<div>`, not `<li>`. |

Covers use a plain `src` on a separate CDN host (`cdn.zinmanga1.com`), so the default
`img@src` works unchanged.

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
