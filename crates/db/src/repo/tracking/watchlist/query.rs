//! The query vocabulary that drives the Watchlist board (sort, order, filter, cursor) and the
//! view types a page is assembled into.

use tankovault_domain::{ProviderState, SeriesId, SeriesSourceId, WatchStatus};
use time::OffsetDateTime;

/// One watchlist row with everything the ledger needs to render and order it.
///
/// The series title and cover, the reader's progress, and the release and source facts the list
/// sorts and groups on, so the Watchlist renders a page without an N+1 `series_detail` fetch
/// (frontend §9.3).
#[derive(Debug, Clone)]
pub struct WatchlistCard {
    /// The series this row tracks.
    pub series_id: SeriesId,
    /// Its canonical title.
    pub series_title: String,
    /// Its cover, `None` when no provider supplied one.
    pub cover_url: Option<String>,
    /// Where the reader says they are with it.
    pub status: WatchStatus,
    /// The per-title notification opt-in.
    pub notify: bool,
    /// When the series entered this reader's watchlist.
    pub added_at: OffsetDateTime,
    /// The reader's progress frontier, `None` before they have marked anything.
    pub last_read_number: Option<f64>,
    /// Distinct chapters strictly above the user's progress, across all sources.
    pub unread: i64,
    /// Distinct whole chapters at or below the user's progress, the progress bar's numerator.
    ///
    /// Not [`Self::last_read_number`]: in a catalogue with gaps the frontier exceeds the chapters
    /// that exist below it, and a bar over [`Self::total_chapters`] would read past 100%. Counted
    /// with the denominator's `floor(number)` `DISTINCT`, so the two are commensurable.
    pub read_count: i64,
    /// The lowest unread chapter, or `None` when the reader is caught up.
    pub next_unread: Option<NextUnread>,
    /// Distinct whole chapters known across all sources, the progress denominator.
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
    /// [`continue_reading`](crate::repo::tracking::dashboard::continue_reading) order on it, and
    /// `published_at` is null for every provider that does not print a date. Anything else would
    /// top "Today" with a row the feed dates to last year.
    pub latest_chapter_at: Option<OffsetDateTime>,
    /// Display name of the provider this series is primarily carried by, for the row submeta.
    ///
    /// Derived and user-agnostic: the source with the most chapters, tie-broken by the most recent
    /// scan and then the provider slug, so the choice is stable between requests. The reader's
    /// preference ([`Self::pinned_source_id`] and their provider order) is resolved where a
    /// source is *opened*, keeping a per-user join off the ledger's hot path for a label.
    pub preferred_source_name: Option<String>,
    /// Distinct providers carrying this series.
    pub source_count: i64,
    /// Whether the preferred source, or the provider behind it, is in a non-`active` state.
    ///
    /// Read off `series_sources.state` and `providers.state`, which the scan pipeline maintains,
    /// so "source offline" agrees with the Providers console. Scoped to the *preferred* source:
    /// a blocked secondary source does not block the reader.
    pub source_degraded: bool,
    /// Whether this series is opted out of external sync (design v2 §A.5).
    pub sync_excluded: bool,
    /// The source this reader pinned for this series, if any.
    ///
    /// The reader's explicit override, unlike the derived [`Self::preferred_source_name`]
    /// beside it: nothing in this crate resolves the two against each other, because the pin
    /// is honoured where a source is *opened* (the Series view) and the derived name is what
    /// the ledger row *displays*. Cleared by the schema when the pinned source goes away.
    pub pinned_source_id: Option<SeriesSourceId>,
    /// Every provider carrying this series, preferred first.
    ///
    /// Empty on [`watchlist_page`](super::watchlist_page) until `attach_sources` runs: a second
    /// statement keyed on the page's ids, since an `array_agg` of four columns would be paid on
    /// every row whether or not the viewport renders it.
    pub sources: Vec<WatchlistSource>,
}

/// The next chapter the reader has not read, for the ledger's `Next unread` column.
#[derive(Debug, Clone, PartialEq)]
pub struct NextUnread {
    /// The chapter number, parts included: `152.5` is a legitimate next read.
    pub number: f64,
    /// The provider's chapter title, when it publishes one.
    pub title: Option<String>,
    /// When it was discovered, matching [`WatchlistCard::latest_chapter_at`]'s clock.
    pub released_at: OffsetDateTime,
}

