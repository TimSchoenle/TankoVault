//! Pure domain types for the `TankoVault` manga aggregator: entities, typed ids, enums, the
//! [`Permission`] and [`Feature`] registries, the [`resolve_link`] resolver, crawl
//! [`Politeness`], and title [`normalize_title`]. No I/O, no async, no persistence.

pub mod entities;
pub mod enums;
pub mod features;
pub mod ids;
pub mod link;
pub mod matching;
pub mod metadata_priority;
pub mod normalize;
pub mod pacing;
pub mod permissions;
pub mod politeness;
pub mod ssrf;

pub use entities::{
    Author, Chapter, Notification, Provider, ReadProgress, ScanRun, ScanTask, Series, SeriesSource,
    SeriesTitle, Tag, User, WatchlistEntry,
};
pub use enums::{
    AccountStatus, AdapterKind, ContentType, ParseEnumError, ProviderState, RunState, ScanMode,
    SeriesStatus, TaskState, WatchStatus,
};
pub use features::{Feature, FeatureGroup, ParseFeatureError};
pub use ids::{
    AuthorId, ChapterId, NotificationId, ProviderId, ScanRunId, ScanTaskId, SeriesId,
    SeriesSourceId, TagId, UserId,
};
pub use link::{ResolveError, resolve_link};
pub use metadata_priority::{MetadataField, MetadataPriority, MetadataSource};
pub use normalize::{compact_key, normalize_title};
pub use pacing::{Pacer, PacingPolicy};
pub use permissions::{
    ParsePermissionError, Permission, PermissionGroup, PermissionPreset, PermissionSet,
};
pub use politeness::{BrowserEmulation, MIN_RPS, Politeness};
