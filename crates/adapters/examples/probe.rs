//! Live provider probe: runs a preset against the real site through the production fetch stack
//! and prints what a fast scan and a bounded slice of a full scan would actually ingest.
//!
//! An example rather than a test because it needs the public internet and a running TRAWL
//! container (`TANKOVAULT_PROBE_SOLVER`, default `http://127.0.0.1:8191`). It is the tool that
//! derives and verifies every selector in [`presets`](tankovault_adapters::presets): a preset
//! whose fast scan reads an empty feed, or whose catalogue walk never terminates, fails here
//! rather than in production, where an empty parse is a valid answer nothing alerts on.
//!
//! ```text
//! cargo run -p tankovault-adapters --example probe -- <slug> [--page N] [--series N]
//! cargo run -p tankovault-adapters --example probe -- <slug> --config candidate.json \
//!     --base-url https://example.org --adapter madara
//! cargo run -p tankovault-adapters --example probe -- <slug> --dump /manga/some-series/
//! ```

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use time::OffsetDateTime;

use tankovault_adapters::{Ctx, SourceAdapter, build_adapter, builtin_presets};
use tankovault_domain::{AdapterKind, Politeness};
use tankovault_fetch::{InMemorySessionStore, ProviderFetchConfig, build_provider_fetcher};
use tankovault_solver::TrawlSolver;

type Failure = Box<dyn std::error::Error>;

const USAGE: &str = "usage: probe <slug> [--config <file>] [--base-url <url>] \
                     [--adapter madara|mangathemesia|manganato|keyoapp|generic_config|custom] \
                     [--page <n>] [--series <n>] [--walk <n>] [--chapters] [--dump <path>]";

/// What to probe: a shipped preset by slug, or a candidate config not yet committed to one.
struct Target {
    slug: String,
    base_url: String,
    adapter: AdapterKind,
    config: serde_json::Value,
    politeness: Politeness,
}

struct Args {
    target: Target,
    /// Catalogue page to walk (full-scan slice).
    page: u32,
    /// How many catalogue items to fetch series metadata and chapters for.
    series: usize,
    /// Print every chapter rather than the first and last few.
    all_chapters: bool,
    /// Walk the catalogue for this many consecutive pages instead of sampling one.
    walk: Option<u32>,
    /// Fetch this relative path and print the body instead of probing (selector recon).
    dump: Option<String>,
}

fn main() -> Result<(), Failure> {
    let args = parse_args()?;
    tokio::runtime::Runtime::new()?.block_on(run(args))
}

fn parse_args() -> Result<Args, Failure> {
    let mut rest = std::env::args().skip(1);
    let slug = rest.next().ok_or(USAGE)?;
    let (mut config, mut base_url, mut adapter) = (None::<PathBuf>, None::<String>, None::<String>);
    let (mut page, mut series, mut all_chapters, mut dump) = (1, 2, false, None);
    let mut walk = None;

    while let Some(flag) = rest.next() {
        let mut value = || rest.next().ok_or_else(|| format!("{flag} needs a value"));
        match flag.as_str() {
            "--config" => config = Some(PathBuf::from(value()?)),
            "--base-url" => base_url = Some(value()?),
            "--adapter" => adapter = Some(value()?),
            "--page" => page = value()?.parse()?,
            "--series" => series = value()?.parse()?,
            "--walk" => walk = Some(value()?.parse()?),
            "--chapters" => all_chapters = true,
            "--dump" => dump = Some(value()?),
            other => return Err(format!("unknown flag {other}\n{USAGE}").into()),
        }
    }

    let target = match config {
        Some(path) => Target {
            base_url: base_url.ok_or("--config needs --base-url")?,
            adapter: adapter
                .as_deref()
                .map_or(Ok(AdapterKind::GenericConfig), parse_kind)?,
            config: serde_json::from_str(&std::fs::read_to_string(path)?)?,
            politeness: Politeness::default(),
            slug,
        },
        None => shipped(&slug)?,
    };
    Ok(Args {
        target,
        page,
        series,
        all_chapters,
        walk,
        dump,
    })
}

fn parse_kind(name: &str) -> Result<AdapterKind, Failure> {
    match name {
        "madara" => Ok(AdapterKind::Madara),
        "mangathemesia" => Ok(AdapterKind::MangaThemesia),
        "manganato" => Ok(AdapterKind::Manganato),
        "keyoapp" => Ok(AdapterKind::Keyoapp),
        "generic_config" => Ok(AdapterKind::GenericConfig),
        "custom" => Ok(AdapterKind::Custom),
        other => Err(format!("unknown adapter kind {other}").into()),
    }
}

