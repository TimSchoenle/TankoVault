//! The authenticated user's watchlist: listing, upsert, removal, and the bulk operations the
//! list view's multi-select drives.

use super::progress::{spawn_targeted_push, spawn_targeted_push_many};
use crate::error::{ApiError, ApiResult};
use crate::openapi::ME_WATCHLIST_TAG;
use crate::state::{AppState, AuthUser};
use axum::Json;
use axum::extract::{Path, Query, State};
use serde::{Deserialize, Serialize};
use tankovault_db::repo::tracking::{
    BULK_ID_LIMIT, WatchlistCursor, WatchlistFilter, WatchlistOrder, WatchlistSort,
};
use tankovault_domain::{SeriesId, SeriesSourceId, WatchStatus};
use time::{Duration, OffsetDateTime};
use utoipa::{IntoParams, ToSchema};

/// Largest page size; bounds a hand-written `limit=100000` from aggregating every chapter of
/// every tracked series in one request.
const MAX_LIMIT: i64 = 200;

/// Highest accepted offset, short of the `i64` overflow an unbounded value allowed on
/// `/v1/series` before it was clamped there for the same reason.
const MAX_OFFSET: i64 = 100_000;

#[derive(Debug, Serialize, ToSchema)]
pub struct WatchlistItem {
    pub series_id: SeriesId,
    pub status: WatchStatus,
    pub notify: bool,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String)]
    pub added_at: time::OffsetDateTime,
    /// Embedded series title so the Watchlist renders without a per-row detail fetch
    /// (frontend §9.3, kills the N+1).
    pub series_title: String,
    pub cover_url: Option<String>,
    /// The user's last-read chapter number, if any.
    pub last_read_number: Option<f64>,
    /// Unread chapters above the user's progress.
    pub unread: i64,
    /// Whole chapters at or below the user's progress — the progress bar's numerator.
    ///
    /// Not [`Self::last_read_number`]: a catalogue with gaps makes the frontier larger than the
    /// number of chapters below it, so a bar drawn from the frontier reads past 100%.
    pub read_count: i64,
    /// The lowest unread chapter, or `null` when the reader is caught up.
    pub next_unread: Option<NextUnread>,
    /// Every provider carrying this series, preferred first.
    pub sources: Vec<WatchlistSource>,
    /// Distinct whole chapters known across all sources — the progress bar's denominator.
    pub total_chapters: i64,
    /// The highest chapter number known across all sources.
    pub latest_chapter_number: Option<f64>,
    /// When the newest chapter was discovered. Drives the default sort, the `Released` column
    /// and the Today/This week/Earlier grouping.
    #[serde(with = "time::serde::rfc3339::option")]
    #[schema(value_type = Option<String>)]
    pub latest_chapter_at: Option<time::OffsetDateTime>,
    /// Display name of the provider this series is primarily carried by.
    pub preferred_source_name: Option<String>,
    /// Distinct providers carrying this series.
    pub source_count: i64,
    /// Whether the preferred source, or the provider behind it, is unhealthy.
    pub source_degraded: bool,
    /// Whether this series is opted out of external sync (design v2 §A.5).
    pub sync_excluded: bool,
    /// The source this reader pinned for this series, or `null` to follow the global order.
    ///
    /// The reader's explicit choice, and not the same question as
    /// [`Self::preferred_source_name`], which stays the derived richest source the ledger row
    /// displays. Set through `PUT /v1/me/watchlist/{series_id}/source-pin`.
    // A bare `Uuid`, not `Option<SeriesSourceId>`: an optional newtype publishes as
    // `oneOf [null, $ref]`, and this field stays out of that union rather than relying on
    // `xtask`'s `collapse_nullable_union` to undo it. Inline `{type: [string, null], format:
    // uuid}` costs the client one `SeriesSourceId::from(uuid)` instead.
    pub pinned_source_id: Option<uuid::Uuid>,
}

