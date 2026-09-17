//! Watchlist backups: reading a watchlist as portable entries, importing portable entries into
//! one, and the queue of imported entries still waiting for their series to be crawled.
//!
//! The matching and merge rules are [`tankovault_domain::watchlist_transfer`]'s; this module
//! only fetches what they need and writes what they decide.

use std::collections::{HashMap, HashSet};

use crate::error::DbResult;
use serde::Serialize;
use sqlx::types::Json;
use sqlx::{PgConnection, PgExecutor, PgPool};
use tankovault_domain::watchlist_transfer::{
    CatalogueProvider, ConflictPolicy, ExternalRef, LocalEntry, Lookups, MAX_ENTRIES, Plan,
    PortableEntry, PortableProgress, ProviderIndex, Resolved, SourceRef, plan, resolve,
};
use tankovault_domain::{ProviderId, SeriesId, SeriesSourceId, UserId, WatchStatus, resolve_link};
use time::OffsetDateTime;
use uuid::Uuid;

/// Every entry on `user_id`'s watchlist as a portable, validated entry, oldest first.
///
/// Sources are listed pinned-first, then by provider slug and path; each entry is passed through
/// [`PortableEntry::sanitized`], so the result always re-imports.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only. An empty watchlist is an empty `Vec`.
pub async fn watchlist_export<'e, E: PgExecutor<'e>>(
    exec: E,
    user_id: UserId,
) -> DbResult<Vec<PortableEntry>> {
    #[derive(Debug, serde::Deserialize)]
    struct SourceRow {
        provider: String,
        base_url: String,
        path: String,
        pinned: bool,
    }
    #[derive(Debug, serde::Deserialize)]
    struct ExternalRow {
        tracker: String,
        id: String,
    }

    // Both lists are capped above the document's own limits, so `sanitized` still has valid
    // items to keep when a few at the front are dropped as invalid.
    let rows = sqlx::query!(
        r#"SELECT s.canonical_title AS "title!",
                  ARRAY(SELECT t.title FROM series_titles t
                         WHERE t.series_id = w.series_id
                         ORDER BY t.title LIMIT 64) AS "alternative_titles!",
                  w.status AS "status!: WatchStatus",
                  w.notify AS "notify!",
                  w.sync_excluded AS "sync_excluded!",
                  w.added_at AS "added_at!",
                  p.last_read_whole_number::float8 AS "whole?",
                  p.last_read_part_number::float8 AS "part?",
                  (SELECT coalesce(jsonb_agg(jsonb_build_object(
                             'provider', src.slug, 'base_url', src.base_url,
                             'path', src.source_path, 'pinned', src.pinned)
                           ORDER BY src.pinned DESC, src.slug, src.source_path), '[]'::jsonb)
                     FROM (SELECT pr.slug, pr.base_url, ss.source_path,
                                  ss.id IS NOT DISTINCT FROM w.pinned_source_id AS pinned
                             FROM series_sources ss
                             JOIN providers pr ON pr.id = ss.provider_id
                            WHERE ss.series_id = w.series_id
                            ORDER BY pinned DESC, pr.slug, ss.source_path
                            LIMIT 64) src) AS "sources!: Json<Vec<SourceRow>>",
                  (SELECT coalesce(jsonb_agg(jsonb_build_object('tracker', m.provider, 'id', m.external_id)
                           ORDER BY m.provider, m.external_id), '[]'::jsonb)
                     FROM sync_mappings m
                    WHERE m.series_id = w.series_id) AS "external_ids!: Json<Vec<ExternalRow>>"
             FROM watchlist_entries w
             JOIN series s ON s.id = w.series_id
             LEFT JOIN read_progress p ON p.user_id = w.user_id AND p.series_id = w.series_id
            WHERE w.user_id = $1
            ORDER BY w.added_at, w.series_id"#,
        user_id.as_uuid(),
    )
    .fetch_all(exec)
    .await?;

    Ok(rows
        .into_iter()
        .filter_map(|r| {
            PortableEntry {
                title: r.title,
                alternative_titles: r.alternative_titles,
                status: r.status,
                notify: r.notify,
                sync_excluded: r.sync_excluded,
                added_at: r.added_at,
                progress: r.whole.map(|whole| PortableProgress {
                    whole,
                    part: r.part,
                }),
                sources: r
                    .sources
                    .0
                    .into_iter()
                    .filter_map(|s| {
                        Some(SourceRef {
                            url: resolve_link(&s.base_url, &s.path).ok()?,
                            provider: s.provider,
                            path: s.path,
                            pinned: s.pinned,
                        })
                    })
                    .collect(),
                external_ids: r
                    .external_ids
                    .0
                    .into_iter()
                    .map(|e| ExternalRef {
                        tracker: e.tracker,
                        id: e.id,
                    })
                    .collect(),
            }
            .sanitized()
        })
        .collect())
}