/// The shipped preset for `slug`, as a probe target.
fn shipped(slug: &str) -> Result<Target, Failure> {
    let preset = builtin_presets()
        .into_iter()
        .find(|p| p.slug == slug)
        .ok_or_else(|| format!("no shipped preset named {slug}"))?;
    Ok(Target {
        slug: preset.slug.to_owned(),
        base_url: preset.base_url.to_owned(),
        adapter: preset.adapter,
        config: preset.config,
        politeness: preset.politeness,
    })
}

/// The provider's real fetch stack, at the preset's own crawl budget.
fn context(target: &Target) -> Result<Ctx, Failure> {
    let endpoint = std::env::var("TANKOVAULT_PROBE_SOLVER")
        .unwrap_or_else(|_| "http://127.0.0.1:8191".to_owned());
    let solver = Arc::new(TrawlSolver::new(endpoint, 90_000, 1_800));
    let politeness = target.politeness.clone().clamped();
    let mut cfg = ProviderFetchConfig::new(
        politeness.user_agent.clone(),
        solver,
        Arc::new(InMemorySessionStore::default()),
    );
    cfg.emulation = politeness.emulation;
    cfg.rps = politeness.rps;
    cfg.concurrency = politeness.concurrency;
    cfg.crawl_delay_ms = politeness.crawl_delay_ms;
    // A cold solve is a full browser navigation; the service defaults are sized for a warm one.
    cfg.request_timeout = Duration::from_secs(120);
    Ok(Ctx {
        base_url: target.base_url.clone(),
        provider_slug: target.slug.clone(),
        fetcher: build_provider_fetcher(cfg)?,
    })
}

async fn run(args: Args) -> Result<(), Failure> {
    let adapter = build_adapter(args.target.adapter, &args.target.slug, &args.target.config)?;
    let ctx = context(&args.target)?;

    if let Some(path) = args.dump {
        println!("{}", ctx.fetch(&path).await?.body);
        return Ok(());
    }

    println!("== {} ({})", args.target.slug, args.target.base_url);
    let feed_head = fast_scan(adapter.as_ref(), &ctx).await;
    match args.walk {
        Some(pages) => catalogue_walk(adapter.as_ref(), &ctx, pages).await,
        None => full_scan(adapter.as_ref(), &ctx, args.page, args.series).await,
    }

    // The fast scan's own paths are the ones a fast scan ingests, and they are the paths most
    // likely to be wrong (a feed that lists chapter URLs parses perfectly and stores series
    // that can never have chapters). Walk one of them as a series, not just the catalogue's.
    if let Some(path) = feed_head {
        println!("\n-- feed item as a series");
        series_report(adapter.as_ref(), &ctx, &path, args.all_chapters).await;
    }
    Ok(())
}

/// Probe the latest-updates feed; returns the first item's path.
async fn fast_scan(adapter: &dyn SourceAdapter, ctx: &Ctx) -> Option<String> {
    println!("\n-- fast scan (list_latest)");
    match adapter.list_latest(ctx).await {
        Ok(items) => {
            println!("   {} items", items.len());
            for item in items.iter().take(5) {
                println!(
                    "   {:<52} ch {:<8} {}",
                    truncate(&item.path, 52),
                    item.latest_chapter,
                    truncate(&item.title, 48)
                );
            }
            items.first().map(|i| i.path.clone())
        }
        Err(e) => {
            println!("   FAILED: {e}");
            None
        }
    }
}

/// Probe one catalogue page and the first `series` entries on it.
async fn full_scan(adapter: &dyn SourceAdapter, ctx: &Ctx, page: u32, series: usize) {
    println!("\n-- full scan (list_catalog page {page})");
    let catalogue = match adapter.list_catalog(ctx, page).await {
        Ok(page) => page,
        Err(e) => {
            println!("   FAILED: {e}");
            return;
        }
    };
    println!(
        "   {} items, has_next {}",
        catalogue.items.len(),
        catalogue.has_next
    );
    for item in catalogue.items.iter().take(5) {
        println!(
            "   {:<52} {}",
            truncate(&item.path, 52),
            truncate(&item.title, 48)
        );
    }
    for item in catalogue.items.iter().take(series) {
        println!("\n-- catalogue item");
        series_report(adapter, ctx, &item.path, false).await;
    }
}

/// A page number far past any real catalogue. A site that answers it with content is
/// re-serving an earlier page, which is the shape of a server-clamped paginator.
const DEEP_PAGE: u32 = 9_999;

