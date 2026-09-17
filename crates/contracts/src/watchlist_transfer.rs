//! The watchlist backup document: its versioned wire shapes, the parser that upgrades any
//! supported version, and the import report.
//!
//! # Versioning
//!
//! Every document carries `format` and `version`. Each version's shape is frozen once released:
//! a change to what an entry holds is a new `vN` module converting into `PortableEntry`, and
//! [`parse_document`] keeps accepting every older version by upgrading it. A document newer than
//! [`CURRENT_VERSION`] is refused by name rather than half-read, since a field this server does
//! not know about may be the one that made the entry mean something different.

use serde::{Deserialize, Serialize};
use tankovault_domain::WatchStatus;
use tankovault_domain::watchlist_transfer::{
    EntryError, ExternalRef, MAX_ENTRIES, PortableEntry, PortableProgress, SourceRef,
};
use time::OffsetDateTime;
use utoipa::ToSchema;

/// The `format` every watchlist backup declares.
pub const FORMAT: &str = "tankovault.watchlist";

/// The version [`export_document`] writes.
pub const CURRENT_VERSION: u32 = 1;

/// Version 1 of the document.
pub mod v1 {
    use super::{Deserialize, OffsetDateTime, Serialize, ToSchema, WatchStatus};

    /// A watchlist backup, version 1.
    ///
    /// Unknown fields are refused at every level: within one version the shape is closed, so an
    /// unrecognised key is a hand-edit gone wrong or a newer document mislabelled, and neither
    /// should be imported as though the key were not there.
    #[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
    #[serde(deny_unknown_fields)]
    #[schema(as = WatchlistExport)]
    pub struct Document {
        /// Always `tankovault.watchlist`.
        pub format: String,
        /// Always `1` for this shape.
        pub version: u32,
        #[serde(with = "time::serde::rfc3339")]
        #[schema(value_type = String)]
        /// When the backup was taken.
        pub exported_at: OffsetDateTime,
        /// Every tracked series.
        pub entries: Vec<Entry>,
    }

    /// One tracked series, identified without any database id.
    #[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
    #[serde(deny_unknown_fields)]
    #[schema(as = WatchlistExportEntry)]
    pub struct Entry {
        /// The series' canonical title.
        pub title: String,
        /// Other titles the series is known by.
        #[serde(default)]
        pub alternative_titles: Vec<String>,
        /// The tracking status.
        pub status: WatchStatus,
        /// Whether new chapters notify the reader.
        pub notify: bool,
        /// Whether the series is kept out of external sync.
        #[serde(default)]
        pub sync_excluded: bool,
        /// When the series was first tracked.
        #[serde(with = "time::serde::rfc3339")]
        #[schema(value_type = String)]
        pub added_at: OffsetDateTime,
        /// `null` when nothing has been read.
        #[serde(default)]
        pub progress: Option<Progress>,
        /// Provider pages carrying the series; the primary way an import finds it again.
        #[serde(default)]
        pub sources: Vec<Source>,
        /// Ids on external trackers, where this deployment had mapped the series to one.
        #[serde(default)]
        pub external_ids: Vec<ExternalId>,
    }

    /// Read progress.
    #[derive(Debug, Clone, Copy, Serialize, Deserialize, ToSchema)]
    #[serde(deny_unknown_fields)]
    #[schema(as = WatchlistExportProgress)]
    pub struct Progress {
        /// The highest whole chapter read.
        pub chapter: f64,
        /// The highest part release read beyond it, such as `12.5`.
        #[serde(default)]
        pub part: Option<f64>,
    }

    /// A provider page carrying the series.
    #[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
    #[serde(deny_unknown_fields)]
    #[schema(as = WatchlistExportSource)]
    pub struct Source {
        /// The provider's slug on the exporting deployment.
        pub provider: String,
        /// The page's absolute URL at export time.
        pub url: String,
        /// The page's path relative to the provider's base URL.
        pub path: String,
        /// Whether the reader pinned this source.
        #[serde(default)]
        pub pinned: bool,
    }

