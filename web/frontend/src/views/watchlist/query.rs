//! The Watchlist's view state, encoded as a URL query string (§3.4) so a filtered watchlist is
//! shareable and the back button is meaningful; one catch-all `?:..query` field rather than seven.

use crate::models::{WatchStatus, WatchStatusExt};
use std::fmt;
use std::fmt::Write as _;

/// How the list is ordered. Mirrors `tankovault_db::repo::tracking::WatchlistSort`; the tokens
/// are the wire contract between them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum Sort {
    /// Newest release first — the default, and the only order the band grouping means anything under.
    #[default]
    Released,
    Unread,
    Added,
    Title,
    Progress,
}

impl Sort {
    pub(crate) const ALL: [Sort; 5] = [
        Self::Released,
        Self::Unread,
        Self::Added,
        Self::Title,
        Self::Progress,
    ];

    pub(crate) fn token(self) -> &'static str {
        match self {
            Self::Released => "released",
            Self::Unread => "unread",
            Self::Added => "added",
            Self::Title => "title",
            Self::Progress => "progress",
        }
    }

    /// The catalogue key of this option's display name (see [`crate::i18n`]).
    pub(crate) fn label_key(self) -> &'static str {
        match self {
            Self::Released => "watchlist.sort.released",
            Self::Unread => "watchlist.sort.unread",
            Self::Added => "watchlist.sort.added",
            Self::Title => "watchlist.sort.title",
            Self::Progress => "watchlist.sort.progress",
        }
    }

    /// An unrecognised token falls back to the default rather than refusing the route — a typo
    /// in a hand-edited URL should land on the default list, not a broken page.
    fn parse(token: &str) -> Self {
        Self::ALL
            .into_iter()
            .find(|s| s.token() == token)
            .unwrap_or_default()
    }

    /// The direction that reads as "most interesting first" — mirrors the server's rule so the
    /// column caret can be drawn without a round trip.
    pub(crate) fn natural_order(self) -> Order {
        match self {
            Self::Title => Order::Asc,
            _ => Order::Desc,
        }
    }

    /// Whether release-recency group headers apply — meaningless once sorted by title or
    /// progress, since a `TODAY` header over alphabetized rows is noise.
    pub(crate) fn groups_by_release(self) -> bool {
        self == Self::Released
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Order {
    Asc,
    Desc,
}

impl Order {
    pub(crate) fn token(self) -> &'static str {
        match self {
            Self::Asc => "asc",
            Self::Desc => "desc",
        }
    }

    fn parse(token: &str) -> Option<Self> {
        match token {
            "asc" => Some(Self::Asc),
            "desc" => Some(Self::Desc),
            _ => None,
        }
    }

    pub(crate) fn flip(self) -> Self {
        match self {
            Self::Asc => Self::Desc,
            Self::Desc => Self::Asc,
        }
    }
}

/// The `Released` filter's window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum Released {
    #[default]
    Any,
    Day,
    Week,
    Month,
}

impl Released {
    pub(crate) const ALL: [Released; 4] = [Self::Any, Self::Day, Self::Week, Self::Month];

    /// The wire token; `Any` is the absence of the parameter, not a value, so a shared URL
    /// doesn't carry a no-op.
    pub(crate) fn token(self) -> &'static str {
        match self {
            Self::Any => "",
            Self::Day => "24h",
            Self::Week => "7d",
            Self::Month => "30d",
        }
    }

    /// The catalogue key of this option's display name (see [`crate::i18n`]).
    pub(crate) fn label_key(self) -> &'static str {
        match self {
            Self::Any => "watchlist.released.any",
            Self::Day => "watchlist.released.day",
            Self::Week => "watchlist.released.week",
            Self::Month => "watchlist.released.month",
        }
    }

    fn parse(token: &str) -> Self {
        Self::ALL
            .into_iter()
            .find(|r| r.token() == token)
            .unwrap_or_default()
    }
}

/// List (option 4a) or cover grid (option 4b). Same data, same filters, same groups.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum View {
    #[default]
    List,
    Grid,
}

