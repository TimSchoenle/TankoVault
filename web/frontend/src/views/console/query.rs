//! The console's view state, encoded as a URL query string, so an operator can paste "the
//! provider that is failing" into chat and the receiver lands on the same row.
//!
//! Follows `views::watchlist::query` exactly — one catch-all `?:..query` field with `Default`,
//! `From<&str>`, `Display` and a round-trip test — because two encoders that disagree about
//! escaping is a defect neither one's tests would find.

use std::fmt;
use std::fmt::Write as _;

/// A time window on the panels that have one (Audit, Scans, Overview).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum Window {
    /// The absence of the parameter, not a value — a shared URL should not carry a no-op.
    #[default]
    Any,
    Hour,
    Day,
    Week,
}

impl Window {
    pub(crate) const ALL: [Window; 4] = [Self::Any, Self::Hour, Self::Day, Self::Week];

    pub(crate) fn token(self) -> &'static str {
        match self {
            Self::Any => "",
            Self::Hour => "1h",
            Self::Day => "24h",
            Self::Week => "7d",
        }
    }

    /// The catalogue key of this window's display name (see [`crate::i18n`]).
    pub(crate) fn label_key(self) -> &'static str {
        match self {
            Self::Any => "console.window.any",
            Self::Hour => "console.window.hour",
            Self::Day => "console.window.day",
            Self::Week => "console.window.week",
        }
    }

    /// How many hours back this window reaches; `None` is no lower bound at all.
    ///
    /// `u32` so the conversion to milliseconds below is lossless — these are small numbers, and
    /// an `i64` here would only buy a precision-loss lint.
    pub(crate) fn hours(self) -> Option<u32> {
        match self {
            Self::Any => None,
            Self::Hour => Some(1),
            Self::Day => Some(24),
            Self::Week => Some(24 * 7),
        }
    }

    /// The RFC 3339 instant this window starts at, as the API's `since` parameter wants it.
    ///
    /// `None` for [`Window::Any`], which is the absence of the parameter rather than a bound at
    /// the beginning of time. Computed from the operator's own clock: the window is what they
    /// see on their own screen, and a server-relative one would drift against it.
    pub(crate) fn since_iso(self) -> Option<String> {
        const MS_PER_HOUR: f64 = 3_600_000.0;
        let hours = self.hours()?;
        let start = crate::platform::now_ms() - f64::from(hours) * MS_PER_HOUR;
        crate::platform::format_timestamp_iso(start)
    }

    /// Parse a `?since=` token. An unrecognised one widens to "any time" rather than refusing
    /// the link — a hand-edited URL should show more, never an error.
    pub(crate) fn parse_token(token: &str) -> Self {
        Self::parse(token)
    }

    fn parse(token: &str) -> Self {
        Self::ALL
            .into_iter()
            .find(|w| w.token() == token)
            .unwrap_or_default()
    }
}

/// The merge queue's confidence band.
///
/// An enum rather than the raw `min_score` float it sends: a float in the URL round-trips
/// through decimal formatting, and the operator picks from four buttons regardless.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum Band {
    #[default]
    All,
    Low,
    Medium,
    High,
}

impl Band {
    pub(crate) const ALL: [Band; 4] = [Self::All, Self::Low, Self::Medium, Self::High];

    /// The `min_score` this band sends to `list_merge_candidates`.
    pub(crate) fn min_score(self) -> f32 {
        match self {
            Self::All => 0.0,
            Self::Low => 0.6,
            Self::Medium => 0.75,
            Self::High => 0.9,
        }
    }

    pub(crate) fn token(self) -> &'static str {
        match self {
            Self::All => "",
            Self::Low => "low",
            Self::Medium => "med",
            Self::High => "high",
        }
    }

    /// The catalogue key of this band's display name (see [`crate::i18n`]).
    pub(crate) fn label_key(self) -> &'static str {
        match self {
            Self::All => "console.merge.bandAll",
            Self::Low => "console.merge.bandLow",
            Self::Medium => "console.merge.bandMed",
            Self::High => "console.merge.bandHigh",
        }
    }

    fn parse(token: &str) -> Self {
        Self::ALL
            .into_iter()
            .find(|b| b.token() == token)
            .unwrap_or_default()
    }
}

/// The default provider for the Sync page's two mapping queues.
const DEFAULT_SYNC_PROVIDER: &str = "anilist";

/// One Sync sub-queue's filter pair.
///
/// Namespaced in the URL (`assign.q`, `remote.q`) because the Sync page renders two of these
/// on one scrolling surface, and a single shared `q` would filter both from one box.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct QueueFilter {
    pub(crate) provider: String,
    pub(crate) q: String,
}

impl Default for QueueFilter {
    fn default() -> Self {
        Self {
            provider: DEFAULT_SYNC_PROVIDER.to_owned(),
            q: String::new(),
        }
    }
}

