//! Scan execution engine — shared by the inline (one-shot) full/fast scans and the
//! `JetStream` task consumer. Every write goes through the idempotent
//! [`tankovault_db::repo::catalog::ingest_series`], so replays are safe.

use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::hash::{Hash as _, Hasher as _};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tankovault_adapters::{ChapterMeta, Ctx, SeriesMeta, SourceAdapter, build_adapter};
use tankovault_bus::Bus;
use tankovault_config::MatchingConfig;
use tankovault_db::PgPool;
use tankovault_domain::chapter_outliers::{OutlierPolicy, implausible_indices};
use tankovault_domain::{AdultTagSet, MetadataPriority, Provider, ProviderId, TagBlocklist};
use tankovault_fetch::{Fetcher, ProviderFetchConfig, SessionStore, build_provider_fetcher};
use tankovault_solver::ChallengeSolver;

mod scans;
mod series;
mod tasks;

#[cfg(test)]
mod tests;

/// Children enqueued per statement when fanning out a catalogue page.
///
/// A catalogue "page" can be up to 20k entries (a sitemap-driven adapter like kunmanga).
/// Chunking bounds any single INSERT and lets `total_tasks` advance incrementally.
const FANOUT_CHUNK: usize = 1_000;

/// Shared dependencies handed to the engine.
#[derive(Clone)]
pub(crate) struct Engine {
    pub(crate) pool: PgPool,
    pub(crate) bus: Option<Bus>,
    pub(crate) solver: Arc<dyn ChallengeSolver>,
    pub(crate) session_store: Arc<dyn SessionStore>,
    pub(crate) worker_id: String,
    /// Safety cap on catalogue pages walked per full scan.
    pub(crate) max_catalog_pages: u32,
    /// The confidence policy for canonicalising a scanned series onto an existing one.
    ///
    /// Held here, not defaulted in the repository, so this path and external sync answer
    /// "is this the same series?" the same way.
    pub(crate) matching: MatchingConfig,
    /// Which source owns each metadata field a scan also supplies.
    ///
    /// Held here for the same reason as [`Self::matching`]: sync writes these columns too, and
    /// a priority only one writer consults is last-writer-wins with extra steps.
    pub(crate) metadata_priority: MetadataPriority,
    /// Which scraped "genres" are not tags at all. Held here for the same reason as
    /// [`Self::metadata_priority`]: sync's enrichment writer interns into the same `tags`
    /// vocabulary, and a guard only one writer applies is not a guard.
    pub(crate) tag_blocklist: TagBlocklist,
    /// Which scraped "genres" classify a series as adult, for the series `AniList` never matches.
    pub(crate) adult_tags: AdultTagSet,
    /// Which scraped chapter numbers the source cannot plausibly have released.
    pub(crate) outliers: OutlierPolicy,
    /// One fetch stack per provider, keyed by the politeness settings it was built from.
    ///
    /// Load-bearing for correctness, not just speed: the rate limiter and adaptive 429
    /// penalty live on the fetcher, so building one per task turns `rps`/`concurrency`
    /// into a per-task budget instead of per-provider, and N concurrent tasks offer N ×
    /// rps to the provider.
    fetchers: Arc<Mutex<HashMap<ProviderId, CachedFetcher>>>,
}

/// Hash the provider settings a fetch stack is built from.
///
/// Only the inputs to `build_provider_fetcher`, so an unrelated column change doesn't
/// discard a warm connection pool, while lowering `rps` takes effect on the next task.
fn politeness_fingerprint(provider: &Provider) -> u64 {
    let p = &provider.politeness;
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    // `f64` is not `Hash`; its bit pattern is, and equality of bit patterns is exactly the
    // "unchanged" test wanted here.
    p.rps.to_bits().hash(&mut hasher);
    p.concurrency.hash(&mut hasher);
    p.crawl_delay_ms.hash(&mut hasher);
    p.user_agent.hash(&mut hasher);
    format!("{:?}", p.emulation).hash(&mut hasher);
    hasher.finish()
}

