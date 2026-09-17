//! The watchlist: which series a user tracks, at what status, and the enriched card the
//! Watchlist board renders.
//!
//! `entries` owns membership, `query` the sort/filter vocabulary a board request speaks,
//! `page` the assembly of a card page, `summary` the counts beside it, and `transfer` backups.

mod entries;
mod page;
mod query;
mod summary;
mod transfer;

pub use entries::{
    BULK_ID_LIMIT, PinOutcome, watchlist_bulk_remove, watchlist_bulk_update, watchlist_list,
    watchlist_remove, watchlist_set_pinned_source, watchlist_set_status, watchlist_status_get,
    watchlist_statuses_for_user, watchlist_track_if_absent, watchlist_upsert,
};
pub use page::{watchlist_card, watchlist_page};
pub use query::{
    NextUnread, ParseWatchlistSortError, ReleaseBucket, ReleaseGroup, WatchlistCard,
    WatchlistCounts, WatchlistCursor, WatchlistFilter, WatchlistOrder, WatchlistPage,
    WatchlistSort, WatchlistSource,
};
pub use summary::{WatchlistSummary, watchlist_summary};
pub use transfer::{
    Commit, ImportOptions, ImportOutcome, ImportRefusal, PendingEntry, SweepOutcome,
    watchlist_export, watchlist_import, watchlist_pending, watchlist_pending_clear,
    watchlist_pending_sweep,
};