/// The complete console view state below the entity segment.
///
/// Every field is a filter or a selection that an operator would lose on reload today. The
/// entity itself is *not* here — it is the path segment, so `/console/providers` is a place
/// rather than a parameter.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct ConsoleQuery {
    /// The selected row, as the string the panel's rows are keyed by.
    ///
    /// Not a `Uuid`: Providers key on `ProviderId`, Users on an opaque account id and Sync on a
    /// series id, and parsing here would reject a shape only the panel knows is valid.
    pub(crate) sel: Option<String>,
    /// The inspector's tab, as that tab strip's own token.
    pub(crate) tab: Option<String>,
    /// The panel's primary `ListSearch` text.
    pub(crate) q: String,
    /// Users' status chip, Privacy's queue filter, Scans' run state — three panels, three
    /// vocabularies, one parameter, each parsed by the panel that owns it.
    pub(crate) status: Option<String>,
    /// Provider slug filter on Scans and Audit.
    pub(crate) provider: Option<String>,
    pub(crate) since: Window,
    /// Zero-based page on the paged panels.
    pub(crate) page: u32,
    /// Users' staff-only chip.
    pub(crate) staff: bool,
    pub(crate) band: Band,
    pub(crate) assign: QueueFilter,
    pub(crate) remote: QueueFilter,
}

impl ConsoleQuery {
    /// The state a rail click lands on: this entity, nothing selected, no filters.
    pub(crate) fn fresh() -> Self {
        Self::default()
    }

    /// The same view with a different row selected, and the page reset.
    ///
    /// Paging is dropped deliberately: a `sel` from page 4 pasted into a link that also carries
    /// `page=4` re-selects correctly, but every *other* way of arriving at a selection means the
    /// operator picked a visible row, and keeping a stale page would scroll it out of the list.
    pub(crate) fn with_selection(&self, sel: Option<String>) -> Self {
        Self {
            sel,
            ..self.clone()
        }
    }

    /// The same view on a different inspector tab.
    pub(crate) fn with_tab(&self, tab: &str) -> Self {
        Self {
            tab: Some(tab.to_owned()),
            ..self.clone()
        }
    }

    /// The same view with the primary search replaced, back on page one.
    pub(crate) fn with_search(&self, q: String) -> Self {
        Self {
            q,
            page: 0,
            ..self.clone()
        }
    }

    /// The same view on another page.
    pub(crate) fn with_page(&self, page: u32) -> Self {
        Self {
            page,
            ..self.clone()
        }
    }

    /// The status token, as the panel's own enum sees it.
    pub(crate) fn status_token(&self) -> &str {
        self.status.as_deref().unwrap_or_default()
    }

    /// The tab token, as the panel's own strip sees it.
    pub(crate) fn tab_token(&self) -> &str {
        self.tab.as_deref().unwrap_or_default()
    }
}

/// Parse the query string. Unknown keys and unparseable values fall back to the default for
/// that field: a URL is user-editable, and half-understanding one beats a blank console.
impl From<&str> for ConsoleQuery {
    fn from(query: &str) -> Self {
        let mut out = Self::default();
        for pair in query.split('&').filter(|s| !s.is_empty()) {
            let (key, raw) = pair.split_once('=').unwrap_or((pair, ""));
            let value = decode_component(raw);
            match key {
                "sel" => out.sel = non_empty(value),
                "tab" => out.tab = non_empty(value),
                "q" => out.q = value,
                "status" => out.status = non_empty(value),
                "provider" => out.provider = non_empty(value),
                "since" => out.since = Window::parse(&value),
                "page" => out.page = value.parse().unwrap_or(0),
                "staff" => out.staff = value != "0",
                "band" => out.band = Band::parse(&value),
                "assign.p" => out.assign.provider = value,
                "assign.q" => out.assign.q = value,
                "remote.p" => out.remote.provider = value,
                "remote.q" => out.remote.q = value,
                _ => {}
            }
        }
        out
    }
}

/// Write only what differs from the default, so a rail click is `/console/providers` and a
/// shared URL names exactly what its sender changed.
impl fmt::Display for ConsoleQuery {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let default = Self::default();
        let mut parts: Vec<String> = Vec::new();
        let mut put =
            |key: &str, value: &str| parts.push(format!("{key}={}", encode_component(value)));

        if let Some(sel) = &self.sel {
            put("sel", sel);
        }
        if let Some(tab) = &self.tab {
            put("tab", tab);
        }
        if !self.q.is_empty() {
            put("q", &self.q);
        }
        if let Some(status) = &self.status {
            put("status", status);
        }
        if let Some(provider) = &self.provider {
            put("provider", provider);
        }
        if self.since != default.since {
            put("since", self.since.token());
        }
        if self.page != 0 {
            put("page", &self.page.to_string());
        }
        if self.staff {
            put("staff", "1");
        }
        if self.band != default.band {
            put("band", self.band.token());
        }
        if self.assign != default.assign {
            put("assign.p", &self.assign.provider);
            put("assign.q", &self.assign.q);
        }
        if self.remote != default.remote {
            put("remote.p", &self.remote.provider);
            put("remote.q", &self.remote.q);
        }
        write!(f, "{}", parts.join("&"))
    }
}

