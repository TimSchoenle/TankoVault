//! Processing one scanned series: ingest, outlier rejection and change detection.

use std::collections::HashMap;
use tankovault_adapters::{Ctx, SourceAdapter};
use tankovault_contracts::ChapterDiscovered;
use tankovault_db::repo::catalog::{ChapterUpsert, ScannedSeries, SeriesUpsert};
use tankovault_domain::{Provider, normalize_title};
use time::OffsetDateTime;

use super::{Engine, chapter_key, content_hash, drop_implausible};

impl Engine {
    /// Fetch, parse, and idempotently ingest one series; emit `chapter.discovered` for
    /// genuinely new chapters. Returns the count of new chapters.
    pub(crate) async fn process_series(
        &self,
        provider: &Provider,
        adapter: &dyn SourceAdapter,
        ctx: &Ctx,
        path: &str,
    ) -> anyhow::Result<usize> {
        let meta = adapter.fetch_series(ctx, path).await?;
        let mut chapters = adapter.fetch_chapters(ctx, path).await?;
        // Before the hash, not after: a source that keeps serving the same junk then hashes as
        // unchanged, so re-scans stay no-ops instead of re-deciding every time.
        drop_implausible(&self.outliers, provider, path, &mut chapters);
        let hash = content_hash(&meta, &chapters);

        // `meta`/`chapters` move into `scanned`, not copy: the fan-out below reads them
        // back out of `scanned`, so a 2,000-chapter series doesn't allocate a second copy
        // of every title and path.
        let normalized_title = normalize_title(&meta.title);
        let scanned = ScannedSeries {
            provider_id: provider.id,
            source_path: path.to_owned(),
            // The one surviving clone: the title is both the provider's label for this source
            // and the canonical series title, and the two are independent thereafter.
            provider_title: Some(meta.title.clone()),
            meta: SeriesUpsert {
                canonical_title: meta.title,
                normalized_title,
                description: meta.description,
                cover_url: meta.cover_url,
                content_type: meta.content_type,
                status: meta.status,
                release_year: meta.release_year,
            },
            alt_titles: meta
                .alt_titles
                .into_iter()
                .map(|t| {
                    let normalized = normalize_title(&t);
                    (t, normalized)
                })
                .collect(),
            tags: meta.tags,
            authors: meta.authors,
            chapters: chapters
                .into_iter()
                .map(|c| ChapterUpsert {
                    number: c.number,
                    volume: None,
                    title: c.title,
                    path: c.path,
                    published_at: c.published_at,
                })
                .collect(),
            content_hash: hash,
        };

        let outcome = tankovault_db::repo::catalog::ingest_series(
            &self.pool,
            &scanned,
            &self.matching,
            &self.metadata_priority,
        )
        .await?;

        if let Some(bus) = &self.bus {
            // One indexing pass, then a lookup per new chapter — an O(n) scan per new
            // number would cost a 2,000-chapter series with 50 new chapters 100,000
            // comparisons.
            let by_number: HashMap<u64, &ChapterUpsert> = scanned
                .chapters
                .iter()
                .map(|c| (chapter_key(c.number), c))
                .collect();

            for number in &outcome.new_chapters {
                if let Some(ch) = by_number.get(&chapter_key(*number)) {
                    let event = ChapterDiscovered {
                        series_id: outcome.series_id,
                        series_source_id: outcome.source_id,
                        provider_id: provider.id,
                        provider_slug: provider.slug.clone(),
                        chapter_number: ch.number,
                        chapter_title: ch.title.clone(),
                        chapter_path: ch.path.clone(),
                        published_at: ch.published_at,
                        discovered_at: OffsetDateTime::now_utc(),
                    };
                    if let Err(e) = bus.publish_chapter(&event).await {
                        tracing::warn!(error = %e, "failed to publish chapter.discovered");
                    }
                }
            }
        }

        if !outcome.new_chapters.is_empty() {
            metrics::counter!(
                "chapters_discovered_total",
                "provider" => provider.slug.clone()
            )
            .increment(outcome.new_chapters.len() as u64);
        }

        Ok(outcome.new_chapters.len())
    }
}
