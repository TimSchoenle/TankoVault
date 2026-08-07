//! Undo, in the catalogue, what the alias over-attach did before the matcher stopped doing it.
//!
//! The scan-time canonicaliser used to treat an exact hit on *any* alternative title as identity,
//! so a source attached to a series on a shredded sentence fragment or a shared `(Colored)` label.
//! Attaching also files that source's own alternative titles under the series it joined, so each
//! wrong attach widened the net for the next one. On the catalogue this was diagnosed against, one
//! row had accumulated 309 sources across three providers and 1 140 alternative titles.
//!
//! `tankovault_matcher` and `find_candidates` stop it happening again; nothing in the normal path
//! undoes what is already stored, because ingest only ever inserts and updates. This does, in
//! three passes that are deliberately ordered:
//!
//! 1. **Keys.** Re-derive every stored normalized key from the title beside it. Keys are
//!    persisted, so a series renamed by a past writer that did not rewrite its key is still
//!    matched under the name it no longer goes by.
//! 2. **Fan-out.** Drop alternative titles whose key more than `MAX_KEY_FANOUT` distinct series
//!    answer to. A name that many works share identifies none of them, and it is the standing
//!    invitation the next attach accepts. Safe in the direction that matters: a legitimately
//!    shared title is re-inserted by the next scan of each series, a shredded fragment is not.
//! 3. **Report.** Rank the series whose sources the matcher would not attach today, worst first,
//!    asking `tankovault_matcher` — the scorer the ingest path asks — rather than a similarity
//!    rule of this tool's own, which would drift away from it. A source is listed when its
//!    provider title scores below `Thresholds::low` against the series it is filed under, the
//!    band where the matcher would have created a separate series rather than even queueing the
//!    pair.
//!
//! Passes 1 and 2 write under `--apply`. **Pass 3 never writes on its own**, and the reason is
//! evidence rather than caution: a low score against today's canonical title is not proof of a
//! bad attach, because a past writer renamed series without rewriting their keys, so a source
//! that attached correctly to "Ubel Blatt II" now scores against "Ubel Blatt II: Knights of the
//! Fallen King" and fails. Measured on the catalogue this was written against, the sources-per-
//! provider distribution decays smoothly from 48 202 series with one to 26 with more than ten —
//! there is no cut-off separating the pathology from a `(Colored)` edition, and a blanket sweep
//! shatters the catalogue in the other direction to fix a minority. So an operator reads the
//! ranking and names the series to split:
//!
//! ```text
//! cargo run -p xtask -- repair-series                       # report
//! cargo run -p xtask -- repair-series --apply               # passes 1 and 2
//! cargo run -p xtask -- repair-series --split <uuid> --apply
//! ```
//!
//! # What a split deliberately does not move
//!
//! `watchlist_entries` and `read_progress` key on `series_id`, and they stay on the surviving
//! series. That is the right default — a user who tracked the row tracked the work it is named
//! after, and the sources leaving are the ones that never belonged — but it means progress a user
//! recorded against a chapter that moves away stays behind. Re-attributing it is a judgement this
//! tool does not make.

use std::collections::BTreeMap;
use tankovault_domain::matching::{Candidate, Query};
use tankovault_domain::{ContentType, SeriesId, normalize_title};
use tankovault_matcher::{Thresholds, assess};
use uuid::Uuid;

/// The same ceiling `find_candidates` and `find_duplicate_pairs` apply; see
/// `crates/db/src/repo/matching/mod.rs`, which is where it is argued.
const MAX_KEY_FANOUT: i64 = 16;

/// One source that does not belong to the series it is filed under.
struct Stray {
    source_id: Uuid,
    provider: String,
    provider_title: String,
    chapters: i64,
}

/// One series and the sources filed under it, as the scoring loop needs to see them.
struct Filed {
    canonical_title: String,
    sources: Vec<Stray>,
}

/// A series holding sources for more than one work.
struct Split {
    series_id: Uuid,
    canonical_title: String,
    /// Total sources and alternative titles on the row — the two numbers that tell an operator
    /// whether they are looking at a `(Colored)` edition or at a runaway.
    sources: usize,
    aliases: usize,
    /// Keyed by the normalized provider title, so every source of one work moves together.
    strays: BTreeMap<String, Vec<Stray>>,
}

/// Report — or with `apply`, perform — the repair passes; with `split`, detach one named series'
/// strays instead.
///
/// # Errors
/// Any database failure, or a `split` id that names no series. Under `apply` each pass is its own
/// transaction, so an interrupted run leaves the catalogue at a pass boundary rather than
/// part-way through one.
pub(crate) async fn run(
    pool: &tankovault_db::PgPool,
    apply: bool,
    split: Option<Uuid>,
) -> anyhow::Result<()> {
    if let Some(series_id) = split {
        let Some(found) = find_strays(pool, Some(series_id)).await?.pop() else {
            anyhow::bail!("{series_id} holds no source the matcher would refuse to attach");
        };
        report(std::slice::from_ref(&found));
        if apply {
            detach(pool, &found).await?;
        } else {
            println!("\ndry run; nothing written. Re-run with --apply.");
        }
        return Ok(());
    }

    rebuild_keys(pool, apply).await?;
    drop_overshared_aliases(pool, apply).await?;
    let strays = find_strays(pool, None).await?;
    report(&strays);
    if !apply {
        println!("\ndry run; nothing written. Re-run with --apply.");
    }
    Ok(())
}