/// How an import treats the watchlist it lands in.
#[derive(Debug, Clone, Copy)]
pub struct ImportOptions {
    /// Which side wins for a series already tracked.
    pub policy: ConflictPolicy,
    /// Remove every tracked series, and every pending entry, the document does not name.
    pub remove_missing: bool,
    /// Allow a unique title to match when no provider page or external id does.
    pub match_titles: bool,
    /// Whether adult-gated series may be matched. A series the reader may not see is treated as
    /// absent, so an import cannot be used to learn which gated series exist.
    pub include_adult: bool,
}

/// Whether an import keeps what it wrote.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Commit {
    /// Commit the transaction.
    Apply,
    /// Roll it back, having computed the same outcome.
    DryRun,
}

/// What an import did, or would do.
#[derive(Debug, Default)]
pub struct ImportOutcome {
    /// The decisions, with per-entry indices.
    pub plan: Plan,
    /// Series removed from the watchlist by `remove_missing`.
    pub removed: usize,
}

/// Why an import was refused without writing anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportRefusal {
    /// Queueing the unmatched entries would take the reader past [`MAX_ENTRIES`] pending entries.
    PendingLimit,
}

/// Import `entries` into `user_id`'s watchlist in one transaction, queueing whatever matches no
/// series yet.
///
/// All or nothing: a failure part-way leaves the watchlist, the progress and the pending queue as
/// they were. A dry run takes the same path and rolls back, so its answer is the one the real
/// import would give against the same data.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only. A refused import is `Ok(Err(ImportRefusal))`, with nothing
/// written.
pub async fn watchlist_import(
    pool: &PgPool,
    user_id: UserId,
    entries: &[PortableEntry],
    options: ImportOptions,
    commit: Commit,
) -> DbResult<Result<ImportOutcome, ImportRefusal>> {
    let mut tx = pool.begin().await?;
    let resolved = resolve_entries(
        &mut tx,
        entries,
        options.match_titles,
        options.include_adult,
    )
    .await?;
    let outcome = plan_and_apply(&mut tx, user_id, entries, &resolved, options).await?;

    let unmatched: Vec<&PortableEntry> = outcome
        .plan
        .unmatched
        .iter()
        .map(|&i| &entries[i])
        .collect();
    if options.remove_missing {
        sqlx::query!(
            "DELETE FROM watchlist_import_pending WHERE user_id = $1",
            user_id.as_uuid()
        )
        .execute(&mut *tx)
        .await?;
    } else {
        let matched_keys: Vec<String> = entries
            .iter()
            .zip(&resolved)
            .filter(|(_, r)| r.is_some())
            .map(|(e, _)| e.identity_key())
            .collect();
        sqlx::query!(
            "DELETE FROM watchlist_import_pending WHERE user_id = $1 AND identity_key = ANY($2)",
            user_id.as_uuid(),
            &matched_keys,
        )
        .execute(&mut *tx)
        .await?;

        let new_keys: HashSet<String> = unmatched.iter().map(|e| e.identity_key()).collect();
        let new_keys: Vec<String> = new_keys.into_iter().collect();
        let others = sqlx::query_scalar!(
            "SELECT count(*) AS \"n!\" FROM watchlist_import_pending \
              WHERE user_id = $1 AND NOT (identity_key = ANY($2))",
            user_id.as_uuid(),
            &new_keys,
        )
        .fetch_one(&mut *tx)
        .await?;
        if usize::try_from(others).unwrap_or(usize::MAX) + new_keys.len() > MAX_ENTRIES {
            tx.rollback().await?;
            return Ok(Err(ImportRefusal::PendingLimit));
        }
    }
    queue_pending(&mut tx, user_id, &unmatched, options).await?;

    match commit {
        Commit::Apply => tx.commit().await?,
        Commit::DryRun => tx.rollback().await?,
    }
    Ok(Ok(outcome))
}