    /// An id on an external tracker.
    #[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
    #[serde(deny_unknown_fields)]
    #[schema(as = WatchlistExportExternalId)]
    pub struct ExternalId {
        /// The tracker, such as `anilist`.
        pub tracker: String,
        /// The tracker's id for the series.
        pub id: String,
    }
}

impl From<v1::Entry> for PortableEntry {
    fn from(e: v1::Entry) -> Self {
        Self {
            title: e.title,
            alternative_titles: e.alternative_titles,
            status: e.status,
            notify: e.notify,
            sync_excluded: e.sync_excluded,
            added_at: e.added_at,
            progress: e.progress.map(|p| PortableProgress {
                whole: p.chapter,
                part: p.part,
            }),
            sources: e
                .sources
                .into_iter()
                .map(|s| SourceRef {
                    provider: s.provider,
                    url: s.url,
                    path: s.path,
                    pinned: s.pinned,
                })
                .collect(),
            external_ids: e
                .external_ids
                .into_iter()
                .map(|x| ExternalRef {
                    tracker: x.tracker,
                    id: x.id,
                })
                .collect(),
        }
    }
}

impl From<PortableEntry> for v1::Entry {
    fn from(e: PortableEntry) -> Self {
        Self {
            title: e.title,
            alternative_titles: e.alternative_titles,
            status: e.status,
            notify: e.notify,
            sync_excluded: e.sync_excluded,
            added_at: e.added_at,
            progress: e.progress.map(|p| v1::Progress {
                chapter: p.whole,
                part: p.part,
            }),
            sources: e
                .sources
                .into_iter()
                .map(|s| v1::Source {
                    provider: s.provider,
                    url: s.url,
                    path: s.path,
                    pinned: s.pinned,
                })
                .collect(),
            external_ids: e
                .external_ids
                .into_iter()
                .map(|x| v1::ExternalId {
                    tracker: x.tracker,
                    id: x.id,
                })
                .collect(),
        }
    }
}

/// Build the current version's document from `entries`.
#[must_use]
pub fn export_document(exported_at: OffsetDateTime, entries: Vec<PortableEntry>) -> v1::Document {
    v1::Document {
        format: FORMAT.to_owned(),
        version: CURRENT_VERSION,
        exported_at,
        entries: entries.into_iter().map(v1::Entry::from).collect(),
    }
}

/// A document read and upgraded to the current model.
#[derive(Debug, Clone)]
pub struct ParsedDocument {
    /// The version the document was written as.
    pub version: u32,
    /// The document's entries, validated.
    pub entries: Vec<PortableEntry>,
}

/// Why a document was refused. Every message is safe to return to the caller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DocumentError {
    /// Not JSON, or not the shape its declared version requires.
    Malformed(String),
    /// JSON, but not a watchlist backup.
    NotAWatchlist,
    /// A version this server does not read.
    UnsupportedVersion {
        /// The version the document declared.
        found: u64,
    },
    /// More entries than one import may carry.
    TooManyEntries {
        /// How many the document carried.
        found: usize,
    },
    /// An entry failed validation.
    InvalidEntry(EntryError),
}

impl std::fmt::Display for DocumentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Malformed(detail) => write!(f, "malformed watchlist document: {detail}"),
            Self::NotAWatchlist => write!(f, "not a watchlist backup (expected format {FORMAT:?})"),
            Self::UnsupportedVersion { found } if *found > u64::from(CURRENT_VERSION) => write!(
                f,
                "watchlist backup version {found} was written by a newer server; this one reads \
                 versions 1 to {CURRENT_VERSION}"
            ),
            Self::UnsupportedVersion { found } => {
                write!(f, "unsupported watchlist backup version {found}")
            }
            Self::TooManyEntries { found } => write!(
                f,
                "a watchlist backup may carry at most {MAX_ENTRIES} entries, this one has {found}"
            ),
            Self::InvalidEntry(e) => write!(f, "invalid watchlist entry: {e}"),
        }
    }
}