/// An empty parameter is the absence of a filter, never a filter matching the empty string.
fn non_empty(value: String) -> Option<String> {
    if value.is_empty() {
        None
    } else {
        Some(value)
    }
}

/// Percent-encode everything the query grammar reserves.
///
/// The router's own encoding leaves `&` and `=` alone, so a provider slug or a search term
/// carrying one would otherwise re-parse as two parameters.
fn encode_component(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char);
            }
            // `write!` into a `String` is infallible, so discarding the `Result` is safe.
            _ => {
                let _ = write!(out, "%{byte:02X}");
            }
        }
    }
    out
}

/// The inverse of [`encode_component`]. A malformed escape is kept verbatim rather than
/// dropped — a hand-typed `100%` in a filter box should search for `100%`, not `100`.
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
    use crate::views::console::ConsoleEntity;

    /// Every field must round-trip: one that does not is a filter that silently resets on
    /// reload, which is the whole defect this module exists to close.
    #[test]
    fn every_field_round_trips_through_the_query_string() {
        let cases = [
            ConsoleQuery::default(),
            ConsoleQuery {
                sel: Some("018f4c2a-0000-7000-8000-000000000001".to_owned()),
                tab: Some("politeness".to_owned()),
                q: "kun manga".to_owned(),
                status: Some("suspended".to_owned()),
                provider: Some("kunmanga".to_owned()),
                since: Window::Week,
                page: 4,
                staff: true,
                band: Band::High,
                assign: QueueFilter {
                    provider: "mangadex".to_owned(),
                    q: "naruto".to_owned(),
                },
                remote: QueueFilter {
                    provider: "anilist".to_owned(),
                    q: "one piece".to_owned(),
                },
            },
            ConsoleQuery {
                since: Window::Hour,
                band: Band::Low,
                ..ConsoleQuery::default()
            },
        ];
        for case in cases {
            let encoded = case.to_string();
            assert_eq!(
                ConsoleQuery::from(encoded.as_str()),
                case,
                "round trip failed for {encoded:?}"
            );
        }
    }

    /// The default state must not name itself in the URL, or every rail click rewrites the
    /// address bar with eleven parameters the operator did not choose.
    #[test]
    fn the_default_query_is_empty() {
        assert_eq!(ConsoleQuery::default().to_string(), "");
        assert_eq!(ConsoleQuery::from(""), ConsoleQuery::default());
    }

    /// A search term containing the query grammar's own separators must survive: the console
    /// searches provider slugs and series titles, and both contain `&` and `=` in the wild.
    #[test]
    fn reserved_characters_in_a_filter_survive() {
        let query = ConsoleQuery {
            q: "fate/stay & night = ?".to_owned(),
            ..ConsoleQuery::default()
        };
        assert_eq!(
            ConsoleQuery::from(query.to_string().as_str()).q,
            "fate/stay & night = ?"
        );
    }

    /// Non-ASCII has to survive too — a good part of this catalogue is not spelled in Latin.
    #[test]
    fn non_ascii_filters_survive() {
        let query = ConsoleQuery {
            q: "九番の鐘".to_owned(),
            ..ConsoleQuery::default()
        };
        assert_eq!(ConsoleQuery::from(query.to_string().as_str()).q, "九番の鐘");
    }

    /// An empty parameter must clear the filter rather than search for the empty string —
    /// `?sel=` is what a hand-cleared URL looks like, and `Some("")` would select no row while
    /// suppressing the fall-back-to-first-row behaviour.
    #[test]
    fn an_empty_parameter_is_an_absent_filter() {
        let parsed = ConsoleQuery::from("sel=&tab=&status=&provider=");
        assert_eq!(parsed.sel, None);
        assert_eq!(parsed.tab, None);
        assert_eq!(parsed.status, None);
        assert_eq!(parsed.provider, None);
    }

    /// A nonsense page must not wedge the pager: `?page=` and `?page=nonsense` are both page one.
    #[test]
    fn an_unparseable_page_is_page_one() {
        assert_eq!(ConsoleQuery::from("page=nonsense").page, 0);
        assert_eq!(ConsoleQuery::from("page=").page, 0);
        assert_eq!(ConsoleQuery::from("page=7").page, 7);
    }

    /// Every rail entity's slug is its URL, so a slug that does not parse back to itself is a
    /// rail entry no link can reach — the same defect class `the_picker_offers_every_adapter_kind`
    /// pins for the adapter vocabulary.
    #[test]
    fn every_entity_slug_parses_back_to_itself() {
        for entity in ConsoleEntity::ALL {
            let slug = entity.slug();
            assert_eq!(
                slug.parse::<ConsoleEntity>().ok(),
                Some(entity),
                "`{slug}` does not round-trip through the route segment"
            );
            assert_eq!(entity.to_string(), slug);
        }
    }
}
