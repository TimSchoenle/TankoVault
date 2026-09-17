//! Portable watchlist entries: what a watchlist backup identifies a series by, and the pure rules
//! for validating an entry, matching it against a catalogue and merging it into a watchlist.
//!
//! Nothing here carries a database id. A backup has to survive the catalogue it was taken from —
//! a wiped database, or a different deployment with different uuids — so an entry names its
//! series only by what another catalogue can rediscover on its own: the provider pages it was
//! read on, external tracker ids, and its titles.

use std::collections::{HashMap, HashSet};

use time::OffsetDateTime;
use url::Url;

use crate::{
    ProviderId, SeriesId, SeriesSourceId, WatchStatus, compact_key, is_storable, normalize_title,
};

/// Most entries one document may carry; also the ceiling on a reader's pending entries.
pub const MAX_ENTRIES: usize = 10_000;
/// Most provider pages one entry may name.
pub const MAX_SOURCES: usize = 32;
/// Most external tracker ids one entry may name.
pub const MAX_EXTERNAL_IDS: usize = 8;
/// Most alternative titles one entry may carry.
pub const MAX_ALTERNATIVE_TITLES: usize = 32;
/// Longest title, in bytes.
pub const MAX_TITLE_BYTES: usize = 512;
/// Longest source URL or path, in bytes.
pub const MAX_URL_BYTES: usize = 2048;
/// Longest provider slug or tracker name, in bytes.
pub const MAX_NAME_BYTES: usize = 64;
/// Longest external id, in bytes.
pub const MAX_EXTERNAL_ID_BYTES: usize = 128;

/// One provider page a series was read on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceRef {
    /// The provider's slug on the exporting deployment.
    pub provider: String,
    /// The absolute page URL at export time.
    pub url: String,
    /// The page path relative to the provider's base URL.
    pub path: String,
    /// Whether the reader pinned this source for the series.
    pub pinned: bool,
}

/// A series' id on an external tracker (`anilist`, …).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalRef {
    /// The tracker name, as `sync_mappings.provider` spells it.
    pub tracker: String,
    /// The tracker's own id for the series.
    pub id: String,
}

/// A reader's two read frontiers, as `read_progress` holds them.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PortableProgress {
    /// Highest whole chapter read.
    pub whole: f64,
    /// Highest part release read ahead of `whole`, if any.
    pub part: Option<f64>,
}

impl PortableProgress {
    /// Whether this is the "nothing read" state, which is not worth a row.
    #[must_use]
    pub fn is_empty(self) -> bool {
        self.whole == 0.0 && self.part.is_none()
    }
}

/// One watchlist entry, independent of any catalogue's ids.
#[derive(Debug, Clone, PartialEq)]
pub struct PortableEntry {
    /// The canonical title on the exporting deployment.
    pub title: String,
    /// Every other title the series was known by there.
    pub alternative_titles: Vec<String>,
    /// The reader's tracking status.
    pub status: WatchStatus,
    /// Whether new chapters notify the reader.
    pub notify: bool,
    /// Whether the series is kept out of external sync.
    pub sync_excluded: bool,
    /// When the reader first tracked the series.
    pub added_at: OffsetDateTime,
    /// The reader's progress, or `None` when nothing was read.
    pub progress: Option<PortableProgress>,
    /// The provider pages carrying the series.
    pub sources: Vec<SourceRef>,
    /// The series' ids on external trackers.
    pub external_ids: Vec<ExternalRef>,
}

/// Why an entry was refused, naming the offending field.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{field}: {reason}")]
pub struct EntryError {
    /// A JSON-path-like pointer (`sources[2].url`), relative to the entry.
    pub field: String,
    /// What is wrong with it.
    pub reason: &'static str,
}

impl EntryError {
    fn new(field: impl Into<String>, reason: &'static str) -> Self {
        Self {
            field: field.into(),
            reason,
        }
    }

    /// Prefix the field with the entry's position in its document.
    #[must_use]
    pub fn at_entry(self, index: usize) -> Self {
        Self {
            field: format!("entries[{index}].{}", self.field),
            reason: self.reason,
        }
    }
}

