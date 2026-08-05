//! The query vocabulary the Watchlist board is driven by — sort, order, filter, cursor —
//! and the view types a page is assembled into.

use tankovault_domain::{ProviderState, SeriesId, WatchStatus};
use time::OffsetDateTime;

/// A watchlist row enriched with the series title + cover, the user's progress, and the
/// release/source facts the list sorts and groups on — so the Watchlist renders each row
/// without an N+1 `series_detail` fetch (frontend §9.3).
#[derive(Debug, Clone)]
pub struct WatchlistCard {
    pub series_id: SeriesId,
    pub series_title: String,
    pub cover_url: Option<String>,
    pub status: WatchStatus,
    pub notify: bool,
    pub added_at: OffsetDateTime,
    pub last_read_number: Option<f64>,
    /// Distinct chapters strictly above the user's progress, across all sources.
    pub unread: i64,
    /// Distinct whole chapters at or below the user's progress — the progress bar's numerator.
    ///
    /// Not [`Self::last_read_number`]: a catalogue with gaps (no chapter 5) makes the frontier
    /// larger than the number of chapters that actually exist below it, so a bar drawn from the
    /// frontier over [`Self::total_chapters`] reads past 100%. Counted with the same
    /// `floor(number)` `DISTINCT` as the denominator, so the two are commensurable by
    /// construction.
    pub read_count: i64,
    /// The lowest unread chapter, or `None` when the reader is caught up.
    pub next_unread: Option<NextUnread>,
    /// Distinct whole chapters known across all sources — the progress denominator.
    ///
    /// Counted with the same `floor(number)` as [`Self::unread`], so a source publishing part
    /// releases cannot push `last_read / total` above 1.
    pub total_chapters: i64,
    /// The highest chapter number known across all sources.
    ///
    /// This is the highest-*numbered* chapter, while [`Self::latest_chapter_at`] is the
    /// most-recently-*discovered* one; a back-fill of an old chapter makes the two disagree.
    /// That is deliberate. The number answers "how far does this series go" and so belongs
    /// with `total_chapters`; the timestamp answers "when did something last happen here" and
    /// so has to match what the feed orders on.
    pub latest_chapter_number: Option<f64>,
    /// When the newest chapter was discovered, across all sources.
    ///
    /// `discovered_at`, not `published_at`: [`feed`](crate::repo::tracking::dashboard::feed) and
    /// [`continue_reading`](crate::repo::tracking::dashboard::continue_reading) both order on `discovered_at`,
    /// and `published_at` is null for every provider that does not print a date. Ordering the
    /// watchlist on anything else would put a row at the top of "Today" that the feed — one
    /// click away, over the same chapters — dates to last year.
    pub latest_chapter_at: Option<OffsetDateTime>,
    /// Display name of the provider this series is primarily carried by, for the row submeta.
    ///
    /// There is no per-user preferred source yet (the Series view notes the same gap), so
    /// "preferred" is derived: the source with the most chapters, tie-broken by the most
    /// recent scan and then the provider slug so the choice is stable between requests rather
    /// than whatever the planner emitted first.
    pub preferred_source_name: Option<String>,
    /// Distinct providers carrying this series.
    pub source_count: i64,
    /// Whether the preferred source — or the provider behind it — is in a non-`active` state.
    ///
    /// Read off the existing `series_sources.state` / `providers.state` health rather than
    /// derived from the last scan run: those columns are what the scan pipeline already
    /// maintains, and a row that says "source offline" has to agree with the Providers console
    /// that shows the same state. Scoped to the *preferred* source deliberately: a series with
    /// three sources where a secondary is blocked is not a series the reader is blocked on.
    pub source_degraded: bool,
    /// Whether this series is opted out of external sync (design v2 §A.5).
    pub sync_excluded: bool,
    /// Every provider carrying this series, preferred first.
    ///
    /// Empty on [`watchlist_page`](super::watchlist_page) until `attach_sources` has run — it is a second statement
    /// keyed on the page's ids rather than an aggregate folded into the row, because an
    /// `array_agg` of four columns per row is paid for every row of every page whether or not
    /// the viewport is wide enough to render it.
    pub sources: Vec<WatchlistSource>,
}