/// The `(provider, stored path)` pairs, external ids and title keys a batch of entries asks
/// about, collected so each kind is one query.
#[derive(Default)]
struct Wanted {
    sources: HashSet<(ProviderId, String)>,
    external: HashSet<(String, String)>,
    titles: HashSet<String>,
}

/// Match every entry against the catalogue in at most four queries, however many entries there
/// are.
async fn resolve_entries(
    conn: &mut PgConnection,
    entries: &[PortableEntry],
    match_titles: bool,
    include_adult: bool,
) -> DbResult<Vec<Option<Resolved>>> {
    let providers = sqlx::query!("SELECT id, slug, base_url FROM providers")
        .fetch_all(&mut *conn)
        .await?;
    let index = ProviderIndex::new(providers.into_iter().map(|p| CatalogueProvider {
        id: ProviderId::from_uuid(p.id),
        slug: p.slug,
        base_url: p.base_url,
    }));

    let mut wanted = Wanted::default();
    for entry in entries {
        for source in &entry.sources {
            wanted.sources.extend(index.candidates(source));
        }
        for external in &entry.external_ids {
            wanted
                .external
                .insert((external.tracker.clone(), external.id.clone()));
        }
        if match_titles {
            wanted.titles.extend(entry.title_keys());
        }
    }

    let lookups = Lookups {
        sources: lookup_sources(conn, wanted.sources, include_adult).await?,
        external: lookup_external(conn, wanted.external, include_adult).await?,
        titles: lookup_titles(conn, wanted.titles, include_adult).await?,
    };
    Ok(entries
        .iter()
        .map(|e| resolve(e, &index, &lookups, match_titles))
        .collect())
}

async fn lookup_sources(
    conn: &mut PgConnection,
    wanted: HashSet<(ProviderId, String)>,
    include_adult: bool,
) -> DbResult<HashMap<(ProviderId, String), (SeriesId, SeriesSourceId)>> {
    if wanted.is_empty() {
        return Ok(HashMap::new());
    }
    let (provider_ids, paths): (Vec<Uuid>, Vec<String>) = wanted
        .into_iter()
        .map(|(p, path)| (p.as_uuid(), path))
        .unzip();
    let rows = sqlx::query!(
        r#"SELECT ss.provider_id, ss.source_path, ss.series_id, ss.id
             FROM unnest($1::uuid[], $2::text[]) AS r(provider_id, source_path)
             JOIN series_sources ss
               ON ss.provider_id = r.provider_id AND ss.source_path = r.source_path
             JOIN series s ON s.id = ss.series_id
            WHERE NOT s.adult_gated OR $3"#,
        &provider_ids,
        &paths,
        include_adult,
    )
    .fetch_all(&mut *conn)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| {
            (
                (ProviderId::from_uuid(r.provider_id), r.source_path),
                (
                    SeriesId::from_uuid(r.series_id),
                    SeriesSourceId::from_uuid(r.id),
                ),
            )
        })
        .collect())
}

async fn lookup_external(
    conn: &mut PgConnection,
    wanted: HashSet<(String, String)>,
    include_adult: bool,
) -> DbResult<HashMap<(String, String), SeriesId>> {
    if wanted.is_empty() {
        return Ok(HashMap::new());
    }
    let (trackers, ids): (Vec<String>, Vec<String>) = wanted.into_iter().unzip();
    let rows = sqlx::query!(
        r#"SELECT m.provider, m.external_id, m.series_id
             FROM unnest($1::text[], $2::text[]) AS r(provider, external_id)
             JOIN sync_mappings m
               ON m.provider = r.provider AND m.external_id = r.external_id
             JOIN series s ON s.id = m.series_id
            WHERE NOT s.adult_gated OR $3"#,
        &trackers,
        &ids,
        include_adult,
    )
    .fetch_all(&mut *conn)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| {
            (
                (r.provider, r.external_id),
                SeriesId::from_uuid(r.series_id),
            )
        })
        .collect())
}

