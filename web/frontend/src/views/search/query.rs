//! Search's view state, encoded as a query string — the same contract Discover and the Watchlist
//! keep, for the same reason: a search worth sending to someone is a search with its options on
//! it, and the back button has to mean something after changing one.
//!
//! The options are the browse endpoint's own parameters. Search is that endpoint with a `query`,
//! so anything it can narrow by, this can offer without the screen inventing a vocabulary.

use crate::models::{ContentType, ContentTypeExt, SeriesStatus, SeriesStatusExt};
use crate::util::{decode_component, encode_component};
use crate::views::discover::{Sort, Tracking};
use std::fmt;

/// Everything the search screen asks for.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct SearchQuery {
    /// The term. Empty is the landing state, not a search for nothing — see
    /// [`Self::is_empty`].
    pub(crate) q: String,
    /// One type, not a set: the endpoint takes one, and a multi-select whose extra choices are
    /// silently dropped is worse than a picker that says what it does.
    pub(crate) content_type: Option<ContentType>,
    pub(crate) status: Option<SeriesStatus>,
    pub(crate) tracking: Tracking,
    /// `None` leaves the ordering to the server, which ranks a searched list by relevance. An
    /// explicit choice is the reader overriding that.
    pub(crate) sort: Option<Sort>,
}

impl SearchQuery {
    /// The same options with a different term, which is what the form submits.
    pub(crate) fn with_term(&self, q: String) -> Self {
        Self { q, ..self.clone() }
    }

    /// Whether this is the landing state rather than a search.
    pub(crate) fn is_empty(&self) -> bool {
        self.q.trim().is_empty()
    }

    /// Whether anything beyond the term narrows the results.
    pub(crate) fn has_options(&self) -> bool {
        self.content_type.is_some()
            || self.status.is_some()
            || self.tracking != Tracking::Any
            || self.sort.is_some()
    }
}

/// Parse the query string. An unparseable value falls back to that field's default: a URL is
/// user-editable, and half-understanding one beats a blank screen.
impl From<&str> for SearchQuery {
    fn from(query: &str) -> Self {
        let mut out = Self::default();
        for pair in query.split('&').filter(|s| !s.is_empty()) {
            let (key, raw) = pair.split_once('=').unwrap_or((pair, ""));
            let value = decode_component(raw);
            match key {
                "q" => out.q = value,
                "type" => {
                    out.content_type = <ContentType as ContentTypeExt>::all()
                        .iter()
                        .copied()
                        .find(|t| t.token() == value);
                }
                "status" => {
                    out.status = <SeriesStatus as SeriesStatusExt>::all()
                        .iter()
                        .copied()
                        .find(|s| s.token() == value);
                }
                "tracking" => out.tracking = Tracking::parse_token(&value),
                "sort" => {
                    out.sort = (!value.is_empty()).then(|| Sort::parse(&value));
                }
                _ => {}
            }
        }
        out
    }
}

/// Write only what differs from the default, so the rail's link stays `/search?` and a shared
/// URL names exactly what its sender chose.
impl fmt::Display for SearchQuery {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut parts: Vec<String> = Vec::new();
        if !self.q.is_empty() {
            parts.push(format!("q={}", encode_component(&self.q)));
        }
        if let Some(content_type) = self.content_type {
            parts.push(format!("type={}", content_type.token()));
        }
        if let Some(status) = self.status {
            parts.push(format!("status={}", status.token()));
        }
        if self.tracking != Tracking::Any {
            parts.push(format!("tracking={}", self.tracking.token()));
        }
        if let Some(sort) = self.sort {
            parts.push(format!("sort={}", sort.token()));
        }
        write!(f, "{}", parts.join("&"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every field must round-trip: one that does not is an option that silently resets on
    /// reload, or a shared link that answers a different question than the one its sender asked.
    #[test]
    fn every_field_round_trips_through_the_query_string() {
        let cases = [
            SearchQuery::default(),
            SearchQuery {
                q: "fate/stay night & co = ?".to_owned(),
                content_type: Some(ContentType::Manhwa),
                status: Some(SeriesStatus::Ongoing),
                tracking: Tracking::Untracked,
                sort: Some(Sort::Chapters),
            },
            SearchQuery {
                q: "九番の鐘".to_owned(),
                ..SearchQuery::default()
            },
        ];
        for case in cases {
            let encoded = case.to_string();
            assert_eq!(
                SearchQuery::from(encoded.as_str()),
                case,
                "round trip failed for {encoded:?}"
            );
        }
    }

    /// The rail's link is `/search`, not `/search?q=&type=&status=&sort=`.
    #[test]
    fn the_default_query_is_empty() {
        assert_eq!(SearchQuery::default().to_string(), "");
        assert_eq!(SearchQuery::from(""), SearchQuery::default());
        assert!(SearchQuery::default().is_empty());
        assert!(!SearchQuery::default().has_options());
    }

    /// An unrecognised token is dropped rather than defaulted: substituting `manga` for a type
    /// this build does not know would answer a filter nobody set while looking like it worked.
    #[test]
    fn unknown_tokens_are_dropped_not_defaulted() {
        let parsed = SearchQuery::from("q=blame&type=nonsense&status=nonsense");
        assert_eq!(parsed.q, "blame");
        assert_eq!(parsed.content_type, None);
        assert_eq!(parsed.status, None);
    }

    /// The absent ordering is the server's relevance ranking, and it has to stay absent — an
    /// unset control that sends `sort=updated` would rank an exact title match by whenever its
    /// last scan happened to be.
    #[test]
    fn an_unset_ordering_names_nothing() {
        assert!(!SearchQuery::default().to_string().contains("sort"));
        let chosen = SearchQuery {
            sort: Some(Sort::Title),
            ..SearchQuery::default()
        };
        assert_eq!(chosen.to_string(), "sort=title");
    }

    /// Whitespace is not a search. The landing state has to survive a stray space, or the screen
    /// answers `Results for " "` with an empty grid.
    #[test]
    fn a_blank_term_is_the_landing_state() {
        assert!(SearchQuery::from("q=%20%20").is_empty());
    }
}
