//! The vocabulary guard on intake: which scraped "genres" are not genres.
//!
//! Aggregator templates put a work's genre chips next to its status, its type and the labels of
//! the summary block itself, and an adapter that scrapes the block scrapes all of it. What
//! reaches the catalogue is a tag called `Updating`, one called `Status`, one called `Manga`.
//! They are not merely useless: every one of them is a facet chip in Discover, a term in the
//! recommender's vocabulary, and — because a term shared by half the catalogue looks like strong
//! evidence to a similarity model — a feature that makes unrelated series look alike.
//!
//! This is the same class of defect migration 0025 repaired for `series_titles`, where the same
//! labels had leaked in as alternative titles and blocked five thousand series onto one key.

use std::collections::HashSet;

/// Reduce a tag name to the key it is stored and compared under.
///
/// Lowercase, alphanumerics kept, every other run collapsed to a single dash. Shared with the
/// `tags.slug` writer on purpose: a blocklist that normalised differently from the column would
/// refuse terms nobody stores and admit the ones that are actually there.
#[must_use]
pub fn slugify(name: &str) -> String {
    let mut slug = String::with_capacity(name.len());
    let mut last_was_dash = true; // suppresses a leading dash
    for c in name.to_lowercase().chars() {
        if c.is_alphanumeric() {
            slug.push(c);
            last_was_dash = false;
        } else if !last_was_dash {
            slug.push('-');
            last_was_dash = true;
        }
    }
    slug.trim_end_matches('-').to_owned()
}

/// Terms refused by default, as slugs.
///
/// Three kinds, and nothing else: a scrape template's own field labels (`status`, `genres`,
/// `alternative`), placeholder values a site shows where it has no answer (`updating`,
/// `unknown`, `n-a`), and medium words that describe the format rather than the work
/// (`manga`, `webtoon`) — the same set [`crate::normalize_title`] already strips from titles.
///
/// Deliberately excludes anything arguable. Publication status (`completed`, `ongoing`) is a
/// column, not a tag, but some catalogues do publish it as a browsable facet, so refusing it is
/// an operator's decision rather than a default.
pub const DEFAULT_BLOCKED_TAGS: &[&str] = &[
    // Placeholders.
    "updating",
    "update",
    "updated",
    "unknown",
    "none",
    "null",
    "n-a",
    "na",
    "tbd",
    "coming-soon",
    "no-genre",
    "no-genres",
    // Template labels.
    "status",
    "genre",
    "genres",
    "alternative",
    "alternative-name",
    "alternative-names",
    "author",
    "authors",
    "artist",
    "artists",
    "type",
    "release",
    "released",
    "rating",
    "view",
    "views",
    "summary",
    "description",
    "tags",
    "chapter",
    "chapters",
    // Medium, not genre.
    "manga",
    "manhwa",
    "manhua",
    "webtoon",
    "webtoons",
    "webcomic",
    "comic",
    "comics",
    "raw",
    "scan",
    "scans",
    "scanlation",
];

/// Tag names intake refuses, matched on their slug.
///
/// Empty is a legitimate state — an operator who configures an empty list has switched the guard
/// off — so this deliberately has no non-empty invariant to enforce.
#[derive(Debug, Clone, Default)]
pub struct TagBlocklist {
    slugs: HashSet<String>,
}

impl TagBlocklist {
    /// Build a blocklist from raw operator-written terms.
    ///
    /// Terms are slugified, so an operator may write `N/A`, `n/a` or `n-a` and mean the one
    /// thing. A term that slugifies to nothing is dropped rather than stored as an empty key
    /// that would match every unnameable tag.
    ///
    /// ```
    /// use tankovault_domain::TagBlocklist;
    ///
    /// let list = TagBlocklist::new(["Updating", "N/A"]);
    /// assert!(list.blocks("updating"));
    /// assert!(list.blocks("  UPDATING  "));
    /// assert!(list.blocks("n/a"));
    /// assert!(!list.blocks("Action"));
    ///
    /// // An empty list is the guard switched off, not a list that blocks everything.
    /// assert!(!TagBlocklist::new(Vec::<String>::new()).blocks("updating"));
    /// ```
    #[must_use]
    pub fn new<I, S>(terms: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        Self {
            slugs: terms
                .into_iter()
                .map(|term| slugify(term.as_ref()))
                .filter(|slug| !slug.is_empty())
                .collect(),
        }
    }

    /// The shipped defaults, for callers with no configuration to hand.
    #[must_use]
    pub fn defaults() -> Self {
        Self::new(DEFAULT_BLOCKED_TAGS)
    }

    /// Whether `name` is refused.
    #[must_use]
    pub fn blocks(&self, name: &str) -> bool {
        !self.slugs.is_empty() && self.slugs.contains(&slugify(name))
    }

    /// Whether the guard is switched off.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.slugs.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::{DEFAULT_BLOCKED_TAGS, TagBlocklist, slugify};

    /// Pins the shape of the key both this and the `tags.slug` writer produce. A drift here
    /// would leave the blocklist comparing against strings the catalogue never stores, which
    /// fails open — junk tags keep arriving and nothing reports a fault.
    #[test]
    fn slugs_match_the_column_they_are_compared_against() {
        assert_eq!(slugify("Slice of Life"), "slice-of-life");
        assert_eq!(slugify("  Sci-Fi  "), "sci-fi");
        assert_eq!(slugify("N/A"), "n-a");
        assert_eq!(slugify("!!!"), "");
    }

    /// The defaults must not refuse a real genre. Spot-checked against the terms a catalogue
    /// actually carries, because the cost of a wrong entry is silent: the tag simply stops
    /// existing, and nothing distinguishes that from a provider that never published it.
    #[test]
    fn the_defaults_leave_real_genres_alone() {
        let list = TagBlocklist::defaults();
        for genre in [
            "Action",
            "Romance",
            "Slice of Life",
            "Sci-Fi",
            "Historical",
            "Mystery",
            "Completed",
            "Ongoing",
            "One Shot",
            "Adaptation",
        ] {
            assert!(!list.blocks(genre), "{genre} must survive the defaults");
        }
        for junk in ["Updating", "Status", "Alternative", "Manhwa", "N/A"] {
            assert!(list.blocks(junk), "{junk} must be refused");
        }
    }

    /// Every shipped default must already be in slug form, or it is dead weight that never
    /// matches: `TagBlocklist::new` slugifies its input, so an entry written any other way
    /// would silently normalise to something else at build time and read as configured.
    #[test]
    fn every_shipped_default_is_written_as_its_own_slug() {
        for term in DEFAULT_BLOCKED_TAGS {
            assert_eq!(&slugify(term), term, "`{term}` is not in slug form");
        }
    }
}