async fn lookup_titles(
    conn: &mut PgConnection,
    wanted: HashSet<String>,
    include_adult: bool,
) -> DbResult<HashMap<String, Vec<SeriesId>>> {
    if wanted.is_empty() {
        return Ok(HashMap::new());
    }
    let keys: Vec<String> = wanted.into_iter().collect();
    // Two arms rather than an `OR` across the tables, so each can use its own
    // `replace(…, ' ', '')` index.
    let rows = sqlx::query!(
        r#"SELECT k.key AS "key!", s.id AS "series_id!"
             FROM unnest($1::text[]) AS k(key)
             JOIN series s ON replace(s.normalized_title, ' ', '') = k.key
            WHERE NOT s.adult_gated OR $2
           UNION
           SELECT k.key, s.id
             FROM unnest($1::text[]) AS k(key)
             JOIN series_titles t ON replace(t.normalized, ' ', '') = k.key
             JOIN series s ON s.id = t.series_id
            WHERE NOT s.adult_gated OR $2"#,
        &keys,
        include_adult,
    )
    .fetch_all(&mut *conn)
    .await?;
    let mut titles: HashMap<String, Vec<SeriesId>> = HashMap::new();
    for r in rows {
        titles
            .entry(r.key)
            .or_default()
            .push(SeriesId::from_uuid(r.series_id));
    }
    Ok(titles)
}

/// A reader's watchlist entries and read progress, as the planner compares against them.
struct LocalState {
    entries: HashMap<SeriesId, LocalEntry>,
    progress: HashMap<SeriesId, PortableProgress>,
}

async fn load_local(conn: &mut PgConnection, user_id: UserId) -> DbResult<LocalState> {
    let entry_rows = sqlx::query!(
        r#"SELECT series_id, status AS "status: WatchStatus", notify, sync_excluded, pinned_source_id
             FROM watchlist_entries WHERE user_id = $1"#,
        user_id.as_uuid(),
    )
    .fetch_all(&mut *conn)
    .await?;
    let progress_rows = sqlx::query!(
        r#"SELECT series_id,
                  last_read_whole_number::float8 AS "whole!",
                  last_read_part_number::float8 AS part
             FROM read_progress WHERE user_id = $1"#,
        user_id.as_uuid(),
    )
    .fetch_all(&mut *conn)
    .await?;
    Ok(LocalState {
        entries: entry_rows
            .into_iter()
            .map(|r| {
                (
                    SeriesId::from_uuid(r.series_id),
                    LocalEntry {
                        status: r.status,
                        notify: r.notify,
                        sync_excluded: r.sync_excluded,
                        pinned_source: r.pinned_source_id.map(SeriesSourceId::from_uuid),
                    },
                )
            })
            .collect(),
        progress: progress_rows
            .into_iter()
            .map(|r| {
                (
                    SeriesId::from_uuid(r.series_id),
                    PortableProgress {
                        whole: r.whole,
                        part: r.part,
                    },
                )
            })
            .collect(),
    })
}

/// Plan the import against the reader's current watchlist and write the plan.
async fn plan_and_apply(
    conn: &mut PgConnection,
    user_id: UserId,
    entries: &[PortableEntry],
    resolved: &[Option<Resolved>],
    options: ImportOptions,
) -> DbResult<ImportOutcome> {
    let local = load_local(conn, user_id).await?;
    let plan = plan(
        entries,
        resolved,
        &local.entries,
        &local.progress,
        options.policy,
    );
    write_entries(conn, user_id, &plan).await?;
    write_progress(conn, user_id, &plan).await?;

    let removed = if options.remove_missing {
        local
            .entries
            .keys()
            .filter(|id| !plan.matched.contains(id))
            .count()
    } else {
        0
    };
    if removed > 0 {
        let keep: Vec<Uuid> = plan.matched.iter().map(|id| id.as_uuid()).collect();
        sqlx::query!(
            "DELETE FROM watchlist_entries WHERE user_id = $1 AND NOT (series_id = ANY($2))",
            user_id.as_uuid(),
            &keep,
        )
        .execute(&mut *conn)
        .await?;
    }
    Ok(ImportOutcome { plan, removed })
}

