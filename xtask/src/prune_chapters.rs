//! Retroactive removal of chapter numbers a source cannot plausibly have released.
//!
//! The scan engine refuses these at ingest, but ingest only ever inserts and updates — nothing
//! in the normal path deletes a chapter — so junk indexed before the guard existed stays
//! indexed. This applies the same rule to what is already stored.
//!
//! Reports by default and writes only under `--apply`, because the judgement is statistical:
//! an operator should read the list before it becomes a `DELETE`.

use std::collections::BTreeMap;
use tankovault_domain::chapter_outliers::{OutlierPolicy, implausible_indices};
use uuid::Uuid;

/// One stored chapter, as the rule needs to see it.
struct Row {
    id: Uuid,
    number: f64,
    path: String,
}

/// A source with at least one implausible chapter.
struct Finding {
    source_id: Uuid,
    provider: String,
    source_path: String,
    /// Highest chapter the rule keeps — the context that makes a rejection judgeable.
    last_kept: f64,
    rejected: Vec<Row>,
}

/// Report — or with `apply`, delete — every stored chapter the outlier rule rejects.
///
/// Uses [`OutlierPolicy::default`] rather than the worker's configured policy: xtask does not
/// load a service configuration, and an operator who has tuned `chapter_outliers.*` away from
/// the defaults should expect this to disagree with their workers.
///
/// # Errors
/// Any database failure. Under `apply` the deletes run in one transaction, so a failure
/// part-way leaves the catalogue as it was.
pub(crate) async fn run(pool: &tankovault_db::PgPool, apply: bool) -> anyhow::Result<()> {
    let findings = scan(pool).await?;

    if findings.is_empty() {
        println!("no implausible chapters found");
        return Ok(());
    }

    let total: usize = findings.iter().map(|f| f.rejected.len()).sum();
    for finding in &findings {
        println!(
            "{} {} (keeps up to {})",
            finding.provider, finding.source_path, finding.last_kept
        );
        for row in &finding.rejected {
            println!("    {:>12}  {}", row.number, row.path);
        }
    }
    println!("\n{total} chapters across {} sources", findings.len());

    if !apply {
        println!("dry run; nothing deleted. Re-run with --apply to delete these rows.");
        return Ok(());
    }

    let ids: Vec<Uuid> = findings
        .iter()
        .flat_map(|f| f.rejected.iter().map(|r| r.id))
        .collect();

    // One transaction and one statement: a partial prune would leave the catalogue in a state
    // no re-run reproduces, since the rule's verdict depends on the numbers still present.
    let mut tx = pool.begin().await?;
    let deleted = sqlx::query("DELETE FROM chapters WHERE id = ANY($1)")
        .bind(&ids)
        .execute(&mut *tx)
        .await?
        .rows_affected();
    // `chapter_count` is a stored per-source total; leaving it is a silent inconsistency the
    // next scan would only fix for sources that happen to be re-scanned.
    sqlx::query(
        "UPDATE series_sources s \
            SET chapter_count = (SELECT count(*) FROM chapters c WHERE c.series_source_id = s.id) \
          WHERE s.id = ANY($1)",
    )
    .bind(findings.iter().map(|f| f.source_id).collect::<Vec<_>>())
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;

    println!("deleted {deleted} chapters");
    println!(
        "note: read_progress stores chapter *numbers*, not ids, so a reader who marked one of \
         these as read keeps that number as their progress."
    );
    Ok(())
}

/// Every source whose stored chapter numbers the rule rejects, ordered by provider and path.
async fn scan(pool: &tankovault_db::PgPool) -> anyhow::Result<Vec<Finding>> {
    let rows: Vec<(Uuid, String, String, f64, Uuid, String)> = sqlx::query_as(
        "SELECT ss.id, p.slug, ss.source_path, c.number::float8, c.id, c.path \
           FROM chapters c \
           JOIN series_sources ss ON ss.id = c.series_source_id \
           JOIN providers p ON p.id = ss.provider_id \
          ORDER BY p.slug, ss.source_path, c.number",
    )
    .fetch_all(pool)
    .await?;

    let mut by_source: BTreeMap<Uuid, (String, String, Vec<Row>)> = BTreeMap::new();
    for (source_id, provider, source_path, number, chapter_id, path) in rows {
        by_source
            .entry(source_id)
            .or_insert_with(|| (provider, source_path, Vec::new()))
            .2
            .push(Row {
                id: chapter_id,
                number,
                path,
            });
    }

    let policy = OutlierPolicy::default();
    let mut findings = Vec::new();
    for (source_id, (provider, source_path, chapters)) in by_source {
        let numbers: Vec<f64> = chapters.iter().map(|c| c.number).collect();
        let rejected = implausible_indices(&numbers, &policy);
        if rejected.is_empty() {
            continue;
        }
        // The rows arrive ordered by number, so the entry below the first rejection is the
        // highest one kept.
        let last_kept = numbers[rejected[0].saturating_sub(1)];
        let mut rejected_rows = Vec::new();
        for (index, row) in chapters.into_iter().enumerate() {
            if rejected.binary_search(&index).is_ok() {
                rejected_rows.push(row);
            }
        }
        findings.push(Finding {
            source_id,
            provider,
            source_path,
            last_kept,
            rejected: rejected_rows,
        });
    }

    findings.sort_by(|a, b| {
        a.provider
            .cmp(&b.provider)
            .then_with(|| a.source_path.cmp(&b.source_path))
    });
    Ok(findings)
}