/// The chapter a `Continue` button opens, and what the ledger's `Next unread` column shows.
#[derive(Debug, Serialize, ToSchema)]
pub struct NextUnread {
    /// The chapter number, parts included.
    pub number: f64,
    /// The provider's chapter title, when it publishes one.
    pub title: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String)]
    pub released_at: time::OffsetDateTime,
}

/// One provider carrying a series, for the ledger's `Sources` column.
#[derive(Debug, Serialize, ToSchema)]
pub struct WatchlistSource {
    /// The provider slug — the monogram tile's letters and a stable client-side key.
    pub code: String,
    pub name: String,
    /// The effective state: the series-source's own, unless the provider behind it is worse.
    pub state: tankovault_domain::ProviderState,
    /// Whether this is the source `preferred_source_name` names.
    pub preferred: bool,
}

impl From<tankovault_db::repo::tracking::WatchlistCard> for WatchlistItem {
    fn from(c: tankovault_db::repo::tracking::WatchlistCard) -> Self {
        Self {
            series_id: c.series_id,
            status: c.status,
            notify: c.notify,
            added_at: c.added_at,
            series_title: c.series_title,
            cover_url: c.cover_url,
            last_read_number: c.last_read_number,
            unread: c.unread,
            read_count: c.read_count,
            next_unread: c.next_unread.map(|n| NextUnread {
                number: n.number,
                title: n.title,
                released_at: n.released_at,
            }),
            sources: c
                .sources
                .into_iter()
                .map(|s| WatchlistSource {
                    code: s.code,
                    name: s.name,
                    state: s.state,
                    preferred: s.preferred,
                })
                .collect(),
            total_chapters: c.total_chapters,
            latest_chapter_number: c.latest_chapter_number,
            latest_chapter_at: c.latest_chapter_at,
            preferred_source_name: c.preferred_source_name,
            source_count: c.source_count,
            source_degraded: c.source_degraded,
            sync_excluded: c.sync_excluded,
            pinned_source_id: c.pinned_source_id.map(SeriesSourceId::as_uuid),
        }
    }
}

/// Entry counts per status, for the tab strip.
#[derive(Debug, Serialize, ToSchema)]
pub struct WatchlistCounts {
    pub reading: i64,
    pub planned: i64,
    pub paused: i64,
    pub completed: i64,
    pub dropped: i64,
    pub all: i64,
    /// Rows whose preferred source is unhealthy, for the toolbar's `Source issues` chip.
    pub source_issues: i64,
}

/// One release-recency band's aggregates, for a group header.
#[derive(Debug, Serialize, ToSchema)]
pub struct WatchlistGroup {
    /// `today` | `week` | `earlier`.
    pub key: String,
    pub title_count: i64,
    pub chapter_count: i64,
}

/// The watchlist page plus the chrome around it.
#[derive(Debug, Serialize, ToSchema)]
pub struct WatchlistView {
    pub items: Vec<WatchlistItem>,
    /// Rows matching the whole filter, `status` included — the pager's denominator.
    pub total: i64,
    pub counts: WatchlistCounts,
    /// Newest band first; empty bands are omitted.
    pub groups: Vec<WatchlistGroup>,
    /// Pass back as `cursor` to fetch the next page; `null` at the end of the list.
    ///
    /// Opaque on purpose: it encodes the sort key the server ordered by, which changes shape
    /// with `sort`, and a client that parsed it would break the first time an order was added.
    pub next_cursor: Option<String>,
}

/// The whole watchlist at a glance, under no filters at all.
#[derive(Debug, Serialize, ToSchema)]
pub struct WatchlistSummaryView {
    pub counts: WatchlistCounts,
    /// Unread chapters across every tracked series, whatever its status.
    pub unread_total: i64,
}

/// The opaque `cursor` token's payload.
///
/// Serialised rather than spelled into the query string because the text key is a series title:
/// arbitrary user-visible text with `&`, `=` and `#` in it, which a hand-built token would have
/// to escape correctly on both sides.
#[derive(Debug, Serialize, Deserialize)]
struct CursorPayload {
    /// The numeric sort key, for every order but `title`.
    n: Option<f64>,
    /// The text sort key, for `title`.
    t: Option<String>,
    i: uuid::Uuid,
}