async fn write_entries(conn: &mut PgConnection, user_id: UserId, plan: &Plan) -> DbResult<()> {
    if plan.writes.is_empty() {
        return Ok(());
    }
    let n = plan.writes.len();
    let mut ids = Vec::with_capacity(n);
    let mut statuses = Vec::with_capacity(n);
    let mut notify = Vec::with_capacity(n);
    let mut excluded = Vec::with_capacity(n);
    let mut added_at = Vec::with_capacity(n);
    let mut pinned = Vec::with_capacity(n);
    for w in &plan.writes {
        ids.push(w.series_id.as_uuid());
        statuses.push(w.status.as_str().to_owned());
        notify.push(w.notify);
        excluded.push(w.sync_excluded);
        added_at.push(w.added_at);
        pinned.push(w.pinned_source.map(SeriesSourceId::as_uuid));
    }
    // The pin is re-scoped to its series here as well as in the planner: `series_sources` ids are
    // global, and this statement is the last point before one is persisted. `added_at` is capped
    // at `now()` so a hand-edited backup cannot plant an entry dated in the future.
    sqlx::query!(
        r#"INSERT INTO watchlist_entries
                  (user_id, series_id, status, notify, sync_excluded, added_at, pinned_source_id)
           SELECT $1, r.series_id, r.status::watch_status, r.notify, r.sync_excluded,
                  LEAST(r.added_at, now()),
                  CASE WHEN EXISTS (SELECT 1 FROM series_sources ss
                                     WHERE ss.id = r.pinned AND ss.series_id = r.series_id)
                       THEN r.pinned END
             FROM unnest($2::uuid[], $3::text[], $4::bool[], $5::bool[],
                         $6::timestamptz[], $7::uuid[])
                  AS r(series_id, status, notify, sync_excluded, added_at, pinned)
           ON CONFLICT (user_id, series_id) DO UPDATE
              SET status = EXCLUDED.status,
                  notify = EXCLUDED.notify,
                  sync_excluded = EXCLUDED.sync_excluded,
                  pinned_source_id = EXCLUDED.pinned_source_id,
                  updated_at = now()"#,
        user_id.as_uuid(),
        &ids,
        &statuses,
        &notify,
        &excluded,
        &added_at,
        &pinned as &[Option<Uuid>],
    )
    .execute(&mut *conn)
    .await?;
    Ok(())
}

async fn write_progress(conn: &mut PgConnection, user_id: UserId, plan: &Plan) -> DbResult<()> {
    if plan.progress.is_empty() {
        return Ok(());
    }
    let n = plan.progress.len();
    let mut ids = Vec::with_capacity(n);
    let mut wholes = Vec::with_capacity(n);
    let mut parts = Vec::with_capacity(n);
    for (id, p) in &plan.progress {
        ids.push(id.as_uuid());
        wholes.push(p.whole);
        parts.push(p.part);
    }
    sqlx::query!(
        r#"INSERT INTO read_progress
                  (user_id, series_id, last_read_whole_number, last_read_part_number)
           SELECT $1, r.series_id, r.whole::numeric(10,4), r.part::numeric(10,4)
             FROM unnest($2::uuid[], $3::float8[], $4::float8[]) AS r(series_id, whole, part)
           ON CONFLICT (user_id, series_id) DO UPDATE
              SET last_read_whole_number = EXCLUDED.last_read_whole_number,
                  last_read_part_number = EXCLUDED.last_read_part_number,
                  updated_at = now()"#,
        user_id.as_uuid(),
        &ids,
        &wholes,
        &parts as &[Option<f64>],
    )
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// A pending row, as the insert statement reads it out of one `jsonb` array.
#[derive(Serialize)]
struct PendingInsert<'a> {
    identity_key: String,
    title: &'a str,
    alternative_titles: &'a [String],
    source_providers: Vec<&'a str>,
    source_urls: Vec<&'a str>,
    source_paths: Vec<&'a str>,
    pinned_source: Option<usize>,
    external_trackers: Vec<&'a str>,
    external_ids: Vec<&'a str>,
    status: &'static str,
    notify: bool,
    sync_excluded: bool,
    #[serde(with = "time::serde::rfc3339")]
    added_at: OffsetDateTime,
    whole: Option<f64>,
    part: Option<f64>,
}