impl View {
    pub(crate) fn token(self) -> &'static str {
        match self {
            Self::List => "list",
            Self::Grid => "grid",
        }
    }

    pub(crate) fn parse(token: &str) -> Self {
        match token {
            "grid" => Self::Grid,
            _ => Self::List,
        }
    }
}

/// The complete Watchlist view state.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct WatchlistQuery {
    /// `None` is the `All` tab, not a missing value.
    pub(crate) status: Option<WatchStatus>,
    pub(crate) sort: Sort,
    /// `None` means whichever direction [`Sort::natural_order`] gives — keeps `?sort=title` off
    /// the surprising Z→A order without pinning every other sort's direction into the URL.
    pub(crate) order: Option<Order>,
    pub(crate) q: String,
    /// Defaults to **on** — the list is a triage queue, and most of a large watchlist is noise
    /// without this.
    pub(crate) unread_only: bool,
    /// Only titles whose preferred source is unhealthy.
    pub(crate) source_issues: bool,
    pub(crate) released: Released,
    pub(crate) view: View,
}

impl Default for WatchlistQuery {
    fn default() -> Self {
        Self {
            status: Some(WatchStatus::Reading),
            sort: Sort::default(),
            order: None,
            q: String::new(),
            unread_only: true,
            source_issues: false,
            released: Released::default(),
            view: View::default(),
        }
    }
}

impl WatchlistQuery {
    /// The direction actually in force.
    pub(crate) fn effective_order(&self) -> Order {
        self.order.unwrap_or_else(|| self.sort.natural_order())
    }

    /// Whether any filter narrows the list beyond the default — gates the "widen your filter"
    /// empty state so it doesn't show when there's nothing to widen.
    pub(crate) fn is_narrowed(&self) -> bool {
        !self.q.is_empty()
            || self.released != Released::Any
            || self.unread_only
            || self.source_issues
    }

    /// The status token to send, or `None` for the `All` tab.
    pub(crate) fn status_token(&self) -> Option<&'static str> {
        self.status.map(|s| s.token())
    }
}

/// Parse the query string. Unknown keys and unparseable values fall back to the default for
/// that field: a URL is user-editable, and half-understanding one is better than a blank page.
impl From<&str> for WatchlistQuery {
    fn from(query: &str) -> Self {
        let mut out = Self::default();
        for pair in query.split('&').filter(|s| !s.is_empty()) {
            let (key, raw) = pair.split_once('=').unwrap_or((pair, ""));
            let value = decode_component(raw);
            match key {
                "status" => out.status = parse_status(&value),
                "sort" => out.sort = Sort::parse(&value),
                "order" => out.order = Order::parse(&value),
                "q" => out.q = value,
                // Present-and-`0` is the only way to turn off the default; absence keeps it on.
                "unread" => out.unread_only = value != "0",
                "issues" => out.source_issues = value != "0",
                "released" => out.released = Released::parse(&value),
                "view" => out.view = View::parse(&value),
                _ => {}
            }
        }
        out
    }
}

/// Write only what differs from the default, so the rail's link is `/watchlist?` and a shared
/// URL names exactly what its sender changed.
impl fmt::Display for WatchlistQuery {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let default = Self::default();
        let mut parts: Vec<String> = Vec::new();
        if self.status != default.status {
            // `all`, not empty — the `All` tab is a choice; an absent parameter already means default.
            parts.push(format!(
                "status={}",
                self.status.map_or("all", |s| s.token())
            ));
        }
        if self.sort != default.sort {
            parts.push(format!("sort={}", self.sort.token()));
        }
        if let Some(order) = self.order {
            parts.push(format!("order={}", order.token()));
        }
        if !self.q.is_empty() {
            parts.push(format!("q={}", encode_component(&self.q)));
        }
        if !self.unread_only {
            parts.push("unread=0".to_owned());
        }
        if self.source_issues {
            parts.push("issues=1".to_owned());
        }
        if self.released != default.released {
            parts.push(format!("released={}", self.released.token()));
        }
        if self.view != default.view {
            parts.push(format!("view={}", self.view.token()));
        }
        write!(f, "{}", parts.join("&"))
    }
}

