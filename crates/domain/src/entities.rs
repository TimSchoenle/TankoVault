//! The persisted rows as the rest of the workspace sees them, carrying no persistence concern.
//!
//! `crates/db` maps SQL rows onto these; nothing here knows a query exists. Chapter numbers are
//! `f64` to match the adapter contract while the column is `numeric(10,4)`, so a value that
//! survives a round trip has at most four decimal places.

use crate::enums::{
    AccountStatus, AdapterKind, ContentType, ProviderState, RunState, ScanMode, SeriesStatus,
    TaskState, WatchStatus,
};
use crate::ids::{
    AuthorId, NotificationId, ProviderId, ScanRunId, ScanTaskId, SeriesId, SeriesSourceId, TagId,
    UserId,
};
use crate::politeness::Politeness;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use utoipa::ToSchema;

/// A source site. The single place a domain (`base_url`) is defined.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct Provider {
    /// Primary key. Deleting it cascades to every source and chapter beneath it; a scan run
    /// survives with its `provider_id` set to `None`.
    pub id: ProviderId,
    /// Stable URL-safe handle, unique across providers, and what a task subject and a console
    /// filter are both written in terms of.
    pub slug: String,
    /// Site name as an operator reads it in the console.
    pub name: String,
    /// The domain root. Changing this one field migrates every stored link.
    pub base_url: String,
    /// Which parser drives this site, and so how `config` is read.
    pub adapter: AdapterKind,
    /// Adapter selector/pagination config (shape depends on `adapter`).
    pub config: serde_json::Value,
    /// Live health, which is what the scheduler consults before dispatching work here.
    pub state: ProviderState,
    /// The crawl budget every request to this site is held to.
    pub politeness: Politeness,
    /// How this row relates to the built-in preset catalogue; `None` for a provider an
    /// operator registered by hand.
    pub preset: Option<PresetLink>,
    /// Wire shape is a plain string (whatever `time`'s serde impl emits), never parsed
    /// client-side — kept untyped here so the generated frontend type doesn't need a date
    /// crate (mirrors the `no date crate in the bundle` constraint on the other timestamp
    /// fields below).
    #[serde(with = "time::serde::rfc3339::option")]
    #[schema(value_type = Option<String>)]
    pub last_full_scan_at: Option<OffsetDateTime>,
    /// When the provider was registered.
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String)]
    pub created_at: OffsetDateTime,
    /// When any field on the row was last written, a preset sync included.
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String)]
    pub updated_at: OffsetDateTime,
}

/// A provider row's link to the preset catalogue shipped with the build.
///
/// `locked` is the contract the whole feature turns on: while it holds, the installer
/// overwrites the **preset-owned fields** — `name`, `base_url`, `adapter` and `config` — from
/// the shipped definition on every rollout, so a selector fix reaches deployments that already
/// carry the row. Unlocking detaches the row for good; it keeps the slug so the console can
/// still offer a re-link, but nothing rewrites it again unless an operator asks.
///
/// `politeness` and `state` sit outside the lock on purpose, and must stay there: a crawl
/// budget and a pause are an operator's answer to their own infrastructure, robots policy and
/// legal position, none of which a shipped preset can know. A lock that silently restored a
/// rate limit an operator had lowered would be a worse bug than a stale selector.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PresetLink {
    /// The governing preset — the row's own slug for anything the installer created.
    pub slug: String,
    /// Whether the preset-owned fields still follow the shipped definition.
    pub locked: bool,
    /// When those fields were last written from the preset.
    #[serde(with = "time::serde::rfc3339::option")]
    #[schema(value_type = Option<String>)]
    pub synced_at: Option<OffsetDateTime>,
}

/// One entry of the preset catalogue bundled with the build, as the installer recorded it.
///
/// Persisted rather than read from `tankovault_adapters`, because the api tier deliberately
/// does not link the adapter crate (it would drag `BoringSSL` into that image); the install job
/// writes the catalogue down and every other tier reads it as data.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PresetDefinition {
    /// Catalogue key, which is also the slug of any provider the installer creates from it.
    pub slug: String,
    /// Site name written onto a locked row on every rollout.
    pub name: String,
    /// Domain root written onto a locked row on every rollout.
    pub base_url: String,
    /// Parser written onto a locked row on every rollout.
    pub adapter: AdapterKind,
    /// Selectors and pagination written onto a locked row on every rollout.
    pub config: serde_json::Value,
    /// What a *new* provider from this preset starts at. Never re-applied to an existing row —
    /// see [`PresetLink`] on why politeness is outside the lock.
    pub politeness: Politeness,
    /// When the installer last recorded this entry, which is also the answer to "did my
    /// rollout run the install job at all?".
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String)]
    pub updated_at: OffsetDateTime,
}