/// Pass 1: re-derive `series.normalized_title` and `series_titles.normalized`.
async fn rebuild_keys(pool: &tankovault_db::PgPool, apply: bool) -> anyhow::Result<()> {
    let stale: i64 = sqlx::query_scalar(
        "SELECT (SELECT count(*) FROM series WHERE normalized_title \
                   IS DISTINCT FROM tv_normalize_title(canonical_title)) \
              + (SELECT count(*) FROM series_titles WHERE normalized \
                   IS DISTINCT FROM tv_normalize_title(title))",
    )
    .fetch_one(pool)
    .await?;
    println!("keys: {stale} rows carry a key their title no longer produces");
    if !apply || stale == 0 {
        return Ok(());
    }
    let report = tankovault_db::repo::matching::rebuild_normalized_keys(pool, normalize_title)
        .await
        .map_err(anyhow::Error::from)?;
    println!(
        "keys: rewrote {} series and {} alternative titles, dropped {} that collided",
        report.series_updated, report.titles_updated, report.titles_deduplicated
    );
    Ok(())
}

/// Pass 2: drop alternative titles too many series answer to.
async fn drop_overshared_aliases(pool: &tankovault_db::PgPool, apply: bool) -> anyhow::Result<()> {
    let rows: Vec<(String, i64)> = sqlx::query_as(
        "SELECT normalized, count(*) AS held \
           FROM series_titles WHERE normalized <> '' \
          GROUP BY normalized HAVING count(*) > $1 \
          ORDER BY held DESC",
    )
    .bind(MAX_KEY_FANOUT)
    .fetch_all(pool)
    .await?;
    if rows.is_empty() {
        println!("aliases: none held by more than {MAX_KEY_FANOUT} series");
        return Ok(());
    }
    let total: i64 = rows.iter().map(|(_, held)| held).sum();
    for (key, held) in rows.iter().take(20) {
        println!("    {held:>6}  {key}");
    }
    println!(
        "aliases: {total} rows across {} keys held by more than {MAX_KEY_FANOUT} series",
        rows.len()
    );
    if !apply {
        return Ok(());
    }
    let deleted = sqlx::query(
        "DELETE FROM series_titles WHERE normalized IN ( \
           SELECT normalized FROM series_titles WHERE normalized <> '' \
            GROUP BY normalized HAVING count(*) > $1)",
    )
    .bind(MAX_KEY_FANOUT)
    .execute(pool)
    .await?
    .rows_affected();
    println!("aliases: deleted {deleted} rows");
    Ok(())
}

/// Print the ranking, worst first, and the totals under it.
fn report(splits: &[Split]) {
    if splits.is_empty() {
        println!("sources: every source scores as belonging to the series it is filed under");
        return;
    }
    let moved: usize = splits
        .iter()
        .map(|s| s.strays.values().flatten().count())
        .sum();
    for split in splits.iter().take(20) {
        println!(
            "{} ({}) — {} sources, {} alternative titles",
            split.canonical_title, split.series_id, split.sources, split.aliases
        );
        for (key, strays) in &split.strays {
            let chapters: i64 = strays.iter().map(|s| s.chapters).sum();
            println!(
                "    -> {key:?} ({} source(s), {chapters} chapter(s)): {} {}",
                strays.len(),
                strays[0].provider,
                strays[0].provider_title
            );
            for stray in strays.iter().skip(1) {
                println!("       {} {}", stray.provider, stray.provider_title);
            }
        }
    }
    println!(
        "sources: {moved} across {} series score below the review floor against the series they \
         are filed under. Read the list, then split one with `--split <series-id> --apply`.",
        splits.len()
    );
}

/// Move one series' strays onto series of their own, grouped so every source of one work lands
/// together.
async fn detach(pool: &tankovault_db::PgPool, split: &Split) -> anyhow::Result<()> {
    let mut tx = pool.begin().await?;
    for (key, strays) in &split.strays {
        let id = Uuid::now_v7();
        sqlx::query(
            "INSERT INTO series (id, canonical_title, normalized_title) VALUES ($1, $2, $3)",
        )
        .bind(id)
        .bind(&strays[0].provider_title)
        .bind(key)
        .execute(&mut *tx)
        .await?;
        let ids: Vec<Uuid> = strays.iter().map(|s| s.source_id).collect();
        // Chapters hang off `series_sources`, so they travel with the row; the watchlist and
        // progress rows key on `series_id` and deliberately do not (see the module comment).
        sqlx::query("UPDATE series_sources SET series_id = $1 WHERE id = ANY($2)")
            .bind(id)
            .bind(&ids)
            .execute(&mut *tx)
            .await?;
        println!("split: {} source(s) -> {id} {key:?}", strays.len());
    }
    tx.commit().await?;
    Ok(())
}

