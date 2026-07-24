//! # tankovault-domain
//!
//! Pure domain types for the `TankoVault` manga aggregator: entities, typed ids, enums, the
//! migration-safe [`resolve_link`] resolver, crawl [`Politeness`], and title
//! [`normalize_title`]. No I/O, no async, no persistence — this crate is the shared
//! vocabulary every other crate speaks.
//!
//! Key invariants (design Appendix A):
//! 1. Store relative paths, resolve at read time via the single [`resolve_link`] fn.
//! 2. Links and metadata only — there is no image/content type anywhere in this crate.

pub mod entities;
pub mod enums;
pub mod ids;
pub mod link;
pub mod normalize;
pub mod politeness;

pub use entities::{
    Author, Chapter, Notification, Provider, ReadProgress, ScanRun, ScanTask, Series, SeriesSource,
    SeriesTitle, Tag, User, WatchlistEntry,
};
pub use enums::{
    AdapterKind, ContentType, ParseEnumError, ProviderState, RunState, ScanMode, SeriesStatus,
    TaskState, UserRole, WatchStatus,
};
pub use ids::{
    AuthorId, ChapterId, NotificationId, ProviderId, ScanRunId, ScanTaskId, SeriesId,
    SeriesSourceId, TagId, UserId,
};
pub use link::{ResolveError, resolve_link};
pub use normalize::normalize_title;
pub use politeness::Politeness;