/// `all` is the `All` tab; anything unrecognised is too, rather than silently falling back to
/// `Reading` (what [`WatchStatusExt::parse`] would do) and hiding most of the list.
fn parse_status(token: &str) -> Option<WatchStatus> {
    if token == "all" {
        return None;
    }
    WatchStatus::all()
        .iter()
        .copied()
        .find(|s| s.token() == token)
}

/// Percent-encode everything the query grammar reserves.
///
/// The router's own encoding leaves `&` and `=` alone, so a filter like `fate/stay & night`
/// would otherwise re-parse as two parameters.
fn encode_component(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char);
            }
            // `write!` into a `String` is infallible, so discarding the `Result` is safe (and
            // avoids an allocation per escaped byte).
            _ => {
                let _ = write!(out, "%{byte:02X}");
            }
        }
    }
    out
}

/// The inverse of [`encode_component`]. A malformed escape is kept verbatim rather than
/// dropped — a hand-typed `100%` in the filter box should search for `100%`, not `100`.
fn decode_component(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(byte) = u8::from_str_radix(&value[i + 1..i + 3], 16) {
                out.push(byte);
                i += 3;
                continue;
            }
        }
        // `+` is a form-encoding convention `encode_component` never emits, but pasted URLs do.
        out.push(if bytes[i] == b'+' { b' ' } else { bytes[i] });
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every field must round-trip: one that doesn't is a filter that silently resets on reload.
    #[test]
    fn every_field_round_trips_through_the_query_string() {
        let cases = [
            WatchlistQuery::default(),
            WatchlistQuery {
                status: None,
                sort: Sort::Progress,
                order: Some(Order::Asc),
                q: "one piece".to_owned(),
                unread_only: false,
                source_issues: true,
                released: Released::Week,
                view: View::Grid,
            },
            WatchlistQuery {
                status: Some(WatchStatus::Dropped),
                sort: Sort::Title,
                order: None,
                q: String::new(),
                unread_only: true,
                source_issues: false,
                released: Released::Any,
                view: View::List,
            },
        ];
        for case in cases {
            let encoded = case.to_string();
            assert_eq!(
                WatchlistQuery::from(encoded.as_str()),
                case,
                "round trip failed for {encoded:?}"
            );
        }
    }

    /// The default state must not name itself in the URL, or every rail click rewrites the
    /// address bar with six parameters the reader did not choose.
    #[test]
    fn the_default_query_is_empty() {
        assert_eq!(WatchlistQuery::default().to_string(), "");
    }

    /// A filter containing the query grammar's own separators used to be re-parsed as extra
    /// parameters, losing everything after the `&`.
    #[test]
    fn reserved_characters_in_the_filter_survive() {
        let query = WatchlistQuery {
            q: "fate/stay & night = ?".to_owned(),
            ..WatchlistQuery::default()
        };
        let parsed = WatchlistQuery::from(query.to_string().as_str());
        assert_eq!(parsed.q, "fate/stay & night = ?");
    }

    /// Non-ASCII has to survive too — a good part of this catalogue is not spelled in Latin.
    #[test]
    fn non_ascii_filters_survive() {
        let query = WatchlistQuery {
            q: "九番の鐘".to_owned(),
            ..WatchlistQuery::default()
        };
        assert_eq!(
            WatchlistQuery::from(query.to_string().as_str()).q,
            "九番の鐘"
        );
    }

    /// An unrecognised status must land on the `All` tab, never on `Reading`: falling back to a
    /// real status hides four fifths of the list while looking like it worked.
    #[test]
    fn an_unknown_status_is_the_all_tab_not_reading() {
        assert_eq!(WatchlistQuery::from("status=nonsense").status, None);
        assert_eq!(WatchlistQuery::from("status=all").status, None);
        assert_eq!(
            WatchlistQuery::from("status=paused").status,
            Some(WatchStatus::Paused)
        );
    }

    /// The router writes an empty string for `/watchlist?`'s default route.
    #[test]
    fn an_empty_query_is_the_default_state() {
        assert_eq!(WatchlistQuery::from(""), WatchlistQuery::default());
    }

    /// A malformed escape is data, not an error.
    #[test]
    fn a_malformed_escape_is_kept_verbatim() {
        assert_eq!(decode_component("100%"), "100%");
        assert_eq!(decode_component("%zz"), "%zz");
    }
}