/// The canonical, provider-independent work.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Series {
    /// Primary key. Deleting it cascades to titles, tags, sources and everything under them.
    pub id: SeriesId,
    /// The title the work is presented under, once its provider titles have been merged.
    pub canonical_title: String,
    /// `canonical_title` under [`crate::normalize_title`], and the only title the matcher
    /// compares. The trigram index sits on this column, not on the display one.
    pub normalized_title: String,
    /// Synopsis, `None` until a provider supplies one. Feeds the full-text search vector
    /// alongside the canonical title.
    pub description: Option<String>,
    /// Cover image on whichever provider supplied it, as a link. No image is fetched or stored.
    pub cover_url: Option<String>,
    /// Medium, `Unknown` until a provider states one.
    pub content_type: ContentType,
    /// Publication status, `Unknown` until a provider states one.
    pub status: SeriesStatus,
    /// Year of first publication, `None` when no provider states one.
    pub release_year: Option<i32>,
    /// When the canonical series was created, which is when some provider was first seen
    /// carrying it.
    pub created_at: OffsetDateTime,
    /// When an ingest or a merge last wrote the row.
    pub updated_at: OffsetDateTime,
}

/// An alternative title that aids cross-provider matching.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SeriesTitle {
    /// The canonical series this title is an alias for.
    pub series_id: SeriesId,
    /// The alias as the provider spells it.
    pub title: String,
    /// `title` under [`crate::normalize_title`]. It is half the primary key, so two providers
    /// spelling one alias differently collapse to a single row.
    pub normalized: String,
}

/// A genre/tag.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct Tag {
    /// Primary key.
    pub id: TagId,
    /// URL-safe key, unique across tags, and what a browse filter carries.
    pub slug: String,
    /// Tag as displayed.
    pub name: String,
}

/// An author/artist credit.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct Author {
    /// Primary key.
    pub id: AuthorId,
    /// URL-safe key, unique across authors.
    pub slug: String,
    /// Credit as displayed.
    pub name: String,
}

/// The join: one canonical series existing at one provider under one path.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SeriesSource {
    /// Primary key.
    pub id: SeriesSourceId,
    /// The canonical series this provider page was matched to.
    pub series_id: SeriesId,
    /// The provider hosting it. `(provider_id, source_path)` is unique, so a page belongs to
    /// at most one source row.
    pub provider_id: ProviderId,
    /// RELATIVE path; resolve against `provider.base_url` at read time.
    pub source_path: String,
    /// The title this provider spells the series under, `None` when the adapter found none.
    pub provider_title: Option<String>,
    /// Hash of last-seen metadata + chapter list, for cheap change detection.
    pub content_hash: Option<Vec<u8>>,
    /// Chapter rows recorded at the last scan of this source. A raw row count: part releases
    /// each count, unlike the whole-chapter figure the series page shows.
    pub chapter_count: i32,
    /// When a scan last read this page, `None` until the first one finishes.
    pub last_scanned_at: Option<OffsetDateTime>,
    /// Health of this one page, which can differ from the provider's.
    pub state: ProviderState,
}

/// A chapter link under a [`SeriesSource`]. Never image data.
///
/// Carries no id: `(series_source_id, number)` identifies a chapter, and migration 0055 made that
/// the primary key. `path` is the **expanded** site-relative link — the repo layer undoes the
/// prefix compression `chapters.path` is stored with, so nothing above it has to know about it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Chapter {
    /// The provider page this link was found on.
    pub series_source_id: SeriesSourceId,
    /// Chapter number, fractional for part releases. With the source id it is the identity.
    pub number: f64,
    /// Chapter title, `None` when the provider publishes only a number.
    pub title: Option<String>,
    /// RELATIVE link to the chapter page.
    pub path: String,
    /// When the provider says the chapter went up, `None` when it publishes no date.
    pub published_at: Option<OffsetDateTime>,
    /// When a scan first saw the chapter. New-chapter notifications and the recency counters
    /// are measured on this, not on `published_at`.
    pub discovered_at: OffsetDateTime,
}