/// The next chapter the reader has not read, for the ledger's `Next unread` column.
#[derive(Debug, Clone, PartialEq)]
pub struct NextUnread {
    /// The chapter number, parts included — `152.5` is a legitimate next read.
    pub number: f64,
    /// The provider's chapter title, when it publishes one.
    pub title: Option<String>,
    /// When it was discovered, matching [`WatchlistCard::latest_chapter_at`]'s clock.
    pub released_at: OffsetDateTime,
}

/// One provider carrying a series, for the ledger's `Sources` column.
#[derive(Debug, Clone, PartialEq)]
pub struct WatchlistSource {
    /// The provider slug — the monogram tile's letters and a stable key for the client.
    pub code: String,
    pub name: String,
    /// The *effective* state: the series-source's own, unless the provider behind it is worse.
    ///
    /// One value rather than two, because the reader's question is "can I read this here?" and
    /// a healthy listing on a blocked provider answers it no.
    pub state: ProviderState,
    /// Whether this is the source [`WatchlistCard::preferred_source_name`] names.
    pub preferred: bool,
}

/// How the watchlist list is ordered.
///
/// A closed enum rather than a passed-through string, for the reason
/// [`SeriesSort`](crate::repo::catalog::SeriesSort) is: an unrecognised token that silently
/// falls back to the default produces a page that looks right and is ordered wrong. The
/// handler parses this and answers `400`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum WatchlistSort {
    /// Newest release first — the default, and the order the Today/This week/Earlier grouping
    /// is meaningful under.
    #[default]
    Released,
    Unread,
    Added,
    Title,
    Progress,
}

impl WatchlistSort {
    /// The wire token, which is also the value bound into the `ORDER BY` `CASE`.
    #[must_use]
    pub fn as_token(self) -> &'static str {
        match self {
            Self::Released => "released",
            Self::Unread => "unread",
            Self::Added => "added",
            Self::Title => "title",
            Self::Progress => "progress",
        }
    }

    /// The direction that reads as "most interesting first" for this key.
    ///
    /// Every order except `title` is a magnitude the reader wants the largest of; `title` is
    /// the one people expect A→Z. Without this the sort control had to ship a second control
    /// beside it just so picking "Title" did not answer Z→A.
    #[must_use]
    pub fn natural_order(self) -> WatchlistOrder {
        match self {
            Self::Title => WatchlistOrder::Asc,
            _ => WatchlistOrder::Desc,
        }
    }
}

impl std::str::FromStr for WatchlistSort {
    type Err = ParseWatchlistSortError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "released" => Ok(Self::Released),
            "unread" => Ok(Self::Unread),
            "added" => Ok(Self::Added),
            "title" => Ok(Self::Title),
            "progress" => Ok(Self::Progress),
            other => Err(ParseWatchlistSortError(other.to_owned())),
        }
    }
}

/// Raised when a client asks for a watchlist order that does not exist.
#[derive(Debug, Clone, thiserror::Error)]
#[error("unknown watchlist sort order: {0:?}")]
pub struct ParseWatchlistSortError(pub String);

/// Ascending or descending, for whichever key [`WatchlistSort`] selected.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum WatchlistOrder {
    #[default]
    Desc,
    Asc,
}

impl WatchlistOrder {
    /// The wire token, which is also the value bound into the `ORDER BY` `CASE`.
    #[must_use]
    pub fn as_token(self) -> &'static str {
        match self {
            Self::Desc => "desc",
            Self::Asc => "asc",
        }
    }
}