/// Encode a repo cursor as the opaque token the client echoes back.
fn encode_cursor(cursor: &WatchlistCursor) -> String {
    use base64::Engine as _;
    let payload = CursorPayload {
        n: cursor.num,
        t: cursor.text.clone(),
        i: cursor.series_id.as_uuid(),
    };
    // `to_vec` cannot fail for this shape — every field is a plain scalar or `null`.
    let json = serde_json::to_vec(&payload).unwrap_or_default();
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(json)
}

/// Decode a client-supplied `cursor`, refusing a malformed one.
///
/// A `400` rather than a silent fall back to page one: a client that has mangled its cursor and
/// is served the top of the list instead reads as "the list restarted", and an infinite scroll
/// that restarts is a loop.
fn decode_cursor(raw: Option<&str>) -> ApiResult<Option<WatchlistCursor>> {
    use base64::Engine as _;
    let Some(token) = raw.map(str::trim).filter(|v| !v.is_empty()) else {
        return Ok(None);
    };
    let bad = || ApiError::BadRequest("malformed cursor".into());
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(token)
        .map_err(|_| bad())?;
    let payload: CursorPayload = serde_json::from_slice(&bytes).map_err(|_| bad())?;
    Ok(Some(WatchlistCursor {
        num: payload.n,
        text: payload.t,
        series_id: SeriesId::from_uuid(payload.i),
    }))
}

/// Query parameters for the watchlist list. All are optional; absent means "no constraint".
#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct WatchlistParams {
    /// `reading | planned | paused | completed | dropped`; absent means every status.
    #[serde(default)]
    pub status: Option<String>,
    /// Free-text filter over title, alternative titles, tags and authors.
    #[serde(default)]
    pub q: Option<String>,
    #[serde(default)]
    pub unread_only: bool,
    /// `24h | 7d | 30d`; absent (or `any`) means no recency constraint.
    ///
    /// A *window* token rather than an absolute instant, despite the name. The client would
    /// have to compute the instant from its own clock, and a browser minutes ahead of the
    /// server would filter out the very releases the "last 24 hours" option exists to show.
    /// The server resolves the window against the clock the timestamps were written by.
    #[serde(default)]
    pub released_since: Option<String>,
    /// Only rows whose preferred source is unhealthy.
    #[serde(default)]
    pub source_issues: bool,
    /// `released | unread | added | title | progress` (default `released`).
    #[serde(default)]
    pub sort: Option<String>,
    /// `asc | desc`. Defaults to whichever direction reads as "most interesting first" for the
    /// chosen sort — descending everywhere except `title`.
    #[serde(default)]
    pub order: Option<String>,
    #[serde(default = "default_limit")]
    pub limit: i64,
    #[serde(default)]
    pub offset: i64,
    /// Opaque token from a previous response's `next_cursor`; supersedes `offset`.
    ///
    /// Keyset, not offset: the watchlist is live — a chapter discovered between two requests
    /// reorders the default sort — and an `OFFSET` page over a shifting list repeats one row
    /// and skips another. `offset` stays accepted for callers that refetch a whole prefix
    /// rather than appending, which is immune to the same problem by construction.
    #[serde(default)]
    pub cursor: Option<String>,
}

fn default_limit() -> i64 {
    60
}

/// Parse an optional query-string token, refusing an unrecognised one.
///
/// An empty value means "not supplied" — the frontend's select controls submit `""` for their
/// "any" option, and treating that as a parse failure would 400 the default page.
fn parse_param<T: std::str::FromStr>(raw: Option<&str>, name: &str) -> ApiResult<Option<T>> {
    match raw.map(str::trim).filter(|v| !v.is_empty()) {
        None => Ok(None),
        Some(value) => value
            .parse()
            .map(Some)
            .map_err(|_| ApiError::BadRequest(format!("unknown {name}: {value:?}"))),
    }
}