/// One provider carrying a series, for the ledger's `Sources` column.
#[derive(Debug, Clone, PartialEq)]
pub struct WatchlistSource {
    /// The provider slug: the monogram tile's letters and a stable key for the client.
    pub code: String,
    /// The provider's display name.
    pub name: String,
    /// The *effective* state: the series-source's own, unless the provider behind it is worse.
    ///
    /// One value, because the reader's question is "can I read this here?", and a healthy
    /// listing on a blocked provider answers no.
    pub state: ProviderState,
    /// Whether this is the source [`WatchlistCard::preferred_source_name`] names.
    pub preferred: bool,
}

/// How the watchlist list is ordered.
///
/// A closed enum, as [`SeriesSort`](crate::repo::catalog::SeriesSort) is: an unrecognised token
/// silently falling back to the default gives a page that looks right and is ordered wrong. The
/// handler parses this and answers `400`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum WatchlistSort {
    /// Newest release first: the default, and the order the Today, This week and Earlier
    /// grouping is meaningful under.
    #[default]
    Released,
    /// Most unread chapters first.
    Unread,
    /// Most recently added first.
    Added,
    /// Canonical title, A to Z.
    Title,
    /// Furthest through first.
    Progress,
}

impl WatchlistSort {
    /// Returns the wire token, which is also the value bound into the `ORDER BY` `CASE`.
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

    /// Returns the direction that reads as "most interesting first" for this key.
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
    /// Largest or newest first.
    #[default]
    Desc,
    /// Smallest or oldest first.
    Asc,
}

