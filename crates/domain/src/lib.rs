//! # tankovault-domain
//!
//! Pure domain types for the `TankoVault` manga aggregator: entities, typed ids, enums, the
//! [`Permission`] and [`Feature`] registries, the migration-safe [`resolve_link`] resolver,
//! crawl [`Politeness`], and title [`normalize_title`]. No I/O, no async, no persistence —
//! this crate is the shared vocabulary every other crate speaks.
//!
//! Authorization is **permission-based**, not role-based: see [`permissions`] for why the
//! ordered `user < operator < admin` tier was removed rather than extended. Every product
//! capability is switchable at runtime from the control plane; the registry of what is
//! switchable lives in [`features`].
//!
//! Key invariants (design Appendix A):
//! 1. Store relative paths, resolve at read time via the single [`resolve_link`] fn.
//! 2. Links and metadata only — there is no image/content type anywhere in this crate.

pub mod entities;
pub mod enums;
pub mod features;
pub mod ids;
pub mod link;
pub mod normalize;
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
pub use normalize::normalize_title;
pub use permissions::{
    ParsePermissionError, Permission, PermissionGroup, PermissionPreset, PermissionSet,
};
pub use politeness::{BrowserEmulation, Politeness};