fn has_control(value: &str) -> bool {
    value.chars().any(char::is_control)
}

fn check_title(field: &str, value: &str) -> Result<(), EntryError> {
    if value.trim().is_empty() {
        return Err(EntryError::new(field, "must not be empty"));
    }
    if value.len() > MAX_TITLE_BYTES {
        return Err(EntryError::new(field, "is too long"));
    }
    if has_control(value) {
        return Err(EntryError::new(
            field,
            "must not contain control characters",
        ));
    }
    Ok(())
}

fn is_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_NAME_BYTES
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

fn check_progress(progress: PortableProgress) -> Result<(), EntryError> {
    if !is_storable(progress.whole) || progress.whole.fract() != 0.0 {
        return Err(EntryError::new(
            "progress.chapter",
            "must be a whole chapter number within the storable range",
        ));
    }
    if let Some(part) = progress.part
        && (!is_storable(part) || part.fract() == 0.0 || part.floor() < progress.whole)
    {
        return Err(EntryError::new(
            "progress.part",
            "must be a fractional chapter number at or above the whole chapter",
        ));
    }
    Ok(())
}

impl PortableEntry {
    /// Check every bound a document entry must meet before anything reads it.
    ///
    /// A backup is a file the reader may have edited, or been handed, so every length is bounded
    /// here rather than trusted to the database: the limits are what keep one import's memory and
    /// statement size proportional to the entry count.
    ///
    /// Source URLs are only ever *compared* against configured provider hosts; nothing fetches
    /// them, so they need no SSRF check. Credentials embedded in one are still refused, because
    /// a URL carrying `user:pass@` is not a page a crawler found and should not be persisted.
    ///
    /// # Errors
    /// The first [`EntryError`] found, in field order.
    pub fn validate(&self) -> Result<(), EntryError> {
        check_title("title", &self.title)?;
        if self.alternative_titles.len() > MAX_ALTERNATIVE_TITLES {
            return Err(EntryError::new("alternative_titles", "has too many items"));
        }
        for (i, title) in self.alternative_titles.iter().enumerate() {
            check_title(&format!("alternative_titles[{i}]"), title)?;
        }
        if let Some(progress) = self.progress {
            check_progress(progress)?;
        }
        if self.sources.len() > MAX_SOURCES {
            return Err(EntryError::new("sources", "has too many items"));
        }
        if self.sources.iter().filter(|s| s.pinned).count() > 1 {
            return Err(EntryError::new("sources", "may pin at most one source"));
        }
        for (i, source) in self.sources.iter().enumerate() {
            validate_source(source).map_err(|e| EntryError {
                field: format!("sources[{i}].{}", e.field),
                reason: e.reason,
            })?;
        }
        if self.external_ids.len() > MAX_EXTERNAL_IDS {
            return Err(EntryError::new("external_ids", "has too many items"));
        }
        for (i, external) in self.external_ids.iter().enumerate() {
            validate_external(external).map_err(|e| EntryError {
                field: format!("external_ids[{i}].{}", e.field),
                reason: e.reason,
            })?;
        }
        Ok(())
    }

    /// Drop or trim whatever in a catalogue-derived entry would fail [`Self::validate`].
    ///
    /// An export is built from catalogue rows the crawler wrote, not from input this module
    /// checked, and one over-long alternative title must not make a reader's whole backup
    /// unimportable. Titles lose control characters and are cut to [`MAX_TITLE_BYTES`]; a source,
    /// external id or part frontier that cannot be made valid is dropped. `None` only when the
    /// title is empty after that.
    #[must_use]
    pub fn sanitized(mut self) -> Option<Self> {
        self.title = clean_title(&self.title)?;
        let mut alternatives: Vec<String> = self
            .alternative_titles
            .iter()
            .filter_map(|t| clean_title(t))
            .filter(|t| *t != self.title)
            .collect();
        alternatives.sort_unstable();
        alternatives.dedup();
        alternatives.truncate(MAX_ALTERNATIVE_TITLES);
        self.alternative_titles = alternatives;
        self.progress = self.progress.and_then(|p| {
            let whole_only = PortableProgress {
                whole: p.whole,
                part: None,
            };
            if check_progress(p).is_ok() {
                Some(p)
            } else {
                check_progress(whole_only).is_ok().then_some(whole_only)
            }
        });
        self.sources.retain(|s| validate_source(s).is_ok());
        self.sources.truncate(MAX_SOURCES);
        let pinned = self.sources.iter().position(|s| s.pinned);
        for (i, source) in self.sources.iter_mut().enumerate() {
            source.pinned = pinned == Some(i);
        }
        self.external_ids.retain(|e| validate_external(e).is_ok());
        self.external_ids.truncate(MAX_EXTERNAL_IDS);
        Some(self)
    }

