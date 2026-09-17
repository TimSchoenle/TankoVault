# Watchlist backup and restore

A reader can download their watchlist as a file and restore it — into the same deployment after a
database wipe, or into a different deployment altogether. This document covers the file format,
how an import finds each series again, and the limits that make the import safe to expose.

| Piece | Where |
| --- | --- |
| Wire format, version dispatch, report types | `crates/contracts/src/watchlist_transfer.rs` |
| Validation, matching and merge rules (pure) | `crates/domain/src/watchlist_transfer.rs` |
| Export query, batched resolution, transactional apply, pending queue | `crates/db/src/repo/tracking/watchlist/transfer.rs` |
| HTTP surface | `services/api/src/me/watchlist_transfer.rs` |
| Pending-entry sweep | `services/control-plane/src/main.rs` (`maybe_resolve_watchlist_imports`) |
| Reader UI | `web/frontend/src/views/account/backup.rs` (`/account/backup`) |

## Endpoints

| Route | Auth | Purpose |
| --- | --- | --- |
| `GET /v1/me/watchlist/export` | session | The backup document, as an attachment, `Cache-Control: no-store` |
| `POST /v1/me/watchlist/import/preview` | session | The import computed and rolled back; writes nothing |
| `POST /v1/me/watchlist/import` | session **+ step-up** | Apply a backup in one transaction |
| `GET /v1/me/watchlist/import/pending` | session | Entries still waiting for their series |
| `DELETE /v1/me/watchlist/import/pending` | session | Stop waiting for them |

Both import routes take `mode` (`merge` \| `overwrite` \| `replace`) and `match_titles` (default
`false`) as query parameters, and the document as an `application/json` body.

## Identity: no database ids

A uuid means nothing outside the database that minted it, so the document never carries one. Each
entry names its series by what another catalogue can rediscover independently, strongest first:

1. **External tracker ids** (`anilist`, …) — matched against `sync_mappings`.
2. **Provider pages** — each source carries the provider `slug`, the site-relative `path` and the
   absolute `url`. A page matches a local `series_sources` row by *either* slug and path (survives
   a provider moving domain) *or* the URL's host (ignoring `www.`) and the path below the
   provider's `base_url` (survives another deployment naming the provider differently).
3. **Title** — only with `match_titles`, and only when exactly one series carries the canonical or
   an alternative title under the same whitespace-insensitive normalisation the matcher uses.
   Titles are where different works collide, so an ambiguous title matches nothing.

The first route that finds a series wins. The first entry to claim a series wins; later entries
naming the same series are reported as duplicates.

## Modes

| Mode | New series | Series already tracked | Tracked series missing from the file |
| --- | --- | --- | --- |
| `merge` | added | kept as they are; read progress only moves forward | kept |
| `overwrite` | added | take the backup's status, flags, pin and progress | kept |
| `replace` | added | as `overwrite` | removed, and all older pending entries discarded |

Progress keeps the `floor(part) >= whole` invariant in every mode, and an empty progress value never
creates a `read_progress` row.

## Pending entries

An entry no route matches is not dropped: it is queued in `watchlist_import_pending` with its
identifiers and choices. The control plane's leader retries the least recently attempted 500 every
`scheduler.watchlist_import_resolve_interval_secs` (default 900), and applies each one whose series
a scan has since ingested, under the mode and title-matching choice of the import that queued it.
This is what makes restoring into a freshly wiped database work: the import queues nearly
everything, and entries attach as providers are crawled.

Rows are keyed by `(user_id, identity_key)`, so importing the same backup twice queues each entry
once. A reader may hold at most 10 000 pending entries; an import that would exceed that is refused
with `409` and writes nothing. Pending entries are included in the GDPR data export and removed with
the account.

## Versioning

Every document declares `"format": "tankovault.watchlist"` and an integer `version`. A released
version's shape is frozen and refuses unknown fields. Changing what an entry holds means:

1. add a `vN` module beside `v1` in `crates/contracts/src/watchlist_transfer.rs`, with its own
   conversion into `PortableEntry`;
2. add its arm to `parse_document` and bump `CURRENT_VERSION`;
3. keep every older arm, so existing backups still restore.

A document with a version newer than the server's is refused by name rather than partly read.

## Security properties

- **Bounded input.** 8 MiB body (raised for the two import routes only), 10 000 entries, and per-entry
  limits on titles, sources, external ids and string lengths, all checked before the database is
  touched. Both import routes draw on the `expensive` rate-limit budget.
- **Step-up on apply.** `overwrite` and `replace` can rewrite or empty a library in one call.
  Preview needs no elevation, because it writes nothing.
- **All or nothing.** One transaction per import; a preview is the same transaction rolled back.
- **Adult gate honoured.** A gated series the reader may not see is treated as absent, so an import
  cannot probe which gated series exist. The sweep re-evaluates the gate at attach time.
- **Scoped writes.** A pinned source is applied only if it belongs to the matched series, checked in
  the planner and again in SQL. `added_at` is capped at `now()`.
- **URLs are compared, never fetched**, so they need no SSRF check; URLs embedding credentials are
  refused anyway.
- **Audited.** `watchlist.export`, `watchlist.import` and `watchlist.import.pending.clear`.
- **Always re-importable.** Export sanitises catalogue data the crawler wrote (over-long or
  control-character titles, unparseable sources) so a reader's own backup never fails validation.
