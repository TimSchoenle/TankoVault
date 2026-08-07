//! Discover's view state, encoded as a URL query string so a filtered grid is shareable and the
//! back button is meaningful — the same contract the Watchlist keeps in `views::watchlist::query`.
//!
//! The split between [`DiscoverFilters`] and [`DiscoverQuery::at`] is load-bearing rather than
//! cosmetic: everything that changes *which* series match lives in the filters, and the anchor is
//! only *where in that result set* the reader was. The grid rebuilds its window when the filters
//! change and holds it when the anchor moves, so anything added to the wrong half either fails to
//! reset the grid or resets it on every scroll tick.

use crate::models::{ContentType, ContentTypeExt, SeriesStatus, SeriesStatusExt};
use crate::util::{decode_component, encode_component};
use std::fmt;

/// Lowest / highest release year the panel's slider exposes; sending a bound only when the
/// user narrows past these avoids the server dropping series with an unknown year.
pub(crate) const YEAR_MIN: i32 = 1970;
pub(crate) const YEAR_MAX: i32 = 2026;

/// Highest minimum-chapter count the panel's slider reaches.
pub(crate) const MIN_CHAPTERS_MAX: i32 = 500;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum Sort {
    #[default]
    Updated,
    /// Best match for the query. Only reachable from Search, which is the same endpoint with a
    /// `query` — the Discover grid has nothing to rank, so this is not in [`Sort::ALL`].
    Relevance,
    Title,
    Chapters,
    Rating,
    Year,
}

impl Sort {
    /// The orders the Discover control offers, in menu order.
    ///
    /// "Most sources" is deliberately absent. It ranks by how many providers this deployment
    /// happens to have crawled a title on, which is an operational fact about the crawler and
    /// not a property of the series — the same reasoning that took the source count off the
    /// cards. The token is still accepted by the API for any client that sends it.
    pub(crate) const ALL: [Sort; 5] = [
        Self::Updated,
        Self::Title,
        Self::Chapters,
        Self::Rating,
        Self::Year,
    ];

    /// The catalogue key of this option's display name (see [`crate::i18n`]).
    pub(crate) fn label_key(self) -> &'static str {
        match self {
            Self::Updated => "discover.sort.updated",
            Self::Relevance => "discover.sort.relevance",
            Self::Title => "discover.sort.title",
            Self::Chapters => "discover.sort.chapters",
            Self::Rating => "discover.sort.rating",
            Self::Year => "discover.sort.year",
        }
    }

    pub(crate) fn token(self) -> &'static str {
        match self {
            Self::Updated => "updated",
            Self::Relevance => "relevance",
            Self::Title => "title",
            Self::Chapters => "chapters",
            Self::Rating => "rating",
            Self::Year => "year",
        }
    }

    pub(crate) fn parse(token: &str) -> Self {
        match token {
            "relevance" => Self::Relevance,
            "title" => Self::Title,
            "chapters" => Self::Chapters,
            "rating" => Self::Rating,
            "year" => Self::Year,
            _ => Self::Updated,
        }
    }
}

/// Everything that decides *which* series the grid shows, and in what order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DiscoverFilters {
    /// The API takes one content type; the panel is multi-select because the chips read as a
    /// set, and the first is what gets sent.
    pub(crate) types: Vec<ContentType>,
    pub(crate) statuses: Vec<SeriesStatus>,
    /// Tag slugs a series must carry.
    pub(crate) inc: Vec<String>,
    /// Tag slugs a series must not carry.
    pub(crate) exc: Vec<String>,
    pub(crate) year_min: i32,
    pub(crate) year_max: i32,
    pub(crate) min_chapters: i32,
    pub(crate) provider: Option<String>,
    pub(crate) sort: Sort,
}

impl Default for DiscoverFilters {
    fn default() -> Self {
        Self {
            types: Vec::new(),
            statuses: Vec::new(),
            inc: Vec::new(),
            exc: Vec::new(),
            year_min: YEAR_MIN,
            year_max: YEAR_MAX,
            min_chapters: 0,
            provider: None,
            sort: Sort::default(),
        }
    }
}