    /// A stable key for this entry, so importing the same backup twice queues it once.
    ///
    /// Derived from the strongest identifier present — an external id, then a source page, then
    /// the title — and from the smallest one of that kind, so reordering a document's lists does
    /// not change it.
    #[must_use]
    pub fn identity_key(&self) -> String {
        if let Some(external) = self
            .external_ids
            .iter()
            .map(|e| format!("ext:{}:{}", e.tracker, e.id))
            .min()
        {
            return external;
        }
        if let Some(source) = self
            .sources
            .iter()
            .map(|s| format!("src:{}:{}", s.provider, s.path))
            .min()
        {
            return source;
        }
        format!("title:{}", compact_key(&normalize_title(&self.title)))
    }

    /// The whitespace-insensitive title keys this entry could match by.
    #[must_use]
    pub fn title_keys(&self) -> Vec<String> {
        let mut keys: Vec<String> = std::iter::once(&self.title)
            .chain(&self.alternative_titles)
            .map(|t| compact_key(&normalize_title(t)))
            .filter(|k| !k.is_empty())
            .collect();
        keys.sort_unstable();
        keys.dedup();
        keys
    }
}

fn validate_external(external: &ExternalRef) -> Result<(), EntryError> {
    if !is_name(&external.tracker) {
        return Err(EntryError::new(
            "tracker",
            "must be a short name of letters, digits, '-' or '_'",
        ));
    }
    if external.id.is_empty()
        || external.id.len() > MAX_EXTERNAL_ID_BYTES
        || external
            .id
            .chars()
            .any(|c| c.is_control() || c.is_whitespace())
    {
        return Err(EntryError::new(
            "id",
            "must be a non-empty token without whitespace",
        ));
    }
    Ok(())
}

/// A title without control characters, trimmed and cut to [`MAX_TITLE_BYTES`] on a character
/// boundary; `None` when nothing is left.
fn clean_title(title: &str) -> Option<String> {
    let mut out = String::new();
    for c in title.trim().chars().filter(|c| !c.is_control()) {
        if out.len() + c.len_utf8() > MAX_TITLE_BYTES {
            break;
        }
        out.push(c);
    }
    let out = out.trim().to_owned();
    (!out.is_empty()).then_some(out)
}

fn validate_source(source: &SourceRef) -> Result<(), EntryError> {
    if !is_name(&source.provider) {
        return Err(EntryError::new(
            "provider",
            "must be a provider slug of letters, digits, '-' or '_'",
        ));
    }
    if source.url.len() > MAX_URL_BYTES {
        return Err(EntryError::new("url", "is too long"));
    }
    let url =
        Url::parse(&source.url).map_err(|_| EntryError::new("url", "must be an absolute URL"))?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return Err(EntryError::new("url", "must be an http(s) URL with a host"));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(EntryError::new("url", "must not embed credentials"));
    }
    if !source.path.starts_with('/')
        || source.path.len() > MAX_URL_BYTES
        || has_control(&source.path)
    {
        return Err(EntryError::new(
            "path",
            "must be a site-relative path starting with '/'",
        ));
    }
    Ok(())
}

/// A provider as the matcher needs it.
#[derive(Debug, Clone)]
pub struct CatalogueProvider {
    /// The provider's id.
    pub id: ProviderId,
    /// The provider's slug.
    pub slug: String,
    /// The provider's base URL, which stored source paths are relative to.
    pub base_url: String,
}