/// The lookup key for a chapter number, pairing a newly-inserted number back to the parsed
/// chapter it came from.
///
/// Keyed on the bit pattern, not a tolerance comparison, matching the old
/// `(a - b).abs() < f64::EPSILON` test for every value a chapter number can take. The one
/// disagreement is `-0.0`, normalised here since chapter 0 (prologues) is real. `NaN`
/// cannot arrive: `parse_number` rejects non-finite values.
fn chapter_key(number: f64) -> u64 {
    if number == 0.0 { 0.0_f64 } else { number }.to_bits()
}

/// A built fetch stack plus the fingerprint of the settings that produced it.
struct CachedFetcher {
    /// Politeness + base URL as the provider row had them when this stack was built. An
    /// operator lowering `rps` mid-run must take effect, so the entry is rebuilt on change
    /// rather than pinned for the process lifetime.
    fingerprint: u64,
    fetcher: Arc<dyn Fetcher>,
}

/// The tunables an engine scans under, grouped so they travel together rather than as a
/// widening argument list.
pub(crate) struct EngineSettings {
    pub(crate) max_catalog_pages: u32,
    pub(crate) matching: MatchingConfig,
    pub(crate) metadata_priority: MetadataPriority,
    pub(crate) tag_blocklist: TagBlocklist,
    pub(crate) adult_tags: AdultTagSet,
    pub(crate) outliers: OutlierPolicy,
}

impl Engine {
    /// Assemble the engine with an empty fetcher cache.
    ///
    /// A constructor, not a struct literal, so [`Engine::fetchers`] stays an
    /// implementation detail callers can't construct pre-populated.
    pub(crate) fn new(
        pool: PgPool,
        bus: Option<Bus>,
        solver: Arc<dyn ChallengeSolver>,
        session_store: Arc<dyn SessionStore>,
        worker_id: String,
        settings: EngineSettings,
    ) -> Self {
        Self {
            pool,
            bus,
            solver,
            session_store,
            worker_id,
            max_catalog_pages: settings.max_catalog_pages,
            matching: settings.matching,
            metadata_priority: settings.metadata_priority,
            tag_blocklist: settings.tag_blocklist,
            adult_tags: settings.adult_tags,
            outliers: settings.outliers,
            fetchers: Arc::default(),
        }
    }

    /// Build the fetch stack for `provider`, reusing the cached one when its settings are
    /// unchanged.
    fn fetcher_for(&self, provider: &Provider) -> anyhow::Result<Arc<dyn Fetcher>> {
        let fingerprint = politeness_fingerprint(provider);

        // No await point held here, so a `std` mutex is right; `tokio::sync::RwLock`
        // would make `provider_context` async for no benefit.
        {
            let cache = self.fetchers.lock().expect("fetcher cache mutex poisoned");
            if let Some(entry) = cache.get(&provider.id)
                && entry.fingerprint == fingerprint
            {
                return Ok(Arc::clone(&entry.fetcher));
            }
        }

        let mut fetch_cfg = ProviderFetchConfig::new(
            provider.politeness.user_agent.clone(),
            self.solver.clone(),
            self.session_store.clone(),
        );
        fetch_cfg.emulation = provider.politeness.emulation;
        fetch_cfg.rps = provider.politeness.rps;
        fetch_cfg.concurrency = provider.politeness.concurrency;
        fetch_cfg.crawl_delay_ms = provider.politeness.crawl_delay_ms;
        fetch_cfg.connect_timeout = Duration::from_secs(10);
        fetch_cfg.request_timeout = Duration::from_secs(30);
        let fetcher = build_provider_fetcher(fetch_cfg)?;

        let mut cache = self.fetchers.lock().expect("fetcher cache mutex poisoned");
        // Another task may have built one concurrently. Either is correct, but the stored
        // entry must be shared, so only insert if it's still absent or stale.
        let entry = cache
            .entry(provider.id)
            .and_modify(|existing| {
                if existing.fingerprint != fingerprint {
                    *existing = CachedFetcher {
                        fingerprint,
                        fetcher: Arc::clone(&fetcher),
                    };
                }
            })
            .or_insert_with(|| CachedFetcher {
                fingerprint,
                fetcher: Arc::clone(&fetcher),
            });
        Ok(Arc::clone(&entry.fetcher))
    }