impl DiscoverFilters {
    /// How many filters the chip bar and the panel's counter report. The sort and the two
    /// sliders are excluded on purpose: a slider parked at its own bound narrows nothing, and
    /// counting it would make "3 active" true on a screen the reader never touched.
    pub(crate) fn active_count(&self) -> usize {
        self.types.len()
            + self.statuses.len()
            + self.inc.len()
            + self.exc.len()
            + usize::from(self.provider.is_some())
    }

    /// Toggle one value in a list filter, which is what every chip in the panel does.
    fn toggle<T: PartialEq>(list: &mut Vec<T>, value: T) {
        if let Some(index) = list.iter().position(|existing| *existing == value) {
            list.remove(index);
        } else {
            list.push(value);
        }
    }

    pub(crate) fn toggle_type(&mut self, value: ContentType) {
        Self::toggle(&mut self.types, value);
    }

    pub(crate) fn toggle_status(&mut self, value: SeriesStatus) {
        Self::toggle(&mut self.statuses, value);
    }

    /// Advance one tag through the chip's three states: neutral → include → exclude → neutral.
    pub(crate) fn cycle_tag(&mut self, slug: &str) {
        if let Some(index) = self.inc.iter().position(|s| s == slug) {
            self.inc.remove(index);
            self.exc.push(slug.to_owned());
        } else if let Some(index) = self.exc.iter().position(|s| s == slug) {
            self.exc.remove(index);
        } else {
            self.inc.push(slug.to_owned());
        }
    }

    pub(crate) fn drop_tag(&mut self, slug: &str) {
        self.inc.retain(|s| s != slug);
        self.exc.retain(|s| s != slug);
    }
}

/// The complete Discover view state: what to show, and where in it the reader was.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct DiscoverQuery {
    pub(crate) filters: DiscoverFilters,
    /// 0-based index, within the filtered result set, of the first card the reader had in view.
    ///
    /// The grid pages on a scroll sentinel, so this — not a page number — is what makes a
    /// position shareable: the page size is derived from the window's width, so the same page
    /// number is a different series on a phone than on a desktop.
    pub(crate) at: usize,
}

impl DiscoverQuery {
    /// The same filters, back at the top. Every filter control routes through this: a narrowed
    /// result set has nothing at the old anchor, so keeping it would open the grid past its end.
    pub(crate) fn with_filters(filters: DiscoverFilters) -> Self {
        Self { filters, at: 0 }
    }
}

/// Parse the query string. Unknown keys and unparseable values fall back to the default for
/// that field: a URL is user-editable, and half-understanding one is better than a blank page.
impl From<&str> for DiscoverQuery {
    fn from(query: &str) -> Self {
        let mut out = Self::default();
        for pair in query.split('&').filter(|s| !s.is_empty()) {
            let (key, raw) = pair.split_once('=').unwrap_or((pair, ""));
            // Decoded per value, never up front: a list parameter has to be split on its
            // separator *before* its members are decoded, or a slug carrying an encoded comma
            // comes back out as two tags.
            match key {
                "type" => {
                    out.filters.types = parse_list(
                        raw,
                        <ContentType as ContentTypeExt>::all(),
                        ContentTypeExt::token,
                    );
                }
                "status" => {
                    out.filters.statuses = parse_list(
                        raw,
                        <SeriesStatus as SeriesStatusExt>::all(),
                        SeriesStatusExt::token,
                    );
                }
                "tag" => out.filters.inc = split_slugs(raw),
                "not" => out.filters.exc = split_slugs(raw),
                "from" => out.filters.year_min = parse_year(raw, YEAR_MIN),
                "to" => out.filters.year_max = parse_year(raw, YEAR_MAX),
                "min" => {
                    out.filters.min_chapters = raw.parse().unwrap_or(0).clamp(0, MIN_CHAPTERS_MAX);
                }
                "provider" => {
                    out.filters.provider = Some(decode_component(raw)).filter(|v| !v.is_empty());
                }
                "sort" => out.filters.sort = Sort::parse(raw),
                "at" => out.at = raw.parse().unwrap_or(0),
                _ => {}
            }
        }
        // A window whose bounds are crossed matches nothing at all, which reads as a broken
        // screen rather than as the typo in the URL that it is.
        if out.filters.year_min > out.filters.year_max {
            std::mem::swap(&mut out.filters.year_min, &mut out.filters.year_max);
        }
        out
    }
}