/// Resolve the `released_since` window token to an instant, refusing an unrecognised one.
///
/// Refused rather than ignored: a client asking for "last 24 hours" and silently receiving the
/// unfiltered list is the failure mode this whole surface exists to avoid.
fn released_cutoff(raw: Option<&str>) -> ApiResult<Option<OffsetDateTime>> {
    let Some(token) = raw.map(str::trim).filter(|v| !v.is_empty()) else {
        return Ok(None);
    };
    let window = match token {
        "any" => return Ok(None),
        "24h" => Duration::hours(24),
        "7d" => Duration::days(7),
        "30d" => Duration::days(30),
        other => {
            return Err(ApiError::BadRequest(format!(
                "unknown released_since: {other:?}"
            )));
        }
    };
    Ok(Some(OffsetDateTime::now_utc() - window))
}

/// Get the watchlist
///
/// The user's watchlist, filtered/sorted/paginated **server-side**, with the tab counts and
/// group-header aggregates the list renders around it.
///
/// The body is an object rather than the bare `WatchlistItem[]` it used to be. That is a
/// breaking change, made deliberately: `total`, `counts` and `groups` cannot ride on response
/// headers without splitting one render into three requests, and the counts in particular are
/// *not* derivable from `items` — they describe the tabs the caller is not looking at. The
/// frontend is the only consumer, and it is regenerated from this document.
#[utoipa::path(
    get,
    path = "/v1/me/watchlist",
    tag = ME_WATCHLIST_TAG,
    params(WatchlistParams),
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "A page of the caller's watchlist", body = WatchlistView),
        (status = 400, description = "unrecognised filter or sort token", body = crate::error::ProblemDetails),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
    )
)]
pub async fn watchlist(
    State(state): State<AppState>,
    user: AuthUser,
    Query(params): Query<WatchlistParams>,
) -> ApiResult<Json<WatchlistView>> {
    // Parsed at the edge so an unrecognised token is a `400`, not a plausible-looking page
    // that's ordered or filtered wrong.
    let sort: WatchlistSort = parse_param(params.sort.as_deref(), "sort")?.unwrap_or_default();
    let order: WatchlistOrder =
        parse_param(params.order.as_deref(), "order")?.unwrap_or_else(|| sort.natural_order());
    let status: Option<WatchStatus> = parse_param(params.status.as_deref(), "status")?;

    let filter = WatchlistFilter {
        series_id: None,
        status,
        query: params.q,
        unread_only: params.unread_only,
        released_since: released_cutoff(params.released_since.as_deref())?,
        source_issues: params.source_issues,
        sort,
        order,
        limit: params.limit.clamp(1, MAX_LIMIT),
        offset: params.offset.clamp(0, MAX_OFFSET),
        cursor: decode_cursor(params.cursor.as_deref())?,
    };

    let page =
        tankovault_db::repo::tracking::watchlist_page(&state.pool, user.user_id, &filter).await?;

    Ok(Json(WatchlistView {
        items: page.items.into_iter().map(WatchlistItem::from).collect(),
        total: page.total,
        counts: WatchlistCounts {
            reading: page.counts.reading,
            planned: page.counts.planned,
            paused: page.counts.paused,
            completed: page.counts.completed,
            dropped: page.counts.dropped,
            all: page.counts.all,
            source_issues: page.counts.source_issues,
        },
        groups: page
            .groups
            .into_iter()
            .map(|g| WatchlistGroup {
                key: g.bucket.as_token().to_owned(),
                title_count: g.title_count,
                chapter_count: g.chapter_count,
            })
            .collect(),
        next_cursor: page.next_cursor.as_ref().map(encode_cursor),
    }))
}