/// Queue `entries` as pending, replacing an earlier queued copy of the same entry.
async fn queue_pending(
    conn: &mut PgConnection,
    user_id: UserId,
    entries: &[&PortableEntry],
    options: ImportOptions,
) -> DbResult<()> {
    if entries.is_empty() {
        return Ok(());
    }
    // Deduplicated by key, last one wins: `ON CONFLICT DO UPDATE` refuses to touch one row twice
    // in a single statement.
    let mut by_key: HashMap<String, PendingInsert<'_>> = HashMap::new();
    for e in entries {
        let key = e.identity_key();
        by_key.insert(
            key.clone(),
            PendingInsert {
                identity_key: key,
                title: &e.title,
                alternative_titles: &e.alternative_titles,
                source_providers: e.sources.iter().map(|s| s.provider.as_str()).collect(),
                source_urls: e.sources.iter().map(|s| s.url.as_str()).collect(),
                source_paths: e.sources.iter().map(|s| s.path.as_str()).collect(),
                pinned_source: e.sources.iter().position(|s| s.pinned),
                external_trackers: e.external_ids.iter().map(|x| x.tracker.as_str()).collect(),
                external_ids: e.external_ids.iter().map(|x| x.id.as_str()).collect(),
                status: e.status.as_str(),
                notify: e.notify,
                sync_excluded: e.sync_excluded,
                added_at: e.added_at,
                whole: e.progress.map(|p| p.whole),
                part: e.progress.and_then(|p| p.part),
            },
        );
    }
    let rows: Vec<PendingInsert<'_>> = by_key.into_values().collect();
    sqlx::query!(
        r#"INSERT INTO watchlist_import_pending
                  (user_id, identity_key, title, alternative_titles, source_providers, source_urls,
                   source_paths, pinned_source, external_trackers, external_ids, status, notify,
                   sync_excluded, added_at, last_read_whole_number, last_read_part_number,
                   prefer_imported, match_titles)
           SELECT $1, e->>'identity_key', e->>'title',
                  ARRAY(SELECT v FROM jsonb_array_elements_text(e->'alternative_titles')
                        WITH ORDINALITY AS a(v, n) ORDER BY n),
                  ARRAY(SELECT v FROM jsonb_array_elements_text(e->'source_providers')
                        WITH ORDINALITY AS a(v, n) ORDER BY n),
                  ARRAY(SELECT v FROM jsonb_array_elements_text(e->'source_urls')
                        WITH ORDINALITY AS a(v, n) ORDER BY n),
                  ARRAY(SELECT v FROM jsonb_array_elements_text(e->'source_paths')
                        WITH ORDINALITY AS a(v, n) ORDER BY n),
                  (e->>'pinned_source')::smallint,
                  ARRAY(SELECT v FROM jsonb_array_elements_text(e->'external_trackers')
                        WITH ORDINALITY AS a(v, n) ORDER BY n),
                  ARRAY(SELECT v FROM jsonb_array_elements_text(e->'external_ids')
                        WITH ORDINALITY AS a(v, n) ORDER BY n),
                  (e->>'status')::watch_status, (e->>'notify')::boolean,
                  (e->>'sync_excluded')::boolean, LEAST((e->>'added_at')::timestamptz, now()),
                  (e->>'whole')::numeric(10,4), (e->>'part')::numeric(10,4),
                  $3, $4
             FROM jsonb_array_elements($2::jsonb) AS e
           ON CONFLICT (user_id, identity_key) DO UPDATE
              SET title = EXCLUDED.title,
                  alternative_titles = EXCLUDED.alternative_titles,
                  source_providers = EXCLUDED.source_providers,
                  source_urls = EXCLUDED.source_urls,
                  source_paths = EXCLUDED.source_paths,
                  pinned_source = EXCLUDED.pinned_source,
                  external_trackers = EXCLUDED.external_trackers,
                  external_ids = EXCLUDED.external_ids,
                  status = EXCLUDED.status,
                  notify = EXCLUDED.notify,
                  sync_excluded = EXCLUDED.sync_excluded,
                  added_at = EXCLUDED.added_at,
                  last_read_whole_number = EXCLUDED.last_read_whole_number,
                  last_read_part_number = EXCLUDED.last_read_part_number,
                  prefer_imported = EXCLUDED.prefer_imported,
                  match_titles = EXCLUDED.match_titles,
                  last_attempt_at = now()"#,
        user_id.as_uuid(),
        Json(&rows) as _,
        options.policy == ConflictPolicy::PreferImported,
        options.match_titles,
    )
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// One of a reader's pending entries, for display.
#[derive(Debug, Clone)]
pub struct PendingEntry {
    /// The entry's title.
    pub title: String,
    /// The entry's source page URLs.
    pub source_urls: Vec<String>,
    /// When it was first queued.
    pub queued_at: OffsetDateTime,
    /// When a match was last attempted.
    pub last_attempt_at: OffsetDateTime,
    /// How many matches have been attempted.
    pub attempts: i32,
}

/// `user_id`'s pending entry count, and the oldest `limit` of them.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only.
pub async fn watchlist_pending(
    pool: &PgPool,
    user_id: UserId,
    limit: i64,
) -> DbResult<(i64, Vec<PendingEntry>)> {
    let total = sqlx::query_scalar!(
        "SELECT count(*) AS \"n!\" FROM watchlist_import_pending WHERE user_id = $1",
        user_id.as_uuid(),
    )
    .fetch_one(pool)
    .await?;
    let items = sqlx::query_as!(
        PendingEntry,
        "SELECT title, source_urls, queued_at, last_attempt_at, attempts \
           FROM watchlist_import_pending WHERE user_id = $1 \
          ORDER BY queued_at, id LIMIT $2",
        user_id.as_uuid(),
        limit,
    )
    .fetch_all(pool)
    .await?;
    Ok((total, items))
}

/// Discard every pending entry `user_id` has, returning how many there were.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only.
pub async fn watchlist_pending_clear(pool: &PgPool, user_id: UserId) -> DbResult<u64> {
    Ok(sqlx::query!(
        "DELETE FROM watchlist_import_pending WHERE user_id = $1",
        user_id.as_uuid(),
    )
    .execute(pool)
    .await?
    .rows_affected())
}

/// What one pass of [`watchlist_pending_sweep`] did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SweepOutcome {
    /// Pending entries examined.
    pub examined: usize,
    /// Pending entries that matched a series and were applied.
    pub attached: usize,
}