impl std::str::FromStr for WatchlistOrder {
    type Err = ParseWatchlistSortError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "desc" => Ok(Self::Desc),
            "asc" => Ok(Self::Asc),
            other => Err(ParseWatchlistSortError(other.to_owned())),
        }
    }
}

/// Server-side filter/sort/paginate criteria for the Watchlist list (frontend §9.4).
///
/// Every field is optional; `None`/`false` means "no constraint". Filtering and sorting are
/// server-side because the list they describe is ~600 rows for a real account and five figures
/// of chapters: shipping the whole thing to the client to sort it is what the redesign exists
/// to stop.
#[derive(Debug, Clone)]
pub struct WatchlistFilter {
    /// Narrow to a single series.
    ///
    /// Not a list filter — it exists so [`watchlist_card`](super::watchlist_card) can reuse this statement instead of
    /// growing a fifth hand-written copy of the unread predicate, which is the drift the
    /// `unread_predicate_agrees_everywhere` test exists to catch.
    pub series_id: Option<SeriesId>,
    pub status: Option<WatchStatus>,
    /// Free-text filter over the canonical title, alternative titles, tag names and author
    /// names.
    pub query: Option<String>,
    pub unread_only: bool,
    /// Only rows whose newest chapter was discovered at or after this instant.
    ///
    /// An instant rather than a window, computed at the edge: the group buckets below use
    /// rolling windows off the database clock, and taking the cutoff as an instant keeps the
    /// filter honest about being evaluated once per request rather than per row.
    pub released_since: Option<OffsetDateTime>,
    /// Only rows whose preferred source is degraded — see [`WatchlistCard::source_degraded`].
    pub source_issues: bool,
    pub sort: WatchlistSort,
    pub order: WatchlistOrder,
    pub limit: i64,
    pub offset: i64,
    /// Resume after this row instead of counting `offset` rows into the result.
    ///
    /// Takes precedence over [`Self::offset`] when set; the two are never combined.
    pub cursor: Option<WatchlistCursor>,
}

impl Default for WatchlistFilter {
    fn default() -> Self {
        Self {
            series_id: None,
            status: None,
            query: None,
            unread_only: false,
            released_since: None,
            source_issues: false,
            sort: WatchlistSort::default(),
            order: WatchlistOrder::default(),
            limit: 60,
            offset: 0,
            cursor: None,
        }
    }
}

/// Where a keyset page resumes: the sort key of the previous page's last row, plus its id.
///
/// **Read out of the row the database ordered on, never recomputed.** `progress` orders on
/// `last_read_whole_number / total_chapters`, and a Rust copy of that expression would be a
/// second definition of the sort order free to drift from the `ORDER BY` — which produces a page
/// that silently repeats or skips rows rather than failing. `fetch_page` therefore selects the
/// key it sorted by and hands it back.
///
/// The id is not decoration. Hundreds of rows tie on `unread` in a 600-entry list, and without
/// a unique tiebreaker in both the order and the seek predicate a keyset page has the same
/// repeat-and-skip defect as `OFFSET`.
#[derive(Debug, Clone, PartialEq)]
pub struct WatchlistCursor {
    /// The numeric sort key, for every order but `title`.
    pub num: Option<f64>,
    /// The text sort key, for `title`.
    pub text: Option<String>,
    pub series_id: SeriesId,
}

impl WatchlistCursor {
    /// Whether the row this cursor names had no sort key at all — a series with no chapters
    /// under `released`, say. Such rows order last in **both** directions (`NULLS LAST`), so
    /// only other keyless rows can follow one.
    pub(super) fn key_is_null(&self) -> bool {
        self.num.is_none() && self.text.is_none()
    }
}