/// Get the watchlist summary
///
/// Per-status counts and the unread total over the **whole** watchlist, under no filters.
///
/// Separate from the list's `counts`, which drop only the `status` arm and keep the search,
/// recency and source filters: those answer "how many would this tab show given what I have
/// typed", while this answers "how big is my library" for surfaces that carry no filter state
/// of their own — a tab badge, the More sheet, a signed-in header.
#[utoipa::path(
    get,
    path = "/v1/me/watchlist/summary",
    tag = ME_WATCHLIST_TAG,
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "Counts across the whole watchlist", body = WatchlistSummaryView),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
    )
)]
pub async fn watchlist_summary(
    State(state): State<AppState>,
    user: AuthUser,
) -> ApiResult<Json<WatchlistSummaryView>> {
    let summary =
        tankovault_db::repo::tracking::watchlist_summary(&state.pool, user.user_id).await?;
    Ok(Json(WatchlistSummaryView {
        counts: WatchlistCounts {
            reading: summary.counts.reading,
            planned: summary.counts.planned,
            paused: summary.counts.paused,
            completed: summary.counts.completed,
            dropped: summary.counts.dropped,
            all: summary.counts.all,
            source_issues: summary.counts.source_issues,
        },
        unread_total: summary.unread_total,
    }))
}

/// One series' watchlist entry, or `null` when the caller does not track it.
///
/// An object wrapping a nullable field rather than a bare nullable body: "not tracked" is a
/// perfectly ordinary answer to this question, so it is a `200` with `entry: null` rather than
/// a `404` the client has to special-case out of its error path.
#[derive(Debug, Serialize, ToSchema)]
pub struct WatchlistEntryView {
    pub entry: Option<WatchlistItem>,
}

/// Get one watchlist entry
///
/// The same enriched row the list renders, for a single series. The Series page used to answer
/// this by fetching the entire watchlist and scanning it — which stopped working the moment the
/// list paginated.
#[utoipa::path(
    get,
    path = "/v1/me/watchlist/{series_id}",
    tag = ME_WATCHLIST_TAG,
    params(("series_id" = SeriesId, Path, description = "Series id")),
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "The entry, or null when untracked", body = WatchlistEntryView),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
    )
)]
pub async fn get_watchlist_entry(
    State(state): State<AppState>,
    user: AuthUser,
    Path(series_id): Path<SeriesId>,
) -> ApiResult<Json<WatchlistEntryView>> {
    let card =
        tankovault_db::repo::tracking::watchlist_card(&state.pool, user.user_id, series_id).await?;
    Ok(Json(WatchlistEntryView {
        entry: card.map(WatchlistItem::from),
    }))
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct WatchlistUpsert {
    #[serde(default)]
    pub status: WatchStatus,
    #[serde(default = "default_true")]
    pub notify: bool,
}

fn default_true() -> bool {
    true
}

/// Add or update a watchlist entry
#[utoipa::path(
    put,
    path = "/v1/me/watchlist/{series_id}",
    tag = ME_WATCHLIST_TAG,
    params(("series_id" = SeriesId, Path, description = "Series id")),
    request_body = WatchlistUpsert,
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "Acknowledged", body = serde_json::Value, example = json!({"ok": true})),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
    )
)]
pub async fn put_watchlist(
    State(state): State<AppState>,
    user: AuthUser,
    Path(series_id): Path<SeriesId>,
    Json(body): Json<WatchlistUpsert>,
) -> ApiResult<Json<serde_json::Value>> {
    tankovault_db::repo::tracking::watchlist_upsert(
        &state.pool,
        user.user_id,
        series_id,
        body.status,
        body.notify,
    )
    .await?;
    spawn_targeted_push(&state, user.user_id, series_id);
    Ok(Json(serde_json::json!({ "ok": true })))
}

/// The source a reader wants a series to open on.
#[derive(Debug, Deserialize, ToSchema)]
pub struct SourcePin {
    /// A `series_sources` id belonging to this series; anything else is a `400`.
    pub series_source_id: SeriesSourceId,
}