/// A claimed pending row.
#[expect(
    clippy::struct_excessive_bools,
    reason = "mirrors the row's boolean columns one to one; `into_entry` and `SweepGroup` are where \n              they become typed choices"
)]
struct PendingRow {
    id: Uuid,
    user_id: Uuid,
    title: String,
    alternative_titles: Vec<String>,
    source_providers: Vec<String>,
    source_urls: Vec<String>,
    source_paths: Vec<String>,
    pinned_source: Option<i16>,
    external_trackers: Vec<String>,
    external_ids: Vec<String>,
    status: WatchStatus,
    notify: bool,
    sync_excluded: bool,
    added_at: OffsetDateTime,
    whole: Option<f64>,
    part: Option<f64>,
    prefer_imported: bool,
    match_titles: bool,
    adult_opt_in: bool,
}

impl PendingRow {
    fn into_entry(self) -> PortableEntry {
        let pinned = self.pinned_source.and_then(|i| usize::try_from(i).ok());
        PortableEntry {
            title: self.title,
            alternative_titles: self.alternative_titles,
            status: self.status,
            notify: self.notify,
            sync_excluded: self.sync_excluded,
            added_at: self.added_at,
            progress: self.whole.map(|whole| PortableProgress {
                whole,
                part: self.part,
            }),
            sources: self
                .source_providers
                .into_iter()
                .zip(self.source_urls)
                .zip(self.source_paths)
                .enumerate()
                .map(|(i, ((provider, url), path))| SourceRef {
                    provider,
                    url,
                    path,
                    pinned: pinned == Some(i),
                })
                .collect(),
            external_ids: self
                .external_trackers
                .into_iter()
                .zip(self.external_ids)
                .map(|(tracker, id)| ExternalRef { tracker, id })
                .collect(),
        }
    }
}