/// Walk the catalogue the way a full scan does — consecutive pages until the adapter says there
/// is no next one — then probe a page far past the end.
///
/// This is the check a single-page sample cannot make. Two failure modes are only visible
/// across pages, and neither reports an error: a paginator clamped server-side re-serves the
/// same document for every page number, so `has_next` never goes false and the walk re-ingests
/// one page until the planner's cap; and a path template that is right on page 1 and wrong
/// afterwards truncates the catalogue to its first page.
async fn catalogue_walk(adapter: &dyn SourceAdapter, ctx: &Ctx, limit: u32) {
    println!("\n-- catalogue walk (up to {limit} pages)");
    let mut seen: HashSet<String> = HashSet::new();
    let mut ended_at = None;
    let mut repeated = false;

    for page in 1..=limit {
        let listing = match adapter.list_catalog(ctx, page).await {
            Ok(listing) => listing,
            Err(e) => {
                println!("   page {page} FAILED: {e}");
                return;
            }
        };
        let fresh = listing
            .items
            .iter()
            .filter(|item| seen.insert(item.path.clone()))
            .count();
        println!(
            "   page {page:>4}: {:>4} items, {fresh:>4} new, has_next {}",
            listing.items.len(),
            listing.has_next
        );
        if fresh == 0 && !listing.items.is_empty() {
            repeated = true;
            println!("   REPEAT: this page re-served series already seen");
        }
        if !listing.has_next {
            ended_at = Some(page);
            break;
        }
    }

    match adapter.list_catalog(ctx, DEEP_PAGE).await {
        Ok(listing) => {
            let fresh = listing
                .items
                .iter()
                .filter(|item| !seen.contains(&item.path))
                .count();
            println!(
                "   page {DEEP_PAGE}: {:>4} items, {fresh:>4} new, has_next {}",
                listing.items.len(),
                listing.has_next
            );
            if listing.has_next && !listing.items.is_empty() && fresh == 0 {
                println!(
                    "   VERDICT: CLAMPED — a page past the end re-serves known series and still \
                     claims a next page, so the walk never terminates"
                );
                return;
            }
            if listing.has_next && !listing.items.is_empty() {
                println!(
                    "   VERDICT: page {DEEP_PAGE} yields unseen series; the catalogue is deeper \
                     than this walk, which is fine, but termination is unproven"
                );
                return;
            }
        }
        // A 404 past the end is the site saying the catalogue ended; the walk reads it the same
        // way, so it is not a failure here either.
        Err(e) => println!("   page {DEEP_PAGE}: refused ({e})"),
    }

    match ended_at {
        Some(page) => println!(
            "   VERDICT: terminates at page {page}; {} distinct series enumerated{}",
            seen.len(),
            if repeated {
                ", but a page repeated"
            } else {
                ""
            }
        ),
        None => println!(
            "   VERDICT: still going at page {limit} ({} distinct series), and a page past the \
             end ends the walk — consistent with a catalogue simply larger than this probe",
            seen.len()
        ),
    }
}

/// Fetch and print one series' metadata and chapter list.
async fn series_report(adapter: &dyn SourceAdapter, ctx: &Ctx, path: &str, all: bool) {
    println!("   path      {path}");
    match adapter.fetch_series(ctx, path).await {
        Ok(meta) => {
            println!("   title     {}", meta.title);
            println!("   alt       {:?}", meta.alt_titles);
            println!(
                "   status    {:?}  type {:?}  year {:?}",
                meta.status, meta.content_type, meta.release_year
            );
            println!("   authors   {:?}", meta.authors);
            println!("   tags      {:?}", meta.tags);
            println!("   cover     {:?}", meta.cover_url);
            println!(
                "   desc      {}",
                truncate(meta.description.as_deref().unwrap_or("(none)"), 100)
            );
        }
        Err(e) => println!("   series FAILED: {e}"),
    }
    match adapter.fetch_chapters(ctx, path).await {
        Ok(chapters) => {
            println!("   chapters  {}", chapters.len());
            let show: Vec<_> = if all || chapters.len() <= 6 {
                chapters.iter().collect()
            } else {
                chapters
                    .iter()
                    .take(3)
                    .chain(chapters.iter().rev().take(3))
                    .collect()
            };
            for c in show {
                println!(
                    "     {:>8}  {:<44} {:?} {:?}",
                    c.number,
                    truncate(&c.path, 44),
                    c.published_at.map(OffsetDateTime::date),
                    c.access
                );
            }
        }
        Err(e) => println!("   chapters FAILED: {e}"),
    }
}

fn truncate(text: &str, max: usize) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max {
        return flat;
    }
    flat.chars().take(max - 1).chain(['…']).collect()
}