/// Pin a series' source
///
/// Overrides the reader's global provider order for this one series. A separate resource from
/// the entry itself rather than a field on [`WatchlistUpsert`]: that body is a full replacement,
/// so a reader toggling `notify` would silently drop their pin.
#[utoipa::path(
    put,
    path = "/v1/me/watchlist/{series_id}/source-pin",
    tag = ME_WATCHLIST_TAG,
    params(("series_id" = SeriesId, Path, description = "Series id")),
    request_body = SourcePin,
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "Acknowledged", body = serde_json::Value, example = json!({"ok": true})),
        (status = 400, description = "the source does not belong to this series", body = crate::error::ProblemDetails),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
        (status = 404, description = "the caller does not track this series", body = crate::error::ProblemDetails),
    )
)]
pub async fn put_source_pin(
    State(state): State<AppState>,
    user: AuthUser,
    Path(series_id): Path<SeriesId>,
    Json(body): Json<SourcePin>,
) -> ApiResult<Json<serde_json::Value>> {
    set_pin(&state, user, series_id, Some(body.series_source_id)).await
}

/// Clear a series' source pin
///
/// Idempotent: clearing a pin that is not set is a `200`, because the reader's intent — "stop
/// overriding for this series" — is satisfied either way.
#[utoipa::path(
    delete,
    path = "/v1/me/watchlist/{series_id}/source-pin",
    tag = ME_WATCHLIST_TAG,
    params(("series_id" = SeriesId, Path, description = "Series id")),
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "Acknowledged", body = serde_json::Value, example = json!({"ok": true})),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
        (status = 404, description = "the caller does not track this series", body = crate::error::ProblemDetails),
    )
)]
pub async fn delete_source_pin(
    State(state): State<AppState>,
    user: AuthUser,
    Path(series_id): Path<SeriesId>,
) -> ApiResult<Json<serde_json::Value>> {
    set_pin(&state, user, series_id, None).await
}

/// Both pin writes, so the outcome mapping lives in one place.
async fn set_pin(
    state: &AppState,
    user: AuthUser,
    series_id: SeriesId,
    source_id: Option<SeriesSourceId>,
) -> ApiResult<Json<serde_json::Value>> {
    use tankovault_db::repo::tracking::PinOutcome;

    match tankovault_db::repo::tracking::watchlist_set_pinned_source(
        &state.pool,
        user.user_id,
        series_id,
        source_id,
    )
    .await?
    {
        PinOutcome::Written => Ok(Json(serde_json::json!({ "ok": true }))),
        PinOutcome::NotTracked => Err(ApiError::NotFound),
        PinOutcome::ForeignSource => Err(ApiError::BadRequest(
            "series_source_id does not belong to this series".into(),
        )),
    }
}