impl WatchlistOrder {
    /// Returns the wire token, which is also the value bound into the `ORDER BY` `CASE`.
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
    /// Not a list filter: it lets [`watchlist_card`](super::watchlist_card) reuse this statement
    /// instead of a fifth copy of the unread predicate, the drift the
    /// `unread_predicate_agrees_everywhere` test catches.
    pub series_id: Option<SeriesId>,
    /// Narrow to one watch status, which is what the tab strip selects.
    pub status: Option<WatchStatus>,
    /// Free-text filter over the canonical title, alternative titles, tag names and author
    /// names.
    pub query: Option<String>,
    /// Drop rows the reader is caught up on.
    pub unread_only: bool,
    /// Only rows whose newest chapter was discovered at or after this instant.
    ///
    /// An instant computed at the edge, not a window: the group buckets use rolling windows off
    /// the database clock, and an instant makes the filter evaluate once per request.
    pub released_since: Option<OffsetDateTime>,
    /// Only rows whose preferred source is degraded; see [`WatchlistCard::source_degraded`].
    pub source_issues: bool,
    /// Which key to order on.
    pub sort: WatchlistSort,
    /// Which direction to order that key in.
    pub order: WatchlistOrder,
    /// Rows per page.
    pub limit: i64,
    /// Rows to skip, ignored once `cursor` is set.
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

/// Wraps a trimmed search term as the `ILIKE` pattern the watchlist statements bind.
///
/// The three statements search four columns each, so escaping in SQL would mean twelve copies of
/// a nested `replace(replace(replace(…)))`; doing it once here keeps the predicate readable and
/// the escape impossible to omit from one branch.
///
/// `\`, `%` and `_` are ordinary characters to someone typing in the filter box and wildcards to
/// `ILIKE`. Without the escape a search for `50%` widens to every row, and a term ending in `\`
/// is a dangling escape that makes Postgres reject the statement outright.
pub(super) fn search_pattern(term: &str) -> String {
    let mut pattern = String::with_capacity(term.len() + 2);
    pattern.push('%');
    for ch in term.chars() {
        if matches!(ch, '\\' | '%' | '_') {
            pattern.push('\\');
        }
        pattern.push(ch);
    }
    pattern.push('%');
    pattern
}

/// Where a keyset page resumes: the sort key of the previous page's last row, plus its id.
///
/// **Read out of the row the database ordered on, never recomputed.** `progress` orders on
/// `last_read_whole_number / total_chapters`, and a Rust copy of that expression could drift from
/// the `ORDER BY` and silently repeat or skip rows. `fetch_page` selects the key it sorted by.
///
/// The id is the unique tiebreaker in both the order and the seek predicate; without it, rows
/// tied on `unread` give a keyset page the repeat-and-skip defect of `OFFSET`.
#[derive(Debug, Clone, PartialEq)]
pub struct WatchlistCursor {
    /// The numeric sort key, for every order but `title`.
    pub num: Option<f64>,
    /// The text sort key, for `title`.
    pub text: Option<String>,
    /// The tiebreak, since a sort key is not unique across rows.
    pub series_id: SeriesId,
}

impl WatchlistCursor {
    /// Returns whether the row this cursor names had no sort key, such as a series with no
    /// chapters under `released`. Such rows order last in **both** directions (`NULLS LAST`), so
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
    /// Entries held at `reading`.
    pub reading: i64,
    /// Entries held at `planned`.
    pub planned: i64,
    /// Entries held at `paused`.
    pub paused: i64,
    /// Entries held at `completed`.
    pub completed: i64,
    /// Entries held at `dropped`.
    pub dropped: i64,
    /// The sum of the five, i.e. the `All` tab.
    pub all: i64,
    /// How many of those rows have a degraded preferred source, for the `Source issues` chip.
    ///
    /// Not derived from the page, since the chip answers for the whole filtered list. Counted
    /// under the tab counts' filters, `source_issues` included, which is harmless: with it on,
    /// every remaining row is degraded and the chip reads the size of the list.
    pub source_issues: i64,
}

impl WatchlistCounts {
    /// Adds one status group's tallies.
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
/// **Rolling windows, not calendar days.** The server does not know the reader's timezone, so a
/// `date_trunc('day', now())` "Today" is wrong by up to a day off UTC. "Within the last 24
/// hours" cannot disagree with the reader's clock, and it is what
/// [`WatchlistFilter::released_since`] means, so the `Released: 24h` filter and the `Today` group
/// contain the same rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReleaseBucket {
    /// Discovered within the last 24 hours.
    Today,
    /// Discovered within the last 7 days, but not the last 24 hours.
    ThisWeek,
    /// Everything older, including rows with no chapters, which have no release instant to band
    /// and would otherwise vanish from the grouped list.
    Earlier,
}

impl ReleaseBucket {
    /// Returns the wire token, which is also what the grouping `CASE` emits.
    #[must_use]
    pub fn as_token(self) -> &'static str {
        match self {
            Self::Today => "today",
            Self::ThisWeek => "week",
            Self::Earlier => "earlier",
        }
    }

    /// Parses the token the grouping `CASE` emitted.
    pub(super) fn from_token(token: &str) -> Self {
        match token {
            "today" => Self::Today,
            "week" => Self::ThisWeek,
            _ => Self::Earlier,
        }
    }

    /// Newest band first, the order the group headers render in.
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
    /// Which band this header covers.
    pub bucket: ReleaseBucket,
    /// Rows falling in it.
    pub title_count: i64,
    /// Unread chapters those rows carry between them.
    pub chapter_count: i64,
}

/// A page of the filtered watchlist, plus everything the chrome around it needs: the tab
/// counts, the group-header aggregates, and the total for the pager.
#[derive(Debug, Clone)]
pub struct WatchlistPage {
    /// The page, in the filter's order.
    pub items: Vec<WatchlistCard>,
    /// Rows matching the *whole* filter, `status` included: the pager's denominator.
    pub total: i64,
    /// What each tab would show, so the strip needs no second request.
    pub counts: WatchlistCounts,
    /// Newest band first. Empty bands are omitted, so a list with nothing released this week
    /// renders `Today` then `Earlier`.
    pub groups: Vec<ReleaseGroup>,
    /// Where the next keyset page resumes, or `None` at the end of the list.
    ///
    /// `None` when the page came back short of `limit`. A full page is not proof more rows
    /// exist, so the last page of an exactly divisible list costs one empty follow-up, the cheap
    /// side of the trade against claiming the list ended early.
    pub next_cursor: Option<WatchlistCursor>,
}