/// Write only what differs from the default, so the rail's link stays `/discover?` and a shared
/// URL names exactly what its sender changed.
impl fmt::Display for DiscoverQuery {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let default = DiscoverFilters::default();
        let filters = &self.filters;
        let mut parts: Vec<String> = Vec::new();
        if !filters.types.is_empty() {
            parts.push(format!(
                "type={}",
                join_tokens(&filters.types, ContentTypeExt::token)
            ));
        }
        if !filters.statuses.is_empty() {
            parts.push(format!(
                "status={}",
                join_tokens(&filters.statuses, SeriesStatusExt::token)
            ));
        }
        if !filters.inc.is_empty() {
            parts.push(format!("tag={}", join_slugs(&filters.inc)));
        }
        if !filters.exc.is_empty() {
            parts.push(format!("not={}", join_slugs(&filters.exc)));
        }
        if filters.year_min != default.year_min {
            parts.push(format!("from={}", filters.year_min));
        }
        if filters.year_max != default.year_max {
            parts.push(format!("to={}", filters.year_max));
        }
        if filters.min_chapters != default.min_chapters {
            parts.push(format!("min={}", filters.min_chapters));
        }
        if let Some(provider) = &filters.provider {
            parts.push(format!("provider={}", encode_component(provider)));
        }
        if filters.sort != default.sort {
            parts.push(format!("sort={}", filters.sort.token()));
        }
        if self.at > 0 {
            parts.push(format!("at={}", self.at));
        }
        write!(f, "{}", parts.join("&"))
    }
}

/// A comma-separated token list, with unrecognised entries dropped rather than defaulted — an
/// unknown content type is a filter this build cannot honour, and silently substituting `manga`
/// would answer a question nobody asked.
fn parse_list<T: Copy + PartialEq>(
    value: &str,
    all: &[T],
    token: impl Fn(&T) -> &'static str,
) -> Vec<T> {
    let mut out: Vec<T> = Vec::new();
    for wanted in value.split(',').filter(|t| !t.is_empty()) {
        if let Some(parsed) = all.iter().copied().find(|value| token(value) == wanted) {
            if !out.contains(&parsed) {
                out.push(parsed);
            }
        }
    }
    out
}

fn join_tokens<T>(values: &[T], token: impl Fn(&T) -> &'static str) -> String {
    values.iter().map(token).collect::<Vec<_>>().join(",")
}

/// Tag slugs are `[a-z0-9-]` by construction, but they arrive from the API rather than from this
/// build, so they are encoded like any other value — a slug carrying a `&` would otherwise take
/// every parameter after it with it.
fn join_slugs(slugs: &[String]) -> String {
    slugs
        .iter()
        .map(|slug| encode_component(slug))
        .collect::<Vec<_>>()
        .join(",")
}

fn split_slugs(value: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for slug in value.split(',').filter(|s| !s.is_empty()) {
        let slug = decode_component(slug);
        if !out.contains(&slug) {
            out.push(slug);
        }
    }
    out
}