/// Remove a watchlist entry
#[utoipa::path(
    delete,
    path = "/v1/me/watchlist/{series_id}",
    tag = ME_WATCHLIST_TAG,
    params(("series_id" = SeriesId, Path, description = "Series id")),
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "Acknowledged", body = serde_json::Value, example = json!({"ok": true})),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
    )
)]
pub async fn delete_watchlist(
    State(state): State<AppState>,
    user: AuthUser,
    Path(series_id): Path<SeriesId>,
) -> ApiResult<Json<serde_json::Value>> {
    tankovault_db::repo::tracking::watchlist_remove(&state.pool, user.user_id, series_id).await?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

/// The ids a bulk operation should act on, plus what to change.
#[derive(Debug, Deserialize, ToSchema)]
pub struct WatchlistBulkUpdate {
    pub series_ids: Vec<SeriesId>,
    /// Leave absent to keep each entry's current status.
    #[serde(default)]
    pub status: Option<WatchStatus>,
    /// Leave absent to keep each entry's current notification setting.
    #[serde(default)]
    pub notify: Option<bool>,
}

/// The ids a bulk removal or bulk mark-read should act on.
#[derive(Debug, Deserialize, ToSchema)]
pub struct WatchlistBulkIds {
    pub series_ids: Vec<SeriesId>,
}

/// Which ids a bulk operation actually acted on, and which it did not.
///
/// Per-id rather than a count, because the two failure modes are different and the UI has to
/// tell them apart: `skipped` means the caller asked about a series it does not track (a stale
/// client, or a title removed in another tab), which is worth reporting, while a transport
/// failure fails the whole call.
#[derive(Debug, Serialize, ToSchema)]
pub struct BulkResult {
    pub applied: Vec<SeriesId>,
    pub skipped: Vec<SeriesId>,
}

impl BulkResult {
    /// Split `requested` by which of them `applied` contains, preserving the caller's order so
    /// the client can zip the answer against the selection it sent.
    pub(super) fn new(requested: &[SeriesId], applied: Vec<SeriesId>) -> Self {
        let applied_set: std::collections::HashSet<SeriesId> = applied.into_iter().collect();
        let mut out = Self {
            applied: Vec::with_capacity(applied_set.len()),
            skipped: Vec::new(),
        };
        for id in requested {
            if applied_set.contains(id) {
                out.applied.push(*id);
            } else {
                out.skipped.push(*id);
            }
        }
        out
    }
}

/// Reject an oversized or empty id list, and hand back the raw uuids the repo binds.
///
/// Refused at [`BULK_ID_LIMIT`] rather than truncated: silently acting on only the first N ids
/// would leave the rest unchanged with nothing in the response to say so.
pub(super) fn bulk_ids(series_ids: &[SeriesId]) -> ApiResult<Vec<uuid::Uuid>> {
    if series_ids.is_empty() {
        return Err(ApiError::BadRequest("series_ids must not be empty".into()));
    }
    if series_ids.len() > BULK_ID_LIMIT {
        return Err(ApiError::BadRequest(format!(
            "at most {BULK_ID_LIMIT} series_ids per call, got {}",
            series_ids.len()
        )));
    }
    Ok(series_ids.iter().map(|id| id.as_uuid()).collect())
}

/// Change many watchlist entries at once
///
/// Applies a status and/or notify change across a selection. Ids the caller does not track are
/// reported in `skipped` rather than inserted — the selection was made from the list, so an id
/// that is not on it means the client is stale, and re-adding a title the user just removed is
/// the wrong repair.
#[utoipa::path(
    post,
    path = "/v1/me/watchlist/bulk",
    tag = ME_WATCHLIST_TAG,
    request_body = WatchlistBulkUpdate,
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "Per-id outcome", body = BulkResult),
        (status = 400, description = "empty or oversized id list", body = crate::error::ProblemDetails),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
    )
)]
pub async fn bulk_update_watchlist(
    State(state): State<AppState>,
    user: AuthUser,
    Json(body): Json<WatchlistBulkUpdate>,
) -> ApiResult<Json<BulkResult>> {
    let ids = bulk_ids(&body.series_ids)?;
    let applied = tankovault_db::repo::tracking::watchlist_bulk_update(
        &state.pool,
        user.user_id,
        &ids,
        body.status,
        body.notify,
    )
    .await?;
    spawn_targeted_push_many(&state, user.user_id, applied.clone());
    Ok(Json(BulkResult::new(&body.series_ids, applied)))
}

/// Remove many watchlist entries at once
#[utoipa::path(
    delete,
    path = "/v1/me/watchlist/bulk",
    tag = ME_WATCHLIST_TAG,
    request_body = WatchlistBulkIds,
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "Per-id outcome", body = BulkResult),
        (status = 400, description = "empty or oversized id list", body = crate::error::ProblemDetails),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
    )
)]
pub async fn bulk_remove_watchlist(
    State(state): State<AppState>,
    user: AuthUser,
    Json(body): Json<WatchlistBulkIds>,
) -> ApiResult<Json<BulkResult>> {
    let ids = bulk_ids(&body.series_ids)?;
    let applied =
        tankovault_db::repo::tracking::watchlist_bulk_remove(&state.pool, user.user_id, &ids)
            .await?;
    // No targeted push: the entry is gone, so there is no local state left to reflect. Removal
    // reaching the remote provider is the sync engine's reconciliation, not a per-series push.
    Ok(Json(BulkResult::new(&body.series_ids, applied)))
}