/// A user account. The password hash lives only in the `db`/`auth` layers, never here.
///
/// Carries no authorization state — permissions are resolved per request from a separate
/// grant store, so an identity record cannot go stale against a revoked grant.
/// [`AccountStatus`] lives here because it is identity, not authorization.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct User {
    /// Primary key.
    pub id: UserId,
    /// Sign-in address. The column is `citext`, so uniqueness ignores case.
    pub email: String,
    /// Display handle, also case-insensitively unique.
    pub username: String,
    /// Identity lifecycle. Not authorization, which is resolved per request from the grants.
    pub status: AccountStatus,
    /// When the account was registered.
    pub created_at: OffsetDateTime,
}

/// A watchlist membership with per-title notification opt-in.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WatchlistEntry {
    /// Who is watching.
    pub user_id: UserId,
    /// What they are watching.
    pub series_id: SeriesId,
    /// Where the user says they are with the series.
    pub status: WatchStatus,
    /// Per-title opt-in, applied on top of the account's own notification preferences.
    pub notify: bool,
    /// When the series entered this user's watchlist.
    pub added_at: OffsetDateTime,
}

/// A user's read position within a series.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadProgress {
    /// Whose position this is.
    pub user_id: UserId,
    /// The series the position is in.
    pub series_id: SeriesId,
    /// Highest chapter number marked read, on the same scale as [`Chapter::number`].
    pub last_read_number: f64,
    /// When the position last moved.
    pub updated_at: OffsetDateTime,
}

/// An in-app notification.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Notification {
    /// Primary key.
    pub id: NotificationId,
    /// Recipient.
    pub user_id: UserId,
    /// Discriminator the client switches on to read `payload`.
    pub kind: String,
    /// Body, shaped by `kind`. The notifier owns that shape; this crate does not parse it.
    pub payload: serde_json::Value,
    /// When the user opened it. `None` is what makes a notification unread.
    #[serde(with = "time::serde::rfc3339::option")]
    pub read_at: Option<OffsetDateTime>,
    /// When the notification was raised.
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

/// A scan run (progress + audit; mirrors `JetStream` dispatch).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ScanRun {
    /// Primary key.
    pub id: ScanRunId,
    /// `None` for a run covering every enabled provider, and also once the provider it named
    /// has been deleted.
    pub provider_id: Option<ProviderId>,
    /// Fixed at dispatch. A run rebuilds the archive or reads the latest feed, never both.
    pub mode: ScanMode,
    /// Lifecycle. `Completed`, `Failed` and `Cancelled` are terminal.
    pub state: RunState,
    /// Tasks the run dispatched.
    pub total_tasks: i32,
    /// Tasks that reached `Done`.
    pub done_tasks: i32,
    /// Tasks that reached `Failed`, incremented by the same statement that records the error.
    pub failed_tasks: i32,
    /// When the first task was claimed, `None` while the run is still queued.
    #[serde(with = "time::serde::rfc3339::option")]
    #[schema(value_type = Option<String>)]
    pub started_at: Option<OffsetDateTime>,
    /// When the run settled, `None` while it is still going.
    #[serde(with = "time::serde::rfc3339::option")]
    #[schema(value_type = Option<String>)]
    pub finished_at: Option<OffsetDateTime>,
    /// When the run was enqueued.
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String)]
    pub created_at: OffsetDateTime,
}

/// A single unit of scan work.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScanTask {
    /// Primary key.
    pub id: ScanTaskId,
    /// The run that dispatched this task.
    pub run_id: ScanRunId,
    /// `catalog_page` | `series` | `latest_feed`.
    pub kind: String,
    /// Task target, e.g. `{"path":"/manga/x","page":3}`.
    pub target: serde_json::Value,
    /// Lifecycle. `Done`, `Failed` and `Skipped` are terminal and no claim can leave them.
    pub state: TaskState,
    /// Claims taken on this task, incremented by the claim itself rather than by the retry.
    pub attempts: i16,
    /// Worker holding the current claim, `None` before the first one.
    pub worker_id: Option<String>,
    /// Why the task failed. Written once, with the move to `Failed`, which is terminal.
    pub error: Option<String>,
    /// When a worker last claimed the task.
    pub claimed_at: Option<OffsetDateTime>,
    /// When the task settled.
    pub finished_at: Option<OffsetDateTime>,
}

impl Provider {
    /// Resolve a relative path stored under this provider into an absolute URL.
    ///
    /// # Errors
    /// Propagates [`crate::link::ResolveError`] from the resolver.
    pub fn resolve(&self, path: &str) -> Result<String, crate::link::ResolveError> {
        crate::link::resolve_link(&self.base_url, path)
    }
}