/// The host a URL is matched by: lower-case, without a leading `www.`.
fn host_key(url: &Url) -> Option<String> {
    let host = url.host_str()?.to_ascii_lowercase();
    Some(
        host.strip_prefix("www.")
            .map_or_else(|| host.clone(), str::to_owned),
    )
}

/// Maps a [`SourceRef`] onto the `(provider, stored path)` pairs it could be stored under here.
#[derive(Debug, Default)]
pub struct ProviderIndex {
    by_slug: HashMap<String, ProviderId>,
    /// Host → `(provider, base path without its trailing '/')`.
    by_host: HashMap<String, Vec<(ProviderId, String)>>,
}

impl ProviderIndex {
    /// Index a catalogue's providers; one whose `base_url` does not parse is matched by slug only.
    #[must_use]
    pub fn new(providers: impl IntoIterator<Item = CatalogueProvider>) -> Self {
        let mut index = Self::default();
        for provider in providers {
            if let Ok(base) = Url::parse(&provider.base_url)
                && let Some(host) = host_key(&base)
            {
                index
                    .by_host
                    .entry(host)
                    .or_default()
                    .push((provider.id, base.path().trim_end_matches('/').to_owned()));
            }
            index.by_slug.insert(provider.slug, provider.id);
        }
        index
    }

    /// Every `(provider, source_path)` this reference could be stored under, most specific first.
    ///
    /// Two independent routes, because each survives a change the other does not: the slug and
    /// path survive a provider moving domain (`base_url` is the one row that changes), and the
    /// URL's host survives a different deployment having named the same site differently.
    #[must_use]
    pub fn candidates(&self, source: &SourceRef) -> Vec<(ProviderId, String)> {
        let mut out: Vec<(ProviderId, String)> = Vec::new();
        let mut push = |pair: (ProviderId, String)| {
            if !out.contains(&pair) {
                out.push(pair);
            }
        };
        if let Some(id) = self.by_slug.get(&source.provider) {
            push((*id, source.path.clone()));
        }
        if let Ok(url) = Url::parse(&source.url)
            && let Some(host) = host_key(&url)
            && let Some(providers) = self.by_host.get(&host)
        {
            let mut full = url.path().to_owned();
            if let Some(query) = url.query() {
                full.push('?');
                full.push_str(query);
            }
            for (id, base_path) in providers {
                if let Some(rest) = full.strip_prefix(base_path.as_str())
                    && rest.starts_with('/')
                {
                    push((*id, rest.to_owned()));
                }
            }
        }
        out
    }
}

/// How an entry was matched to a local series.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatchedBy {
    /// An external tracker id already mapped to the series.
    ExternalId,
    /// A provider page already attached to the series.
    Source,
    /// A unique title match — the weakest route, reported so the reader can check it.
    Title,
}

/// What the catalogue answered for the identifiers a batch of entries asked about.
#[derive(Debug, Default)]
pub struct Lookups {
    /// `(provider, source_path)` → the series and source stored under it.
    pub sources: HashMap<(ProviderId, String), (SeriesId, SeriesSourceId)>,
    /// `(tracker, external id)` → the series mapped to it.
    pub external: HashMap<(String, String), SeriesId>,
    /// Compact title key → every series carrying it, as a canonical or alternative title.
    pub titles: HashMap<String, Vec<SeriesId>>,
}

/// An entry's local series, and its pinned source where that also matched.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Resolved {
    /// The local series.
    pub series_id: SeriesId,
    /// The route that found it.
    pub matched_by: MatchedBy,
    /// The local source the entry pinned, when it belongs to that series.
    pub pinned_source: Option<SeriesSourceId>,
}