/// Every series holding a source the matcher would not have attached, worst first. `only` narrows
/// it to one series.
///
/// Keys are re-derived here rather than read: pass 1 rewrites them, and in a dry run it has not,
/// so reading `series.normalized_title` would judge half the catalogue against a name it no
/// longer goes by and report a split for every stale key.
async fn find_strays(
    pool: &tankovault_db::PgPool,
    only: Option<Uuid>,
) -> anyhow::Result<Vec<Split>> {
    let rows: Vec<(Uuid, String, Uuid, String, Option<String>, i64)> = sqlx::query_as(
        "SELECT s.id, s.canonical_title, ss.id, p.slug, ss.provider_title, \
                (SELECT count(*) FROM chapters c WHERE c.series_source_id = ss.id) \
           FROM series s \
           JOIN series_sources ss ON ss.series_id = s.id \
           JOIN providers p ON p.id = ss.provider_id \
          WHERE ($1::uuid IS NULL OR s.id = $1) \
            AND s.id IN (SELECT series_id FROM series_sources GROUP BY series_id \
                          HAVING count(*) > 1) \
          ORDER BY s.id",
    )
    .bind(only)
    .fetch_all(pool)
    .await?;

    let aliases: Vec<(Uuid, Vec<String>)> = sqlx::query_as(
        "SELECT series_id, array_agg(tv_normalize_title(title)) FROM series_titles \
          GROUP BY series_id",
    )
    .fetch_all(pool)
    .await?;
    let aliases: BTreeMap<Uuid, Vec<String>> = aliases.into_iter().collect();

    let mut by_series: BTreeMap<Uuid, Filed> = BTreeMap::new();
    for (series_id, canonical_title, source_id, provider, title, chapters) in rows {
        // A source with no provider title says nothing about which work it is, so it cannot be
        // the evidence that moves it: it stays wherever it is filed.
        let Some(provider_title) = title else {
            continue;
        };
        by_series
            .entry(series_id)
            .or_insert_with(|| Filed {
                canonical_title,
                sources: Vec::new(),
            })
            .sources
            .push(Stray {
                source_id,
                provider,
                provider_title,
                chapters,
            });
    }

    let thresholds = Thresholds::default();
    let mut splits = Vec::new();
    for (
        series_id,
        Filed {
            canonical_title,
            sources,
        },
    ) in by_series
    {
        // Everything the ingest path would have known about the series at attach time, and
        // nothing it would not: a provider title carries no medium, year, credit or genre, so
        // scoring here is purely textual — which is the conservative reading, since no
        // corroborating signal can lift a coincidence over the floor either.
        // The series' own alternative titles count as evidence in the survey, so a romaji listing
        // and a punctuation variant are not reported as strays. They are dropped once an operator
        // names a series to split, and that is the whole content of naming it: on a row that
        // absorbed 309 sources the 1 140 aliases it accumulated *are* the damage, and scoring
        // against them hides every source they pulled in.
        let alt_normalized_titles = if only.is_some() {
            Vec::new()
        } else {
            aliases.get(&series_id).cloned().unwrap_or_default()
        };
        let candidate = Candidate {
            series_id: SeriesId::from_uuid(series_id),
            normalized_title: normalize_title(&canonical_title),
            similarity: 0.0,
            alt_normalized_titles,
            content_type: ContentType::Unknown,
            release_year: None,
            tags: Vec::new(),
            authors: Vec::new(),
        };

        let source_count = sources.len();
        let mut groups: BTreeMap<String, Vec<Stray>> = BTreeMap::new();
        for source in sources {
            let normalized = normalize_title(&source.provider_title);
            let query = Query {
                normalized_title: normalized.clone(),
                content_type: ContentType::Unknown,
                release_year: None,
                tags: Vec::new(),
                authors: Vec::new(),
            };
            // Below the review floor is the one unambiguous verdict: the matcher would have
            // created a separate series rather than even queueing the pair. The review band is
            // left alone — a punctuation variant, a romaji listing and a `(Colored)` edition all
            // score there, and moving them would shatter the catalogue to fix a minority.
            if assess(&query, &candidate).score >= thresholds.low {
                continue;
            }
            groups.entry(normalized).or_default().push(source);
        }
        if groups.is_empty() {
            continue;
        }
        splits.push(Split {
            series_id,
            canonical_title,
            sources: source_count,
            aliases: aliases.get(&series_id).map_or(0, Vec::len),
            strays: groups,
        });
    }
    // Worst first: the pathology this exists for is a handful of rows that absorbed hundreds of
    // sources, and an operator reading a truncated list needs those at the top of it.
    splits.sort_by_key(|s| std::cmp::Reverse(s.strays.values().flatten().count()));
    Ok(splits)
}