    /// Build the per-provider adapter + injected fetch stack + context.
    ///
    /// The adapter is rebuilt per call (cheap, stateless); the fetch stack is not — it
    /// carries the rate limiter and throttle penalty, which must be shared across a
    /// provider's tasks to mean anything.
    pub(crate) fn provider_context(
        &self,
        provider: &Provider,
    ) -> anyhow::Result<(Box<dyn SourceAdapter>, Ctx)> {
        let adapter = build_adapter(provider.adapter, &provider.slug, &provider.config)?;
        let ctx = Ctx {
            base_url: provider.base_url.clone(),
            provider_slug: provider.slug.clone(),
            fetcher: self.fetcher_for(provider)?,
        };
        Ok((adapter, ctx))
    }
}

/// Summary of an inline scan.
#[derive(Debug, Default)]
pub(crate) struct ScanSummary {
    pub(crate) series_seen: usize,
    pub(crate) series_failed: usize,
    pub(crate) new_chapters: usize,
}

/// Drop chapter entries whose numbers the source cannot plausibly have released.
///
/// These are the source's own slugs, not a parse failure — `chapter-180302` (a date),
/// `chapter-2025` (a year), `chapter-2099` (a number lifted out of the series title). Left in,
/// one of them becomes the series' latest chapter and every reader's progress against it reads
/// as hundreds of chapters behind.
fn drop_implausible(
    policy: &OutlierPolicy,
    provider: &Provider,
    path: &str,
    chapters: &mut Vec<ChapterMeta>,
) {
    let numbers: Vec<f64> = chapters.iter().map(|c| c.number).collect();
    let rejected = implausible_indices(&numbers, policy);
    if rejected.is_empty() {
        return;
    }

    // Logged with the numbers, not just a count: this is the only record that a chapter was
    // skipped, and an operator judging whether the thresholds are right needs to see them.
    tracing::warn!(
        provider = %provider.slug,
        series_path = %path,
        rejected = rejected.len(),
        of = chapters.len(),
        numbers = ?rejected.iter().map(|&i| numbers[i]).collect::<Vec<_>>(),
        "skipping implausible chapter numbers"
    );
    metrics::counter!("chapters_rejected_total", "provider" => provider.slug.clone())
        .increment(rejected.len() as u64);

    // `rejected` is ascending and `retain` visits in order, so the running index tracks the
    // original positions those indices refer to.
    let mut index = 0usize;
    chapters.retain(|_| {
        let keep = rejected.binary_search(&index).is_err();
        index += 1;
        keep
    });
}

/// Content hash over title + chapter (number, path) pairs, for cheap change detection.
/// Record that a catalogue walk stopped at the page budget with pages still unread.
///
/// The warning beside each call site is the only other trace, and it has already been missed
/// once in practice: a large provider was silently truncated for as long as nobody grepped.
fn catalog_truncated(provider_slug: &str) {
    metrics::counter!(
        "scan_catalog_pages_truncated_total",
        "provider" => provider_slug.to_owned()
    )
    .increment(1);
}

fn content_hash(meta: &SeriesMeta, chapters: &[ChapterMeta]) -> Vec<u8> {
    let mut h = Sha256::new();
    h.update(meta.title.as_bytes());
    if let Some(desc) = &meta.description {
        h.update(desc.as_bytes());
    }
    for c in chapters {
        h.update(c.number.to_le_bytes());
        h.update(b"|");
        h.update(c.path.as_bytes());
        h.update(b"\n");
    }
    h.finalize().to_vec()
}