/// How many entries the user holds at each status, under every filter *except* `status`.
///
/// Excluding `status` from its own counts is the point: the tab strip has to answer "how many
/// would I see if I clicked this tab", and a count that already had the current tab's filter
/// applied would read `0` on every tab but the active one.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WatchlistCounts {
    pub reading: i64,
    pub planned: i64,
    pub paused: i64,
    pub completed: i64,
    pub dropped: i64,
    /// The sum of the five, i.e. the `All` tab.
    pub all: i64,
    /// How many of those rows have a degraded preferred source, for the `Source issues` chip.
    ///
    /// It lives here rather than being derived from the page because the chip has to answer
    /// "how many across the whole filtered list", and the page is sixty rows of it. Counted
    /// under the same filters as the tab counts — including `source_issues` itself, which is
    /// harmless: with that filter on, every remaining row is degraded and the chip correctly
    /// reads the size of what you are looking at.
    pub source_issues: i64,
}

impl WatchlistCounts {
    /// Add one status group's tallies.
    pub(super) fn add(&mut self, status: WatchStatus, n: i64, degraded: i64) {
        match status {
            WatchStatus::Reading => self.reading += n,
            WatchStatus::Planned => self.planned += n,
            WatchStatus::Paused => self.paused += n,
            WatchStatus::Completed => self.completed += n,
            WatchStatus::Dropped => self.dropped += n,
        }
        self.all += n;
        self.source_issues += degraded;
    }
}

/// Which release-recency band a row falls into, i.e. which group header it renders under.
///
/// **Rolling windows, not calendar days.** The server has no idea what timezone the reader is
/// in, so a `date_trunc('day', now())` bucket labelled "Today" is wrong by up to a day for
/// anyone not on UTC. "Within the last 24 hours" cannot disagree with the reader's clock, and
/// it is also exactly what [`WatchlistFilter::released_since`] means — so the `Released: 24h`
/// filter and the `Today` group always contain the same rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReleaseBucket {
    /// Discovered within the last 24 hours.
    Today,
    /// Discovered within the last 7 days, but not the last 24 hours.
    ThisWeek,
    /// Everything older — including rows with no chapters at all, which have no release
    /// instant to band and would otherwise vanish from the grouped list.
    Earlier,
}

impl ReleaseBucket {
    /// The wire token, which is also what the grouping `CASE` emits.
    #[must_use]
    pub fn as_token(self) -> &'static str {
        match self {
            Self::Today => "today",
            Self::ThisWeek => "week",
            Self::Earlier => "earlier",
        }
    }

    /// Parse the token the grouping `CASE` emitted.
    pub(super) fn from_token(token: &str) -> Self {
        match token {
            "today" => Self::Today,
            "week" => Self::ThisWeek,
            _ => Self::Earlier,
        }
    }

    /// Newest band first — the order the group headers render in.
    pub(super) fn rank(self) -> u8 {
        match self {
            Self::Today => 0,
            Self::ThisWeek => 1,
            Self::Earlier => 2,
        }
    }
}

/// One group header's aggregates: how many titles fall in the band and how many unread
/// chapters they carry between them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReleaseGroup {
    pub bucket: ReleaseBucket,
    pub title_count: i64,
    pub chapter_count: i64,
}

/// A page of the filtered watchlist, plus everything the chrome around it needs: the tab
/// counts, the group-header aggregates, and the total for the pager.
#[derive(Debug, Clone)]
pub struct WatchlistPage {
    pub items: Vec<WatchlistCard>,
    /// Rows matching the *whole* filter, `status` included — the pager's denominator.
    pub total: i64,
    pub counts: WatchlistCounts,
    /// Newest band first. Empty bands are omitted, so a list with nothing released this week
    /// renders `Today` then `Earlier`.
    pub groups: Vec<ReleaseGroup>,
    /// Where the next keyset page resumes, or `None` at the end of the list.
    ///
    /// `None` when the page came back short of `limit`. A full page is not proof more rows
    /// exist, so the last page of an exactly-divisible list costs one empty follow-up — which
    /// is the cheap side of the trade against claiming the list ended when it had not.
    pub next_cursor: Option<WatchlistCursor>,
}
