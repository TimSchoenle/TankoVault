//! The watchlist: which series a user tracks, at what status, and the enriched card the
//! Watchlist board renders.
//!
//! `entries` owns membership, `query` the sort/filter vocabulary a board request speaks,
//! `page` the assembly of a card page, and `summary` the counts beside it.

mod entries;
mod page;
mod query;
mod summary;

pub use entries::{
    BULK_ID_LIMIT, watchlist_bulk_remove, watchlist_bulk_update, watchlist_list, watchlist_remove,
    watchlist_set_status, watchlist_status_get, watchlist_statuses_for_user, watchlist_upsert,
};
pub use page::{watchlist_card, watchlist_page};
pub use query::{
    NextUnread, ParseWatchlistSortError, ReleaseBucket, ReleaseGroup, WatchlistCard,
    WatchlistCounts, WatchlistCursor, WatchlistFilter, WatchlistOrder, WatchlistPage,
    WatchlistSort, WatchlistSource,
};
pub use summary::{WatchlistSummary, watchlist_summary};