/// Match one entry against the catalogue: external ids, then source pages, then — only when
/// `match_titles` — a title that names exactly one series.
///
/// Title matching is opt-in and must be unique because titles are where two different works
/// collide; attaching a reader's progress to the wrong series is worse than leaving it pending.
#[must_use]
pub fn resolve(
    entry: &PortableEntry,
    index: &ProviderIndex,
    lookups: &Lookups,
    match_titles: bool,
) -> Option<Resolved> {
    let by_external = entry.external_ids.iter().find_map(|e| {
        lookups
            .external
            .get(&(e.tracker.clone(), e.id.clone()))
            .copied()
    });
    let (series_id, matched_by) = if let Some(id) = by_external {
        (id, MatchedBy::ExternalId)
    } else if let Some((id, _)) = entry
        .sources
        .iter()
        .flat_map(|s| index.candidates(s))
        .find_map(|key| lookups.sources.get(&key).copied())
    {
        (id, MatchedBy::Source)
    } else if match_titles {
        let found: HashSet<SeriesId> = entry
            .title_keys()
            .iter()
            .filter_map(|k| lookups.titles.get(k))
            .flatten()
            .copied()
            .collect();
        let mut found = found.into_iter();
        match (found.next(), found.next()) {
            (Some(id), None) => (id, MatchedBy::Title),
            _ => return None,
        }
    } else {
        return None;
    };

    let pinned_source = entry
        .sources
        .iter()
        .filter(|s| s.pinned)
        .flat_map(|s| index.candidates(s))
        .filter_map(|key| lookups.sources.get(&key))
        .find(|(series, _)| *series == series_id)
        .map(|(_, source)| *source);

    Some(Resolved {
        series_id,
        matched_by,
        pinned_source,
    })
}

/// Which side wins when an entry is already on the watchlist.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ConflictPolicy {
    /// Keep the local entry; only read progress may move, and only forwards.
    KeepLocal,
    /// The imported entry replaces the local one, progress included.
    PreferImported,
}

/// A watchlist entry as it stands locally.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LocalEntry {
    /// The tracking status.
    pub status: WatchStatus,
    /// Whether new chapters notify.
    pub notify: bool,
    /// Whether the series is kept out of external sync.
    pub sync_excluded: bool,
    /// The pinned source, if any.
    pub pinned_source: Option<SeriesSourceId>,
}

/// One watchlist row to insert or update.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EntryWrite {
    /// The series tracked.
    pub series_id: SeriesId,
    /// The tracking status.
    pub status: WatchStatus,
    /// Whether new chapters notify.
    pub notify: bool,
    /// Whether the series is kept out of external sync.
    pub sync_excluded: bool,
    /// When the entry was first added; only used when inserting.
    pub added_at: OffsetDateTime,
    /// The source to pin, already checked to belong to the series.
    pub pinned_source: Option<SeriesSourceId>,
}

/// The progress to write for `local` given `imported`, or `None` when nothing should change.
///
/// Under [`ConflictPolicy::KeepLocal`] progress only advances: each frontier takes the higher of
/// the two, and a part frontier left behind by the whole one is dropped, preserving the
/// `floor(part) >= whole` invariant `read_progress` relies on. An empty import never creates a
/// row, since "no row" and "chapter 0" mean different things to external sync.
#[must_use]
pub fn merged_progress(
    local: Option<PortableProgress>,
    imported: Option<PortableProgress>,
    policy: ConflictPolicy,
) -> Option<PortableProgress> {
    let imported = imported?;
    let target = match (policy, local) {
        (_, None) | (ConflictPolicy::PreferImported, Some(_)) => imported,
        (ConflictPolicy::KeepLocal, Some(local)) => {
            let whole = local.whole.max(imported.whole);
            let part = [local.part, imported.part]
                .into_iter()
                .flatten()
                .filter(|p| p.floor() >= whole)
                .reduce(f64::max);
            PortableProgress { whole, part }
        }
    };
    match local {
        None if target.is_empty() => None,
        Some(local) if local == target => None,
        _ => Some(target),
    }
}