/// The pending rows one resolve-plan-apply can handle together: one reader, one set of choices.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct SweepGroup {
    user_id: Uuid,
    policy: ConflictPolicy,
    match_titles: bool,
    include_adult: bool,
}

/// Retry the `limit` least recently attempted pending entries, attaching each one whose series
/// the catalogue now has.
///
/// Rows are claimed `FOR UPDATE SKIP LOCKED` in the same transaction as the writes they cause, so
/// two sweeps cannot apply one entry twice and a failed pass leaves every row as it was. Each
/// entry is applied under the conflict policy and title matching its import asked for, with the
/// adult gate evaluated for the reader as they stand now; `adult_feature_on` is the deployment
/// half of that gate.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only.
pub async fn watchlist_pending_sweep(
    pool: &PgPool,
    limit: i64,
    adult_feature_on: bool,
) -> DbResult<SweepOutcome> {
    let mut tx = pool.begin().await?;
    let rows = sqlx::query_as!(
        PendingRow,
        r#"SELECT p.id, p.user_id, p.title, p.alternative_titles, p.source_providers,
                  p.source_urls, p.source_paths, p.pinned_source, p.external_trackers,
                  p.external_ids, p.status AS "status: WatchStatus", p.notify, p.sync_excluded,
                  p.added_at, p.last_read_whole_number::float8 AS whole,
                  p.last_read_part_number::float8 AS part, p.prefer_imported, p.match_titles,
                  u.adult_opt_in
             FROM watchlist_import_pending p
             JOIN users u ON u.id = p.user_id
            ORDER BY p.last_attempt_at, p.id
            LIMIT $1
              FOR UPDATE OF p SKIP LOCKED"#,
        limit,
    )
    .fetch_all(&mut *tx)
    .await?;

    let examined = rows.len();
    let mut groups: HashMap<SweepGroup, Vec<(Uuid, PortableEntry)>> = HashMap::new();
    for row in rows {
        let group = SweepGroup {
            user_id: row.user_id,
            policy: if row.prefer_imported {
                ConflictPolicy::PreferImported
            } else {
                ConflictPolicy::KeepLocal
            },
            match_titles: row.match_titles,
            include_adult: adult_feature_on && row.adult_opt_in,
        };
        groups
            .entry(group)
            .or_default()
            .push((row.id, row.into_entry()));
    }

    let mut attached: Vec<Uuid> = Vec::new();
    let mut retry: Vec<Uuid> = Vec::new();
    for (group, members) in groups {
        let (ids, entries): (Vec<Uuid>, Vec<PortableEntry>) = members.into_iter().unzip();
        let options = ImportOptions {
            policy: group.policy,
            remove_missing: false,
            match_titles: group.match_titles,
            include_adult: group.include_adult,
        };
        let resolved =
            resolve_entries(&mut tx, &entries, group.match_titles, group.include_adult).await?;
        plan_and_apply(
            &mut tx,
            UserId::from_uuid(group.user_id),
            &entries,
            &resolved,
            options,
        )
        .await?;
        for (id, r) in ids.into_iter().zip(&resolved) {
            if r.is_some() {
                attached.push(id);
            } else {
                retry.push(id);
            }
        }
    }

    sqlx::query!(
        "DELETE FROM watchlist_import_pending WHERE id = ANY($1)",
        &attached
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query!(
        "UPDATE watchlist_import_pending \
            SET attempts = attempts + 1, last_attempt_at = now() \
          WHERE id = ANY($1)",
        &retry
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(SweepOutcome {
        examined,
        attached: attached.len(),
    })
}
