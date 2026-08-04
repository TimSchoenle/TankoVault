//! The watchlist: which series a user tracks, at what status, and the enriched card the
//! Watchlist board renders.

use std::collections::HashMap;

use crate::error::DbResult;
use sqlx::{FromRow, PgExecutor, PgPool};
use tankovault_domain::{ProviderState, SeriesId, UserId, WatchStatus, WatchlistEntry};
use time::OffsetDateTime;
use uuid::Uuid;

/// Add or update a watchlist entry.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only — no other variant is reachable. Re-adding a tracked series
/// is an update, not [`crate::DbError::Conflict`]; a `series_id` that does not exist is a
/// foreign-key violation and so a 500 rather than a 404, which is safe only because callers
/// resolve the series first.
pub async fn watchlist_upsert<'e, E: PgExecutor<'e>>(
    exec: E,
    user_id: UserId,
    series_id: SeriesId,
    status: WatchStatus,
    notify: bool,
) -> DbResult<()> {
    sqlx::query!(
        "INSERT INTO watchlist_entries (user_id, series_id, status, notify) \
         VALUES ($1,$2,$3,$4) \
         ON CONFLICT (user_id, series_id) DO UPDATE \
            SET status = EXCLUDED.status, notify = EXCLUDED.notify, updated_at = now()",
        user_id.as_uuid(),
        series_id.as_uuid(),
        status as WatchStatus,
        notify,
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// Remove a watchlist entry.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only — no other variant is reachable. Removing something the user
/// was not tracking is `Ok(())`, not [`crate::DbError::NotFound`] — the count is not returned
/// at all, so untracking is idempotent and a caller cannot answer "was it there?".
pub async fn watchlist_remove<'e, E: PgExecutor<'e>>(
    exec: E,
    user_id: UserId,
    series_id: SeriesId,
) -> DbResult<()> {
    sqlx::query!(
        "DELETE FROM watchlist_entries WHERE user_id = $1 AND series_id = $2",
        user_id.as_uuid(),
        series_id.as_uuid(),
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// The largest number of ids a bulk watchlist operation will act on in one call.
///
/// The cap is enforced at the edge, not here — this constant is what the edge clamps to, so
/// the API and the repo cannot disagree about it. 200 is well past any selection a person
/// makes by hand (select-all over a filtered tab is the realistic maximum) and keeps the
/// `= ANY($2)` array small enough that the statement stays a single index scan.
pub const BULK_ID_LIMIT: usize = 200;

/// Apply a status and/or notify change to many watchlist entries at once, returning the ids
/// that were actually changed.
///
/// **Update, not upsert.** [`watchlist_upsert`] creates the entry it is given; this refuses to,
/// because the bulk bar operates on a selection made *from* the list — an id that is not on it
/// is a stale client, and inserting it would silently re-add a title the user had just removed
/// in another tab. Ids that matched nothing are simply absent from the result, which is what
/// lets the handler answer per-id rather than all-or-nothing.
///
/// `None` for either field leaves that column alone, so "mute 40 titles" does not also
/// normalise their statuses.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only — no other variant is reachable. An empty `series_ids`, and a
/// set of ids the user tracks none of, are both an empty `Vec` rather than
/// [`crate::DbError::NotFound`].
pub async fn watchlist_bulk_update<'e, E: PgExecutor<'e>>(
    exec: E,
    user_id: UserId,
    series_ids: &[Uuid],
    status: Option<WatchStatus>,
    notify: Option<bool>,
) -> DbResult<Vec<SeriesId>> {
    let changed = sqlx::query_scalar!(
        "UPDATE watchlist_entries \
            SET status = COALESCE($3, status), \
                notify = COALESCE($4, notify), \
                updated_at = now() \
          WHERE user_id = $1 AND series_id = ANY($2) \
          RETURNING series_id",
        user_id.as_uuid(),
        series_ids,
        status as Option<WatchStatus>,
        notify,
    )
    .fetch_all(exec)
    .await?;
    Ok(changed.into_iter().map(SeriesId::from_uuid).collect())
}

/// Remove many watchlist entries at once, returning the ids that were actually removed.
///
/// Unlike [`watchlist_remove`], which is idempotent and cannot tell you whether anything was
/// there, this reports what it deleted: removing 40 titles has to be able to say "38 of these
/// are gone, two were not yours" rather than claim a success it did not have.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only — no other variant is reachable. Ids the user was not tracking
/// are absent from the result rather than [`crate::DbError::NotFound`].
pub async fn watchlist_bulk_remove<'e, E: PgExecutor<'e>>(
    exec: E,
    user_id: UserId,
    series_ids: &[Uuid],
) -> DbResult<Vec<SeriesId>> {
    let removed = sqlx::query_scalar!(
        "DELETE FROM watchlist_entries \
          WHERE user_id = $1 AND series_id = ANY($2) \
          RETURNING series_id",
        user_id.as_uuid(),
        series_ids,
    )
    .fetch_all(exec)
    .await?;
    Ok(removed.into_iter().map(SeriesId::from_uuid).collect())
}

/// List a user's watchlist entries.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only — no other variant is reachable. An empty watchlist is an
/// empty `Vec`.
pub async fn watchlist_list<'e, E: PgExecutor<'e>>(
    exec: E,
    user_id: UserId,
) -> DbResult<Vec<WatchlistEntry>> {
    #[derive(FromRow)]
    struct Row {
        user_id: Uuid,
        series_id: Uuid,
        status: WatchStatus,
        notify: bool,
        added_at: OffsetDateTime,
    }
    let rows = sqlx::query_as!(
        Row,
        "SELECT user_id, series_id, status AS \"status: WatchStatus\", notify, added_at \
         FROM watchlist_entries WHERE user_id = $1 ORDER BY added_at DESC",
        user_id.as_uuid(),
    )
    .fetch_all(exec)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| WatchlistEntry {
            user_id: UserId::from_uuid(r.user_id),
            series_id: SeriesId::from_uuid(r.series_id),
            status: r.status,
            notify: r.notify,
            added_at: r.added_at,
        })
        .collect())
}

/// Set a watchlist entry's status without disturbing its `notify` flag, inserting the
/// entry (with `notify` defaulted on) if absent. Used by `AniList` pull to import and
/// refresh statuses without clobbering a user's per-title notification choice.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only — no other variant is reachable. As with
/// [`watchlist_upsert`], an existing entry is updated rather than raised as
/// [`crate::DbError::Conflict`] — which is what lets a pull run repeatedly without the
/// import deciding it has already happened.
pub async fn watchlist_set_status<'e, E: PgExecutor<'e>>(
    exec: E,
    user_id: UserId,
    series_id: SeriesId,
    status: WatchStatus,
) -> DbResult<()> {
    sqlx::query!(
        "INSERT INTO watchlist_entries (user_id, series_id, status) \
         VALUES ($1,$2,$3) \
         ON CONFLICT (user_id, series_id) DO UPDATE \
            SET status = EXCLUDED.status, updated_at = now()",
        user_id.as_uuid(),
        series_id.as_uuid(),
        status as WatchStatus,
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// A user's current watch status for a series, if tracked. Used by the targeted single-series
/// sync push (design: immediate targeted push) to read local state without fetching the whole
/// watchlist.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only — no other variant is reachable. An untracked series is
/// `Ok(None)`, which the targeted push reads as "nothing local to send" rather than as a
/// failure.
pub async fn watchlist_status_get<'e, E: PgExecutor<'e>>(
    exec: E,
    user_id: UserId,
    series_id: SeriesId,
) -> DbResult<Option<WatchStatus>> {
    let status = sqlx::query_scalar!(
        "SELECT status AS \"status: WatchStatus\" FROM watchlist_entries WHERE user_id = $1 AND series_id = $2",
        user_id.as_uuid(),
        series_id.as_uuid(),
    )
    .fetch_optional(exec)
    .await?;
    Ok(status)
}

/// Every watchlist status `user_id` holds, keyed by series.
///
/// The batched form of [`watchlist_status_get`], prefetched once per reconciliation run rather
/// than queried per remote entry (PERF-13).
///
/// # Errors
/// [`crate::DbError::Sqlx`] only — no other variant is reachable. Untracked series are
/// **absent** from the map rather than present with a default, so a lookup miss must mean
/// "not tracked" to the caller and never "status unknown".
pub async fn watchlist_statuses_for_user<'e, E: PgExecutor<'e>>(
    exec: E,
    user_id: UserId,
) -> DbResult<HashMap<SeriesId, WatchStatus>> {
    #[derive(FromRow)]
    struct Row {
        series_id: Uuid,
        status: WatchStatus,
    }
    let rows = sqlx::query_as!(
        Row,
        "SELECT series_id, status AS \"status: WatchStatus\" \
         FROM watchlist_entries WHERE user_id = $1",
        user_id.as_uuid(),
    )
    .fetch_all(exec)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| (SeriesId::from_uuid(r.series_id), r.status))
        .collect())
}

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
    /// `discovered_at`, not `published_at`: [`feed`](super::dashboard::feed) and
    /// [`continue_reading`](super::dashboard::continue_reading) both order on `discovered_at`,
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
    /// Empty on [`watchlist_page`] until [`attach_sources`] has run — it is a second statement
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
    /// Not a list filter — it exists so [`watchlist_card`] can reuse this statement instead of
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
/// that silently repeats or skips rows rather than failing. [`fetch_page`] therefore selects the
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
    fn key_is_null(&self) -> bool {
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
    fn add(&mut self, status: WatchStatus, n: i64, degraded: i64) {
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
    fn from_token(token: &str) -> Self {
        match token {
            "today" => Self::Today,
            "week" => Self::ThisWeek,
            _ => Self::Earlier,
        }
    }

    /// Newest band first — the order the group headers render in.
    fn rank(self) -> u8 {
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

/// The row `query_as!` fills for one page of the list.
#[derive(FromRow)]
struct CardRow {
    series_id: Uuid,
    series_title: String,
    cover_url: Option<String>,
    status: WatchStatus,
    notify: bool,
    added_at: OffsetDateTime,
    last_read_number: Option<f64>,
    unread: i64,
    read_count: i64,
    total_chapters: i64,
    latest_chapter_number: Option<f64>,
    latest_chapter_at: Option<OffsetDateTime>,
    next_unread_number: Option<f64>,
    next_unread_title: Option<String>,
    next_unread_at: Option<OffsetDateTime>,
    preferred_source_name: Option<String>,
    source_count: i64,
    source_degraded: bool,
    sync_excluded: bool,
    /// The key the statement ordered by, carried out so [`WatchlistCursor`] cannot recompute
    /// it differently.
    sort_num: Option<f64>,
    sort_text: Option<String>,
}

impl CardRow {
    /// The cursor that resumes immediately after this row.
    fn cursor(&self) -> WatchlistCursor {
        WatchlistCursor {
            num: self.sort_num,
            text: self.sort_text.clone(),
            series_id: SeriesId::from_uuid(self.series_id),
        }
    }
}

impl From<CardRow> for WatchlistCard {
    fn from(r: CardRow) -> Self {
        Self {
            series_id: SeriesId::from_uuid(r.series_id),
            series_title: r.series_title,
            cover_url: r.cover_url,
            status: r.status,
            notify: r.notify,
            added_at: r.added_at,
            last_read_number: r.last_read_number,
            unread: r.unread,
            read_count: r.read_count,
            // The number and the timestamp come from the same row, so either both are present
            // or the reader is caught up; a number without an instant cannot occur.
            next_unread: r
                .next_unread_number
                .zip(r.next_unread_at)
                .map(|(number, released_at)| NextUnread {
                    number,
                    title: r.next_unread_title,
                    released_at,
                }),
            total_chapters: r.total_chapters,
            latest_chapter_number: r.latest_chapter_number,
            latest_chapter_at: r.latest_chapter_at,
            preferred_source_name: r.preferred_source_name,
            source_count: r.source_count,
            source_degraded: r.source_degraded,
            sync_excluded: r.sync_excluded,
            sources: Vec::new(),
        }
    }
}

/// List a user's watchlist, filtered, sorted and paginated in SQL, together with the tab
/// counts and the group-header aggregates. `unread` counts distinct whole chapters
/// (`floor(number)`) so part releases don't inflate it.
///
/// The unread filter is the fourth copy of the predicate documented on
/// [`dashboard`](super::dashboard); it must stay the negation of
/// [`ReadProgress::covers`](super::ReadProgress::covers), or this badge disagrees with the feed
/// that links to the same chapters. `repo_tracking`'s `unread_predicate_agrees_everywhere` test
/// is what holds the four together.
///
/// # Why three statements
///
/// The page, the per-status counts and the group aggregates answer three different questions
/// over three different predicate sets — the counts deliberately drop the `status` filter, the
/// groups keep it — so no single statement produces all three without a `GROUPING SETS`
/// construction that would be harder to read than the three it replaced. They are issued
/// concurrently on separate pool connections, so the trio costs one round trip, exactly as
/// [`list_series_filtered`](crate::repo::catalog::list_series_filtered) does.
///
/// `total` is **derived** from the group aggregates rather than counted a fourth time: the
/// grouping query carries the identical predicate list, every row lands in exactly one band,
/// and a separate `count(*)` could only ever disagree with the sum by racing it.
///
/// Takes `&PgPool` rather than a generic executor precisely so the three can overlap.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only — no other variant is reachable. A user tracking nothing, and
/// a filter matching nothing, are both an empty page with zeroed counts rather than
/// [`crate::DbError::NotFound`]. A tracked series with no progress row comes back through the
/// `LEFT JOIN` with `last_read_number: None` and its full chapter count as `unread`, which is a
/// valid row rather than a missing one; a tracked series with no chapters at all comes back
/// with `total_chapters: 0` and `latest_chapter_at: None`, and bands as
/// [`ReleaseBucket::Earlier`].
pub async fn watchlist_page(
    pool: &PgPool,
    user_id: UserId,
    filter: &WatchlistFilter,
) -> DbResult<WatchlistPage> {
    let query = filter
        .query
        .as_deref()
        .map(str::trim)
        .filter(|q| !q.is_empty());

    let (rows, status_counts, groups) = tokio::try_join!(
        fetch_page(pool, user_id, filter, query),
        fetch_counts(pool, user_id, filter, query),
        fetch_groups(pool, user_id, filter, query),
    )?;

    let mut counts = WatchlistCounts::default();
    for (status, n, degraded) in status_counts {
        counts.add(status, n, degraded);
    }
    let total = groups.iter().map(|g| g.title_count).sum();

    let next_cursor = (i64::try_from(rows.len()).unwrap_or(i64::MAX) >= filter.limit)
        .then(|| rows.last().map(CardRow::cursor))
        .flatten();

    Ok(WatchlistPage {
        items: attach_sources(pool, rows).await?,
        total,
        counts,
        groups,
        next_cursor,
    })
}

/// Turn page rows into cards with their [`WatchlistCard::sources`] filled in.
///
/// The one place a `WatchlistCard` is built for a caller, so no path can hand out a card whose
/// empty `sources` means "not loaded" rather than "no sources".
async fn attach_sources(pool: &PgPool, rows: Vec<CardRow>) -> DbResult<Vec<WatchlistCard>> {
    let ids: Vec<Uuid> = rows.iter().map(|r| r.series_id).collect();
    let mut by_series = fetch_sources(pool, &ids).await?;
    Ok(rows
        .into_iter()
        .map(|row| {
            let mut card = WatchlistCard::from(row);
            card.sources = by_series.remove(&card.series_id).unwrap_or_default();
            card
        })
        .collect())
}

/// One page of matching rows, in the requested order.
///
/// # Why the sort key is computed in a subquery
///
/// The order is chosen by two bound tokens, so `ORDER BY` needs an ascending and a descending
/// arm for each. Written flat, that means repeating the five-branch key expression twice, and
/// the two copies drifting is a list silently ordered by the wrong thing under one direction.
/// Naming the key once in the subquery and ordering the outer query by it costs a nesting
/// level Postgres flattens anyway.
///
/// The final `series_id` tiebreaker is not decoration: without it rows sharing a leading key —
/// and with `unread` over 600 entries there are hundreds of ties — have no defined order, so
/// two adjacent `OFFSET` pages can repeat one row and skip another.
#[expect(
    clippy::too_many_lines,
    reason = "one `query_as!` invocation: the length is the SQL literal and its bindings, and \
              splitting a statement across helpers is exactly the drift the `--wl-cols` comment \
              and the sort-key subquery both exist to prevent"
)]
async fn fetch_page(
    pool: &PgPool,
    user_id: UserId,
    filter: &WatchlistFilter,
    query: Option<&str>,
) -> DbResult<Vec<CardRow>> {
    let cursor = filter.cursor.as_ref();
    let rows = sqlx::query_as!(
        CardRow,
        "SELECT q.series_id AS \"series_id!\", q.series_title AS \"series_title!\", q.cover_url, \
                q.status AS \"status!: WatchStatus\", q.notify AS \"notify!\", \
                q.added_at AS \"added_at!\", q.sync_excluded AS \"sync_excluded!\", \
                q.last_read_number, q.unread AS \"unread!\", q.read_count AS \"read_count!\", \
                q.total_chapters AS \"total_chapters!\", q.latest_chapter_number, \
                q.latest_chapter_at, \
                q.next_unread_number AS \"next_unread_number?\", \
                q.next_unread_title AS \"next_unread_title?\", \
                q.next_unread_at AS \"next_unread_at?\", q.preferred_source_name, \
                q.source_count AS \"source_count!\", q.source_degraded AS \"source_degraded!\", \
                q.sort_num, q.sort_text \
         FROM ( \
           SELECT w.series_id, s.canonical_title AS series_title, s.cover_url, w.status, \
                  w.notify, w.added_at, w.sync_excluded, \
                  rp.last_read_whole_number::float8 AS last_read_number, \
                  ch.unread, ch.read_count, ch.total_chapters, ch.latest_chapter_number, \
                  ch.latest_chapter_at, \
                  nu.number AS next_unread_number, nu.title AS next_unread_title, \
                  nu.discovered_at AS next_unread_at, \
                  src.preferred_source_name, src.source_count, src.source_degraded, \
                  CASE $7 \
                    WHEN 'released' THEN extract(epoch FROM ch.latest_chapter_at)::float8 \
                    WHEN 'unread'   THEN ch.unread::float8 \
                    WHEN 'added'    THEN extract(epoch FROM w.added_at)::float8 \
                    WHEN 'progress' THEN CASE WHEN ch.total_chapters > 0 \
                                              THEN COALESCE(rp.last_read_whole_number, 0)::float8 \
                                                   / ch.total_chapters END \
                  END AS sort_num, \
                  CASE WHEN $7 = 'title' THEN s.canonical_title END AS sort_text \
           FROM watchlist_entries w \
           JOIN series s ON s.id = w.series_id \
           LEFT JOIN read_progress rp ON rp.user_id = w.user_id AND rp.series_id = w.series_id \
           CROSS JOIN LATERAL ( \
             SELECT COALESCE(count(DISTINCT floor(c.number)), 0) AS total_chapters, \
                    COALESCE(count(DISTINCT floor(c.number)) FILTER ( \
                      WHERE floor(c.number) > COALESCE(rp.last_read_whole_number, 0) \
                        AND NOT (c.number <> floor(c.number) \
                                 AND rp.last_read_part_number IS NOT NULL \
                                 AND c.number <= rp.last_read_part_number) \
                    ), 0) AS unread, \
                    COALESCE(count(DISTINCT floor(c.number)) FILTER ( \
                      WHERE floor(c.number) <= COALESCE(rp.last_read_whole_number, 0) \
                    ), 0) AS read_count, \
                    max(c.number)::float8 AS latest_chapter_number, \
                    max(c.discovered_at) AS latest_chapter_at \
             FROM series_sources ss JOIN chapters c ON c.series_source_id = ss.id \
             WHERE ss.series_id = w.series_id \
           ) ch \
           LEFT JOIN LATERAL ( \
             SELECT c.number::float8 AS number, c.title, c.discovered_at \
             FROM series_sources ss JOIN chapters c ON c.series_source_id = ss.id \
             WHERE ss.series_id = w.series_id \
               AND floor(c.number) > COALESCE(rp.last_read_whole_number, 0) \
               AND NOT (c.number <> floor(c.number) \
                        AND rp.last_read_part_number IS NOT NULL \
                        AND c.number <= rp.last_read_part_number) \
             ORDER BY c.number, c.discovered_at, c.id \
             LIMIT 1 \
           ) nu ON true \
           CROSS JOIN LATERAL ( \
             SELECT count(DISTINCT ss.provider_id) AS source_count, \
                    (array_agg(p.name ORDER BY ss.chapter_count DESC, \
                                                ss.last_scanned_at DESC NULLS LAST, \
                                                p.slug))[1] AS preferred_source_name, \
                    COALESCE((array_agg(ss.state <> 'active' OR p.state <> 'active' \
                                        ORDER BY ss.chapter_count DESC, \
                                                 ss.last_scanned_at DESC NULLS LAST, \
                                                 p.slug))[1], false) AS source_degraded \
             FROM series_sources ss JOIN providers p ON p.id = ss.provider_id \
             WHERE ss.series_id = w.series_id \
           ) src \
           WHERE w.user_id = $1 \
             AND ($2::watch_status IS NULL OR w.status = $2) \
             AND ($3::text IS NULL \
                  OR strpos(lower(s.canonical_title), lower($3)) > 0 \
                  OR EXISTS (SELECT 1 FROM series_titles st \
                             WHERE st.series_id = w.series_id \
                               AND strpos(lower(st.title), lower($3)) > 0) \
                  OR EXISTS (SELECT 1 FROM series_tags stg JOIN tags t ON t.id = stg.tag_id \
                             WHERE stg.series_id = w.series_id \
                               AND strpos(lower(t.name), lower($3)) > 0) \
                  OR EXISTS (SELECT 1 FROM series_authors sa JOIN authors a ON a.id = sa.author_id \
                             WHERE sa.series_id = w.series_id \
                               AND strpos(lower(a.name), lower($3)) > 0)) \
             AND (NOT $4::boolean OR ch.unread > 0) \
             AND ($5::timestamptz IS NULL OR ch.latest_chapter_at >= $5) \
             AND (NOT $6::boolean OR src.source_degraded) \
             AND ($11::uuid IS NULL OR w.series_id = $11) \
         ) q \
         WHERE NOT $12::boolean \
            OR CASE WHEN $7 = 'title' THEN \
                 (q.sort_text IS NULL AND NOT $15::boolean) \
                 OR ($15 AND q.sort_text IS NULL AND q.series_id > $16::uuid) \
                 OR (NOT $15 AND q.sort_text IS NOT NULL AND ( \
                        ($8 = 'desc' AND q.sort_text < $14::text) \
                     OR ($8 = 'asc'  AND q.sort_text > $14::text) \
                     OR (q.sort_text = $14 AND q.series_id > $16::uuid))) \
               ELSE \
                 (q.sort_num IS NULL AND NOT $15::boolean) \
                 OR ($15 AND q.sort_num IS NULL AND q.series_id > $16::uuid) \
                 OR (NOT $15 AND q.sort_num IS NOT NULL AND ( \
                        ($8 = 'desc' AND q.sort_num < $13::float8) \
                     OR ($8 = 'asc'  AND q.sort_num > $13::float8) \
                     OR (q.sort_num = $13 AND q.series_id > $16::uuid))) \
               END \
         ORDER BY CASE WHEN $8 = 'asc'  THEN q.sort_num  END ASC  NULLS LAST, \
                  CASE WHEN $8 = 'desc' THEN q.sort_num  END DESC NULLS LAST, \
                  CASE WHEN $8 = 'asc'  THEN q.sort_text END ASC  NULLS LAST, \
                  CASE WHEN $8 = 'desc' THEN q.sort_text END DESC NULLS LAST, \
                  q.series_id \
         LIMIT $9 OFFSET $10",
        user_id.as_uuid(),
        filter.status as Option<WatchStatus>,
        query,
        filter.unread_only,
        filter.released_since,
        filter.source_issues,
        filter.sort.as_token(),
        filter.order.as_token(),
        filter.limit,
        // A cursor replaces the offset rather than adding to it: seeking past the row the
        // caller named and *then* skipping N more would drop rows nobody asked to skip.
        if cursor.is_some() { 0 } else { filter.offset },
        filter.series_id.map(SeriesId::as_uuid),
        cursor.is_some(),
        cursor.and_then(|c| c.num),
        cursor.and_then(|c| c.text.as_deref()),
        cursor.is_some_and(WatchlistCursor::key_is_null),
        cursor.map_or_else(uuid::Uuid::nil, |c| c.series_id.as_uuid()),
    )
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// Every provider carrying each of `series_ids`, preferred first.
///
/// A second statement keyed on the page's ids rather than an aggregate folded into
/// [`fetch_page`]: the ledger only renders this column above 1500px, and four more `array_agg`
/// columns per row are paid on every page whether or not anything reads them. Empty in, empty
/// out — no statement is issued for an empty page.
///
/// The `preferred` flag repeats [`fetch_page`]'s ranking (`chapter_count`, then the most recent
/// scan, then the slug) because the two must name the same source; a `Sources` column whose
/// tinted tile disagrees with the row's own submeta is worse than an untinted one.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only — no other variant is reachable. A series with no sources is
/// simply absent from the map.
async fn fetch_sources(
    pool: &PgPool,
    series_ids: &[Uuid],
) -> DbResult<HashMap<SeriesId, Vec<WatchlistSource>>> {
    #[derive(FromRow)]
    struct Row {
        series_id: Uuid,
        code: String,
        name: String,
        state: ProviderState,
        preferred: bool,
    }

    if series_ids.is_empty() {
        return Ok(HashMap::new());
    }
    let rows = sqlx::query_as!(
        Row,
        "SELECT ss.series_id AS \"series_id!\", p.slug AS \"code!\", p.name AS \"name!\", \
                CASE WHEN p.state <> 'active' THEN p.state ELSE ss.state END \
                  AS \"state!: ProviderState\", \
                (row_number() OVER (PARTITION BY ss.series_id \
                                    ORDER BY ss.chapter_count DESC, \
                                             ss.last_scanned_at DESC NULLS LAST, \
                                             p.slug) = 1) AS \"preferred!\" \
         FROM series_sources ss JOIN providers p ON p.id = ss.provider_id \
         WHERE ss.series_id = ANY($1) \
         ORDER BY ss.series_id, ss.chapter_count DESC, \
                  ss.last_scanned_at DESC NULLS LAST, p.slug",
        series_ids,
    )
    .fetch_all(pool)
    .await?;

    let mut out: HashMap<SeriesId, Vec<WatchlistSource>> = HashMap::new();
    for row in rows {
        out.entry(SeriesId::from_uuid(row.series_id))
            .or_default()
            .push(WatchlistSource {
                code: row.code,
                name: row.name,
                state: row.state,
                preferred: row.preferred,
            });
    }
    Ok(out)
}

/// One series' watchlist row, enriched exactly as the list's rows are — or `None` when the
/// user does not track it.
///
/// The Series page needs the same card the list renders (status, notify, progress, sync
/// exclusion). Fetching the whole watchlist to find one row breaks once the list paginates:
/// past the first page the entry simply is not in the response, so the page would render
/// "not tracked" for a series the user tracks.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only — no other variant is reachable. An untracked series is
/// `Ok(None)` rather than [`crate::DbError::NotFound`]: "do you track this" is a question with
/// a negative answer, not a missing resource.
pub async fn watchlist_card(
    pool: &PgPool,
    user_id: UserId,
    series_id: SeriesId,
) -> DbResult<Option<WatchlistCard>> {
    let filter = WatchlistFilter {
        series_id: Some(series_id),
        limit: 1,
        ..WatchlistFilter::default()
    };
    let rows = fetch_page(pool, user_id, &filter, None).await?;
    Ok(attach_sources(pool, rows).await?.into_iter().next())
}

/// The whole watchlist at a glance: per-status counts and the unread total, under **no**
/// filters at all.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WatchlistSummary {
    pub counts: WatchlistCounts,
    /// Unread chapters across every tracked series, whatever its status.
    pub unread_total: i64,
}

/// The unfiltered shape of a user's watchlist.
///
/// Distinct from [`WatchlistPage::counts`], which drops only the `status` arm and keeps the
/// search, recency and source filters: those answer "how many would this tab show *given what
/// I have typed*", while this answers "how big is my library" for surfaces with no filter state
/// of their own — a tab badge, a More sheet, a signed-in header.
///
/// One statement rather than [`fetch_counts`] with an empty filter: with no free-text arm there
/// is no reason to join `series` at all, and the unread sum rides along in the same scan.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only — no other variant is reachable. A user tracking nothing is a
/// zeroed summary, not [`crate::DbError::NotFound`].
pub async fn watchlist_summary<'e, E: PgExecutor<'e>>(
    exec: E,
    user_id: UserId,
) -> DbResult<WatchlistSummary> {
    #[derive(FromRow)]
    struct Row {
        status: WatchStatus,
        n: i64,
        degraded: i64,
        unread: i64,
    }
    let rows = sqlx::query_as!(
        Row,
        "SELECT w.status AS \"status!: WatchStatus\", count(*) AS \"n!\", \
                count(*) FILTER (WHERE src.source_degraded) AS \"degraded!\", \
                COALESCE(sum(ch.unread), 0)::int8 AS \"unread!\" \
         FROM watchlist_entries w \
         LEFT JOIN read_progress rp ON rp.user_id = w.user_id AND rp.series_id = w.series_id \
         CROSS JOIN LATERAL ( \
           SELECT COALESCE(count(DISTINCT floor(c.number)) FILTER ( \
                    WHERE floor(c.number) > COALESCE(rp.last_read_whole_number, 0) \
                      AND NOT (c.number <> floor(c.number) \
                               AND rp.last_read_part_number IS NOT NULL \
                               AND c.number <= rp.last_read_part_number) \
                  ), 0) AS unread \
           FROM series_sources ss JOIN chapters c ON c.series_source_id = ss.id \
           WHERE ss.series_id = w.series_id \
         ) ch \
         CROSS JOIN LATERAL ( \
           SELECT COALESCE((array_agg(ss.state <> 'active' OR p.state <> 'active' \
                                      ORDER BY ss.chapter_count DESC, \
                                               ss.last_scanned_at DESC NULLS LAST, \
                                               p.slug))[1], false) AS source_degraded \
           FROM series_sources ss JOIN providers p ON p.id = ss.provider_id \
           WHERE ss.series_id = w.series_id \
         ) src \
         WHERE w.user_id = $1 \
         GROUP BY w.status",
        user_id.as_uuid(),
    )
    .fetch_all(exec)
    .await?;

    let mut summary = WatchlistSummary::default();
    for row in rows {
        summary.counts.add(row.status, row.n, row.degraded);
        summary.unread_total += row.unread;
    }
    Ok(summary)
}

/// How many entries sit at each status under every filter *but* `status`.
///
/// The predicate list is [`fetch_page`]'s minus the status arm and must stay that way — a tab
/// whose count disagrees with the list it opens is worse than one with no count at all.
async fn fetch_counts(
    pool: &PgPool,
    user_id: UserId,
    filter: &WatchlistFilter,
    query: Option<&str>,
) -> DbResult<Vec<(WatchStatus, i64, i64)>> {
    #[derive(FromRow)]
    struct Row {
        status: WatchStatus,
        n: i64,
        degraded: i64,
    }
    let rows = sqlx::query_as!(
        Row,
        "SELECT w.status AS \"status!: WatchStatus\", count(*) AS \"n!\", \
                count(*) FILTER (WHERE src.source_degraded) AS \"degraded!\" \
         FROM watchlist_entries w \
         JOIN series s ON s.id = w.series_id \
         LEFT JOIN read_progress rp ON rp.user_id = w.user_id AND rp.series_id = w.series_id \
         CROSS JOIN LATERAL ( \
           SELECT COALESCE(count(DISTINCT floor(c.number)) FILTER ( \
                    WHERE floor(c.number) > COALESCE(rp.last_read_whole_number, 0) \
                      AND NOT (c.number <> floor(c.number) \
                               AND rp.last_read_part_number IS NOT NULL \
                               AND c.number <= rp.last_read_part_number) \
                  ), 0) AS unread, \
                  max(c.discovered_at) AS latest_chapter_at \
           FROM series_sources ss JOIN chapters c ON c.series_source_id = ss.id \
           WHERE ss.series_id = w.series_id \
         ) ch \
         CROSS JOIN LATERAL ( \
           SELECT COALESCE((array_agg(ss.state <> 'active' OR p.state <> 'active' \
                                      ORDER BY ss.chapter_count DESC, \
                                               ss.last_scanned_at DESC NULLS LAST, \
                                               p.slug))[1], false) AS source_degraded \
           FROM series_sources ss JOIN providers p ON p.id = ss.provider_id \
           WHERE ss.series_id = w.series_id \
         ) src \
         WHERE w.user_id = $1 \
           AND ($2::text IS NULL \
                OR strpos(lower(s.canonical_title), lower($2)) > 0 \
                OR EXISTS (SELECT 1 FROM series_titles st \
                           WHERE st.series_id = w.series_id \
                             AND strpos(lower(st.title), lower($2)) > 0) \
                OR EXISTS (SELECT 1 FROM series_tags stg JOIN tags t ON t.id = stg.tag_id \
                           WHERE stg.series_id = w.series_id \
                             AND strpos(lower(t.name), lower($2)) > 0) \
                OR EXISTS (SELECT 1 FROM series_authors sa JOIN authors a ON a.id = sa.author_id \
                           WHERE sa.series_id = w.series_id \
                             AND strpos(lower(a.name), lower($2)) > 0)) \
           AND (NOT $3::boolean OR ch.unread > 0) \
           AND ($4::timestamptz IS NULL OR ch.latest_chapter_at >= $4) \
           AND (NOT $5::boolean OR src.source_degraded) \
           AND ($6::uuid IS NULL OR w.series_id = $6) \
         GROUP BY w.status",
        user_id.as_uuid(),
        query,
        filter.unread_only,
        filter.released_since,
        filter.source_issues,
        filter.series_id.map(SeriesId::as_uuid),
    )
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| (r.status, r.n, r.degraded))
        .collect())
}

/// The group-header aggregates, over the *whole* filter — `status` included, since the groups
/// band the list the user is actually looking at.
///
/// The bands are rolling windows off the database clock; see [`ReleaseBucket`] for why they are
/// not calendar days. A row with no chapters has no `latest_chapter_at`, both comparisons are
/// `NULL`, and it falls to the `ELSE` arm — which is what keeps `total` (the sum of the bands)
/// equal to the number of rows the page query can return.
async fn fetch_groups(
    pool: &PgPool,
    user_id: UserId,
    filter: &WatchlistFilter,
    query: Option<&str>,
) -> DbResult<Vec<ReleaseGroup>> {
    #[derive(FromRow)]
    struct Row {
        bucket: String,
        title_count: i64,
        chapter_count: i64,
    }
    let rows = sqlx::query_as!(
        Row,
        "SELECT CASE \
                  WHEN ch.latest_chapter_at >= now() - interval '24 hours' THEN 'today' \
                  WHEN ch.latest_chapter_at >= now() - interval '7 days'   THEN 'week' \
                  ELSE 'earlier' \
                END AS \"bucket!\", \
                count(*) AS \"title_count!\", \
                COALESCE(sum(ch.unread), 0)::int8 AS \"chapter_count!\" \
         FROM watchlist_entries w \
         JOIN series s ON s.id = w.series_id \
         LEFT JOIN read_progress rp ON rp.user_id = w.user_id AND rp.series_id = w.series_id \
         CROSS JOIN LATERAL ( \
           SELECT COALESCE(count(DISTINCT floor(c.number)) FILTER ( \
                    WHERE floor(c.number) > COALESCE(rp.last_read_whole_number, 0) \
                      AND NOT (c.number <> floor(c.number) \
                               AND rp.last_read_part_number IS NOT NULL \
                               AND c.number <= rp.last_read_part_number) \
                  ), 0) AS unread, \
                  max(c.discovered_at) AS latest_chapter_at \
           FROM series_sources ss JOIN chapters c ON c.series_source_id = ss.id \
           WHERE ss.series_id = w.series_id \
         ) ch \
         CROSS JOIN LATERAL ( \
           SELECT COALESCE((array_agg(ss.state <> 'active' OR p.state <> 'active' \
                                      ORDER BY ss.chapter_count DESC, \
                                               ss.last_scanned_at DESC NULLS LAST, \
                                               p.slug))[1], false) AS source_degraded \
           FROM series_sources ss JOIN providers p ON p.id = ss.provider_id \
           WHERE ss.series_id = w.series_id \
         ) src \
         WHERE w.user_id = $1 \
           AND ($2::watch_status IS NULL OR w.status = $2) \
           AND ($3::text IS NULL \
                OR strpos(lower(s.canonical_title), lower($3)) > 0 \
                OR EXISTS (SELECT 1 FROM series_titles st \
                           WHERE st.series_id = w.series_id \
                             AND strpos(lower(st.title), lower($3)) > 0) \
                OR EXISTS (SELECT 1 FROM series_tags stg JOIN tags t ON t.id = stg.tag_id \
                           WHERE stg.series_id = w.series_id \
                             AND strpos(lower(t.name), lower($3)) > 0) \
                OR EXISTS (SELECT 1 FROM series_authors sa JOIN authors a ON a.id = sa.author_id \
                           WHERE sa.series_id = w.series_id \
                             AND strpos(lower(a.name), lower($3)) > 0)) \
           AND (NOT $4::boolean OR ch.unread > 0) \
           AND ($5::timestamptz IS NULL OR ch.latest_chapter_at >= $5) \
           AND (NOT $6::boolean OR src.source_degraded) \
           AND ($7::uuid IS NULL OR w.series_id = $7) \
         GROUP BY 1",
        user_id.as_uuid(),
        filter.status as Option<WatchStatus>,
        query,
        filter.unread_only,
        filter.released_since,
        filter.source_issues,
        filter.series_id.map(SeriesId::as_uuid),
    )
    .fetch_all(pool)
    .await?;

    let mut groups: Vec<ReleaseGroup> = rows
        .into_iter()
        .map(|r| ReleaseGroup {
            bucket: ReleaseBucket::from_token(&r.bucket),
            title_count: r.title_count,
            chapter_count: r.chapter_count,
        })
        .collect();
    groups.sort_unstable_by_key(|g| g.bucket.rank());
    Ok(groups)
}