/// Everything an import would change, and how each entry fared.
#[derive(Debug, Default)]
pub struct Plan {
    /// Watchlist rows to insert or update.
    pub writes: Vec<EntryWrite>,
    /// Progress rows to write, exactly as given.
    pub progress: Vec<(SeriesId, PortableProgress)>,
    /// Series the document matched, whether or not anything about them changed.
    pub matched: HashSet<SeriesId>,
    /// Entries new to the watchlist.
    pub added: usize,
    /// Entries already tracked that the import changed.
    pub updated: usize,
    /// Entries already tracked that the import left as they were.
    pub unchanged: usize,
    /// Indices of entries that matched no local series.
    pub unmatched: Vec<usize>,
    /// Indices of entries that matched a series an earlier entry already claimed.
    pub duplicates: Vec<usize>,
    /// Indices of entries matched only by title.
    pub title_matches: Vec<usize>,
}

/// Decide what importing `entries` into a watchlist holding `local` should write.
///
/// `resolved[i]` is `entries[i]`'s match. `local_progress` covers every series the reader has
/// progress on, tracked or not: a progress row can outlive its watchlist entry, and an import
/// that re-adds the series must merge with it rather than write over it. The first entry matching a series wins; a later one is
/// reported as a duplicate rather than applied over it, so the outcome does not depend on which
/// of two conflicting rows a document happened to list last.
///
/// # Panics
/// When `resolved` and `entries` differ in length.
#[must_use]
pub fn plan<S: std::hash::BuildHasher, T: std::hash::BuildHasher>(
    entries: &[PortableEntry],
    resolved: &[Option<Resolved>],
    local: &HashMap<SeriesId, LocalEntry, S>,
    local_progress: &HashMap<SeriesId, PortableProgress, T>,
    policy: ConflictPolicy,
) -> Plan {
    assert_eq!(entries.len(), resolved.len(), "one resolution per entry");
    let mut plan = Plan::default();
    for (index, (entry, resolution)) in entries.iter().zip(resolved).enumerate() {
        let Some(resolution) = resolution else {
            plan.unmatched.push(index);
            continue;
        };
        if !plan.matched.insert(resolution.series_id) {
            plan.duplicates.push(index);
            continue;
        }
        if resolution.matched_by == MatchedBy::Title {
            plan.title_matches.push(index);
        }

        let imported = EntryWrite {
            series_id: resolution.series_id,
            status: entry.status,
            notify: entry.notify,
            sync_excluded: entry.sync_excluded,
            added_at: entry.added_at,
            pinned_source: resolution.pinned_source,
        };
        let existing = local.get(&resolution.series_id);
        let entry_changed = match existing {
            None => {
                plan.writes.push(imported);
                plan.added += 1;
                true
            }
            Some(current) => {
                let differs = current.status != imported.status
                    || current.notify != imported.notify
                    || current.sync_excluded != imported.sync_excluded
                    || current.pinned_source != imported.pinned_source;
                if policy == ConflictPolicy::PreferImported && differs {
                    plan.writes.push(imported);
                    true
                } else {
                    false
                }
            }
        };

        let progress = merged_progress(
            local_progress.get(&resolution.series_id).copied(),
            entry.progress,
            policy,
        );
        if let Some(progress) = progress {
            plan.progress.push((resolution.series_id, progress));
        }
        if existing.is_some() {
            if entry_changed || progress.is_some() {
                plan.updated += 1;
            } else {
                plan.unchanged += 1;
            }
        }
    }
    plan
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn provider(n: u128, slug: &str, base: &str) -> CatalogueProvider {
        CatalogueProvider {
            id: ProviderId::from_uuid(Uuid::from_u128(n)),
            slug: slug.to_owned(),
            base_url: base.to_owned(),
        }
    }

    fn source(provider: &str, url: &str, path: &str) -> SourceRef {
        SourceRef {
            provider: provider.to_owned(),
            url: url.to_owned(),
            path: path.to_owned(),
            pinned: false,
        }
    }

    fn entry(title: &str) -> PortableEntry {
        PortableEntry {
            title: title.to_owned(),
            alternative_titles: Vec::new(),
            status: WatchStatus::Reading,
            notify: true,
            sync_excluded: false,
            added_at: OffsetDateTime::UNIX_EPOCH,
            progress: None,
            sources: Vec::new(),
            external_ids: Vec::new(),
        }
    }

    fn series(n: u128) -> SeriesId {
        SeriesId::from_uuid(Uuid::from_u128(n))
    }

    fn progress(whole: f64, part: Option<f64>) -> PortableProgress {
        PortableProgress { whole, part }
    }

    #[test]
    fn a_source_is_found_by_slug_after_its_provider_moved_domain() {
        let index = ProviderIndex::new([provider(1, "asura", "https://asura.new/")]);
        let found = index.candidates(&source("asura", "https://asura.old/series/x", "/series/x"));
        assert_eq!(
            found,
            vec![(
                ProviderId::from_uuid(Uuid::from_u128(1)),
                "/series/x".into()
            )]
        );
    }

    #[test]
    fn a_source_is_found_by_host_when_another_deployment_named_the_provider_differently() {
        let index = ProviderIndex::new([provider(2, "asura-scans", "https://www.Asura.gg/read")]);
        let found = index.candidates(&source(
            "asura",
            "https://asura.gg/read/series/x?id=4",
            "/whatever",
        ));
        assert_eq!(
            found,
            vec![(
                ProviderId::from_uuid(Uuid::from_u128(2)),
                "/series/x?id=4".into()
            )]
        );
    }

    /// A host match must respect the base path: `https://h/read` does not own `https://h/reader`.
    #[test]
    fn a_host_match_does_not_cross_a_base_path_boundary() {
        let index = ProviderIndex::new([provider(3, "p", "https://h.example/read")]);
        assert!(
            index
                .candidates(&source("q", "https://h.example/reader/x", "/x"))
                .is_empty()
        );
    }

    #[test]
    fn credentials_in_a_source_url_are_refused() {
        let mut e = entry("X");
        e.sources
            .push(source("p", "https://user:pw@h.example/x", "/x"));
        let err = e.validate().unwrap_err();
        assert_eq!(err.field, "sources[0].url");
    }

    #[test]
    fn a_part_behind_the_whole_frontier_is_refused() {
        let mut e = entry("X");
        e.progress = Some(progress(10.0, Some(9.5)));
        assert_eq!(e.validate().unwrap_err().field, "progress.part");
    }

    /// An export must always re-import: catalogue data the crawler wrote is not held to the
    /// document's limits, and one bad alternative title would otherwise refuse the whole backup.
    #[test]
    fn a_sanitized_entry_always_validates() {
        let mut e = entry(&format!("  {}\u{7}  ", "\u{e9}".repeat(400)));
        e.alternative_titles = vec!["\n".into(), "ok".into(), "x".repeat(600)];
        e.progress = Some(progress(10.0, Some(3.5)));
        e.sources = vec![
            SourceRef {
                pinned: true,
                ..source("p", "not a url", "/a")
            },
            source("p", "https://p.example/b", "/b"),
        ];
        e.external_ids = vec![ExternalRef {
            tracker: "anilist".into(),
            id: "has space".into(),
        }];
        let clean = e.sanitized().unwrap();
        assert_eq!(clean.validate(), Ok(()));
        assert!(clean.title.len() <= MAX_TITLE_BYTES);
        assert_eq!(clean.progress, Some(progress(10.0, None)));
        assert_eq!(clean.sources.len(), 1);
        assert!(!clean.sources[0].pinned);
        assert!(clean.external_ids.is_empty());
    }

    #[test]
    fn identity_key_ignores_list_order() {
        let mut a = entry("X");
        a.sources = vec![
            source("b", "https://b.example/2", "/2"),
            source("a", "https://a.example/1", "/1"),
        ];
        let mut b = a.clone();
        b.sources.reverse();
        assert_eq!(a.identity_key(), b.identity_key());
        assert_eq!(a.identity_key(), "src:a:/1");
    }

    /// Title matching is how two different works get conflated, so an ambiguous title must
    /// leave the entry unmatched rather than pick one.
    #[test]
    fn an_ambiguous_title_matches_nothing() {
        let index = ProviderIndex::default();
        let mut lookups = Lookups::default();
        lookups
            .titles
            .insert("berserk".into(), vec![series(1), series(2)]);
        assert_eq!(resolve(&entry("Berserk"), &index, &lookups, true), None);

        lookups.titles.insert("berserk".into(), vec![series(1)]);
        assert_eq!(resolve(&entry("Berserk"), &index, &lookups, false), None);
        assert_eq!(
            resolve(&entry("Berserk"), &index, &lookups, true).map(|r| r.matched_by),
            Some(MatchedBy::Title)
        );
    }

    /// A pin naming a source of a different series than the entry matched must not be applied.
    #[test]
    fn a_pin_on_another_series_is_dropped() {
        let index = ProviderIndex::new([provider(1, "p", "https://p.example")]);
        let pid = ProviderId::from_uuid(Uuid::from_u128(1));
        let mut lookups = Lookups::default();
        lookups
            .external
            .insert(("anilist".into(), "7".into()), series(1));
        lookups.sources.insert(
            (pid, "/other".into()),
            (series(2), SeriesSourceId::from_uuid(Uuid::from_u128(9))),
        );
        let mut e = entry("X");
        e.external_ids.push(ExternalRef {
            tracker: "anilist".into(),
            id: "7".into(),
        });
        e.sources.push(SourceRef {
            pinned: true,
            ..source("p", "https://p.example/other", "/other")
        });
        let resolved = resolve(&e, &index, &lookups, false).unwrap();
        assert_eq!(resolved.series_id, series(1));
        assert_eq!(resolved.pinned_source, None);
    }

    #[test]
    fn keep_local_only_advances_progress() {
        let policy = ConflictPolicy::KeepLocal;
        assert_eq!(
            merged_progress(
                Some(progress(10.0, None)),
                Some(progress(5.0, None)),
                policy
            ),
            None
        );
        assert_eq!(
            merged_progress(
                Some(progress(10.0, Some(10.5))),
                Some(progress(12.0, None)),
                policy
            ),
            Some(progress(12.0, None))
        );
        assert_eq!(
            merged_progress(
                Some(progress(10.0, None)),
                Some(progress(9.0, Some(11.5))),
                policy
            ),
            Some(progress(10.0, Some(11.5)))
        );
        assert_eq!(
            merged_progress(None, Some(progress(0.0, None)), policy),
            None
        );
    }

    #[test]
    fn prefer_imported_may_move_progress_backwards() {
        assert_eq!(
            merged_progress(
                Some(progress(10.0, None)),
                Some(progress(5.0, None)),
                ConflictPolicy::PreferImported
            ),
            Some(progress(5.0, None))
        );
    }

    #[test]
    fn plan_counts_and_reports_duplicates() {
        let resolved_one = Some(Resolved {
            series_id: series(1),
            matched_by: MatchedBy::Source,
            pinned_source: None,
        });
        let entries = vec![entry("A"), entry("A again"), entry("B")];
        let resolved = vec![resolved_one, resolved_one, None];
        let out = plan(
            &entries,
            &resolved,
            &HashMap::new(),
            &HashMap::new(),
            ConflictPolicy::KeepLocal,
        );
        assert_eq!(out.added, 1);
        assert_eq!(out.duplicates, vec![1]);
        assert_eq!(out.unmatched, vec![2]);
    }

    #[test]
    fn keep_local_leaves_an_existing_entry_alone() {
        let local_entry = LocalEntry {
            status: WatchStatus::Dropped,
            notify: false,
            sync_excluded: true,
            pinned_source: None,
        };
        let local = HashMap::from([(series(1), local_entry)]);
        let no_progress = HashMap::new();
        let entries = vec![entry("A")];
        let resolved = vec![Some(Resolved {
            series_id: series(1),
            matched_by: MatchedBy::ExternalId,
            pinned_source: None,
        })];
        let keep = plan(
            &entries,
            &resolved,
            &local,
            &no_progress,
            ConflictPolicy::KeepLocal,
        );
        assert!(keep.writes.is_empty());
        assert_eq!(keep.unchanged, 1);
        let prefer = plan(
            &entries,
            &resolved,
            &local,
            &no_progress,
            ConflictPolicy::PreferImported,
        );
        assert_eq!(prefer.writes.len(), 1);
        assert_eq!(prefer.updated, 1);
    }
}