fn parse_year(value: &str, fallback: i32) -> i32 {
    value
        .parse()
        .map_or(fallback, |year: i32| year.clamp(YEAR_MIN, YEAR_MAX))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every field must round-trip: one that doesn't is a filter that silently resets on reload,
    /// or — for `at` — a shared link that lands somewhere its sender never was.
    #[test]
    fn every_field_round_trips_through_the_query_string() {
        let cases = [
            DiscoverQuery::default(),
            DiscoverQuery {
                filters: DiscoverFilters {
                    types: vec![ContentType::Manhwa, ContentType::Webtoon],
                    statuses: vec![SeriesStatus::Ongoing],
                    inc: vec!["action".to_owned(), "drama".to_owned()],
                    exc: vec!["ecchi".to_owned()],
                    year_min: 2005,
                    year_max: 2020,
                    min_chapters: 120,
                    provider: Some("kunmanga".to_owned()),
                    sort: Sort::Rating,
                },
                at: 384,
            },
        ];
        for case in cases {
            let encoded = case.to_string();
            assert_eq!(
                DiscoverQuery::from(encoded.as_str()),
                case,
                "round trip failed for {encoded:?}"
            );
        }
    }

    /// The default state must not name itself in the URL, or every rail click rewrites the
    /// address bar with nine parameters the reader did not choose.
    #[test]
    fn the_default_query_is_empty() {
        assert_eq!(DiscoverQuery::default().to_string(), "");
        assert_eq!(DiscoverQuery::from(""), DiscoverQuery::default());
    }

    /// A tag slug is the one value here that comes from the catalogue rather than from this
    /// build, so it has to survive the separators the grammar reserves — including the comma
    /// this parameter itself is split on.
    ///
    /// The bug this pins: the parser decoded the whole parameter before splitting it, so a slug
    /// whose encoded form contained `%2C` was split at the comma the encoding existed to hide,
    /// and one tag filter silently became two.
    #[test]
    fn tag_slugs_survive_the_separators() {
        let query = DiscoverQuery::with_filters(DiscoverFilters {
            inc: vec!["a,b".to_owned(), "c&d=e".to_owned()],
            ..DiscoverFilters::default()
        });
        assert_eq!(
            DiscoverQuery::from(query.to_string().as_str()).filters.inc,
            query.filters.inc
        );
    }

    /// An unrecognised token is dropped, not defaulted. Substituting a real value would answer
    /// a filter the reader never set while looking like it worked.
    #[test]
    fn unknown_tokens_are_dropped_not_defaulted() {
        let parsed = DiscoverQuery::from("type=manga,nonsense&status=nonsense");
        assert_eq!(parsed.filters.types, vec![ContentType::Manga]);
        assert!(parsed.filters.statuses.is_empty());
    }

    /// A hand-edited URL must not be able to state a window that matches nothing, or an
    /// off-by-one in someone's typing reads as an empty catalogue.
    #[test]
    fn a_crossed_year_window_is_repaired_not_honoured() {
        let parsed = DiscoverQuery::from("from=2020&to=1990");
        assert_eq!(parsed.filters.year_min, 1990);
        assert_eq!(parsed.filters.year_max, 2020);
    }

    /// Out-of-range numbers are clamped rather than refused: the sliders cannot express them,
    /// so honouring one would strand the panel showing a value it can't get back to.
    #[test]
    fn out_of_range_numbers_are_clamped_to_the_controls() {
        let parsed = DiscoverQuery::from("from=1200&to=9999&min=99999");
        assert_eq!(parsed.filters.year_min, YEAR_MIN);
        assert_eq!(parsed.filters.year_max, YEAR_MAX);
        assert_eq!(parsed.filters.min_chapters, MIN_CHAPTERS_MAX);
    }

    /// Changing a filter has to drop the anchor. A narrowed result set has nothing at the old
    /// index, so carrying it over opens the grid past the end of its own results.
    #[test]
    fn changing_a_filter_drops_the_anchor() {
        let deep = DiscoverQuery {
            filters: DiscoverFilters::default(),
            at: 500,
        };
        let mut filters = deep.filters.clone();
        filters.toggle_type(ContentType::Manga);
        assert_eq!(DiscoverQuery::with_filters(filters).at, 0);
    }

    /// The chip's three states have to cycle in one direction and end where they started, or a
    /// tag can be left in a state the reader cannot click their way out of.
    #[test]
    fn a_tag_chip_cycles_neutral_include_exclude_neutral() {
        let mut filters = DiscoverFilters::default();
        filters.cycle_tag("action");
        assert_eq!(filters.inc, vec!["action".to_owned()]);
        filters.cycle_tag("action");
        assert!(filters.inc.is_empty());
        assert_eq!(filters.exc, vec!["action".to_owned()]);
        filters.cycle_tag("action");
        assert!(filters.inc.is_empty() && filters.exc.is_empty());
    }
}