impl std::error::Error for DocumentError {}

/// Just enough to decide which version's shape to read the rest as.
#[derive(Deserialize)]
struct Envelope {
    format: Option<String>,
    version: Option<u64>,
}

/// Parse any supported version of a watchlist backup and validate every entry.
///
/// Reads the bytes twice — once for the envelope, once as the declared version — rather than
/// through a `serde_json::Value`, so an import never holds an untyped tree of the whole document.
///
/// # Errors
/// [`DocumentError`], naming the first problem found.
pub fn parse_document(bytes: &[u8]) -> Result<ParsedDocument, DocumentError> {
    let malformed = |e: serde_json::Error| DocumentError::Malformed(e.to_string());
    let envelope: Envelope = serde_json::from_slice(bytes).map_err(malformed)?;
    if envelope.format.as_deref() != Some(FORMAT) {
        return Err(DocumentError::NotAWatchlist);
    }
    let found = envelope.version.ok_or(DocumentError::NotAWatchlist)?;
    let (version, entries) = match found {
        1 => {
            let doc: v1::Document = serde_json::from_slice(bytes).map_err(malformed)?;
            (
                1,
                doc.entries
                    .into_iter()
                    .map(PortableEntry::from)
                    .collect::<Vec<_>>(),
            )
        }
        _ => return Err(DocumentError::UnsupportedVersion { found }),
    };
    if entries.len() > MAX_ENTRIES {
        return Err(DocumentError::TooManyEntries {
            found: entries.len(),
        });
    }
    for (index, entry) in entries.iter().enumerate() {
        entry
            .validate()
            .map_err(|e| DocumentError::InvalidEntry(e.at_entry(index)))?;
    }
    Ok(ParsedDocument { version, entries })
}

/// How an import treats series already on the watchlist.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
#[schema(as = WatchlistImportMode)]
pub enum ImportMode {
    /// Add what is missing; keep every existing entry, advancing read progress only.
    #[default]
    Merge,
    /// Add what is missing; the backup's entry and progress replace an existing one.
    Overwrite,
    /// As `overwrite`, and remove every entry — and every pending entry — the backup lacks.
    Replace,
}

/// One entry an import wants the reader to look at.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[schema(as = WatchlistImportIssue)]
pub struct ImportIssue {
    /// The entry's zero-based position in the document.
    pub index: i64,
    /// The entry's title, as the document gave it.
    pub title: String,
}

/// What an import did, or with `dry_run`, would do.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[schema(as = WatchlistImportReport)]
pub struct ImportReport {
    /// Whether nothing was written.
    pub dry_run: bool,
    /// The mode the import ran in.
    pub mode: ImportMode,
    /// The document version that was read.
    pub format_version: u32,
    /// Entries in the document.
    pub entries: i64,
    /// Entries added to the watchlist.
    pub added: i64,
    /// Existing entries the import changed.
    pub updated: i64,
    /// Existing entries left exactly as they were.
    pub unchanged: i64,
    /// Entries removed because the backup lacks them (`replace` only).
    pub removed: i64,
    /// Series whose read progress was written.
    pub progress_written: i64,
    /// Entries naming no series this catalogue has yet, queued to attach once it does.
    pub pending: i64,
    /// Entries matching a series an earlier entry already claimed; ignored.
    pub duplicates: i64,
    /// The first 100 entries left pending.
    pub pending_entries: Vec<ImportIssue>,
    /// The first 100 entries matched by title alone, which are worth checking.
    pub title_matches: Vec<ImportIssue>,
}

/// Most entries an [`ImportReport`] lists by name in each list; the counts are always complete.
pub const ISSUE_LIST_LIMIT: usize = 100;
// The published field descriptions spell the number out; change both together.
const _: () = assert!(ISSUE_LIST_LIMIT == 100);

/// A backup entry still waiting for its series to appear in this catalogue.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[schema(as = WatchlistPendingEntry)]
pub struct PendingEntryView {
    /// The entry's title.
    pub title: String,
    /// The entry's source page URLs, so the reader can see what it is waiting for.
    pub source_urls: Vec<String>,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String)]
    /// When the entry was first queued.
    pub queued_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String)]
    /// When a match was last attempted.
    pub last_attempt_at: OffsetDateTime,
    /// How many times a match has been attempted.
    pub attempts: i32,
}

/// The reader's pending backup entries.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[schema(as = WatchlistPending)]
pub struct PendingView {
    /// Every pending entry the reader has.
    pub total: i64,
    /// The oldest 200 of them.
    pub items: Vec<PendingEntryView>,
}

/// Most pending entries one [`PendingView`] lists.
pub const PENDING_LIST_LIMIT: i64 = 200;
// The published field description spells the number out; change both together.
const _: () = assert!(PENDING_LIST_LIMIT == 200);

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn doc(version: u64, entries: &serde_json::Value) -> Vec<u8> {
        serde_json::to_vec(&json!({
            "format": FORMAT,
            "version": version,
            "exported_at": "2026-09-17T12:00:00Z",
            "entries": entries,
        }))
        .unwrap()
    }

    fn entry() -> serde_json::Value {
        json!({
            "title": "Berserk",
            "status": "reading",
            "notify": true,
            "added_at": "2026-01-01T00:00:00Z",
            "progress": { "chapter": 12.0, "part": 12.5 },
            "sources": [{ "provider": "p", "url": "https://p.example/s/berserk", "path": "/s/berserk", "pinned": true }],
            "external_ids": [{ "tracker": "anilist", "id": "30002" }],
        })
    }

    #[test]
    fn an_exported_document_parses_back_to_the_same_entries() {
        let parsed = parse_document(&doc(1, &json!([entry()]))).unwrap();
        let exported = export_document(OffsetDateTime::UNIX_EPOCH, parsed.entries.clone());
        let bytes = serde_json::to_vec(&exported).unwrap();
        let reparsed = parse_document(&bytes).unwrap();
        assert_eq!(reparsed.version, CURRENT_VERSION);
        assert_eq!(reparsed.entries, parsed.entries);
    }

    #[test]
    fn a_newer_version_is_refused_by_name() {
        let err = parse_document(&doc(2, &json!([]))).unwrap_err();
        assert_eq!(err, DocumentError::UnsupportedVersion { found: 2 });
        assert!(err.to_string().contains("newer server"));
    }

    #[test]
    fn a_document_without_the_format_marker_is_refused() {
        let bytes = serde_json::to_vec(&json!({ "version": 1, "entries": [] })).unwrap();
        assert_eq!(
            parse_document(&bytes).unwrap_err(),
            DocumentError::NotAWatchlist
        );
    }

    /// Within a version the shape is closed: an extra key must not be silently ignored.
    #[test]
    fn an_unknown_field_is_refused() {
        let mut e = entry();
        e["series_id"] = json!("00000000-0000-0000-0000-000000000001");
        assert!(matches!(
            parse_document(&doc(1, &json!([e]))),
            Err(DocumentError::Malformed(_))
        ));
    }

    #[test]
    fn an_invalid_entry_is_reported_with_its_index() {
        let mut bad = entry();
        bad["sources"][0]["url"] = json!("javascript:alert(1)");
        let err = parse_document(&doc(1, &json!([entry(), bad]))).unwrap_err();
        let DocumentError::InvalidEntry(e) = err else {
            panic!("expected an entry error, got {err:?}");
        };
        assert_eq!(e.field, "entries[1].sources[0].url");
    }
}
