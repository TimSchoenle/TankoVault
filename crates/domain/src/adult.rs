//! Ingest-time adult classification from a provider's own genre chips.
//!
//! The authoritative classifier is `AniList`'s `isAdult`, applied by the enrichment sweep. This is
//! the fallback for everything the sweep has not matched — which is most of a freshly-scanned
//! catalogue, and permanently so for any series `AniList` does not carry. Without it
//! `series.is_adult` sits at its `false` default and an unmatched adult series reads as safe.
//!
//! The two never share a column: see migration 0040 for why an `AniList` refresh must not be able
//! to clear a verdict this made, and why this may only ever answer "yes".

use crate::term_filter::slugify;
use std::collections::HashSet;

/// Genre chips that classify a work as adult, as slugs.
///
/// **Every term here is one that means explicit sexual content and nothing else.** The list is
/// short on purpose: this gate hides series from readers, and a term that merely *co-occurs*
/// with adult work buys a little safety by making the catalogue wrong for everyone.
///
/// Deliberately excluded, and none of these is an oversight:
///
/// - `ecchi` — suggestive, not explicit; it is the standard chip on mainstream shounen comedy.
/// - `yaoi`, `yuri`, `bl`, `gl` — these name whose relationship a work is about, not how
///   explicitly it is depicted. Flagging them adult would hide an entire genre of non-explicit
///   work behind an age gate on the basis of its characters' gender, which is both wrong about
///   the content and not a distinction this system should be drawing.
/// - `mature`, `seinen`, `josei` — audience and tone, routinely applied for violence.
/// - `doujinshi` — self-published, which says nothing about the content.
///
/// If a deployment's providers disagree, that is what the operator's own additions are for.
pub const DEFAULT_ADULT_TAGS: &[&str] = &[
    "adult",
    "hentai",
    "smut",
    "erotica",
    "erotic",
    "pornographic",
    "porn",
    "nsfw",
    "18",
    "18-plus",
    "r18",
    "r-18",
];

/// The terms whose presence on a scanned series marks it adult.
///
/// Never empty: unlike [`crate::TermBlocklist`], there is no configuration that switches this
/// off. An operator who wants adult series visible turns on the deployment flag or leaves the
/// reader's opt-in available — both of which are decisions recorded somewhere a person can
/// audit, where an emptied classifier would just silently stop classifying.
#[derive(Debug, Clone)]
pub struct AdultTagSet {
    slugs: HashSet<String>,
}

impl Default for AdultTagSet {
    fn default() -> Self {
        Self::defaults()
    }
}

impl AdultTagSet {
    /// The shipped terms plus an operator's additions.
    ///
    /// Additions only — the shipped set is not removable, per the type's own documentation.
    /// Terms are slugified, so an operator may write `R-18`, `r18` or `R 18` and mean one thing.
    ///
    /// ```
    /// use tankovault_domain::AdultTagSet;
    ///
    /// let tags = AdultTagSet::with_extra(["Ero Guro"]);
    /// assert!(tags.classifies("Hentai"));
    /// assert!(tags.classifies("  ADULT  "));
    /// assert!(tags.classifies("ero guro"));
    ///
    /// // Substring matching would catch this one; exact slug matching must not.
    /// assert!(!tags.classifies("Young Adult"));
    /// assert!(!tags.classifies("Ecchi"));
    /// ```
    #[must_use]
    pub fn with_extra<I, S>(extra: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        Self {
            slugs: DEFAULT_ADULT_TAGS
                .iter()
                .map(|term| (*term).to_owned())
                .chain(extra.into_iter().map(|term| slugify(term.as_ref())))
                .filter(|slug| !slug.is_empty())
                .collect(),
        }
    }

    /// The shipped terms alone.
    #[must_use]
    pub fn defaults() -> Self {
        Self::with_extra(Vec::<String>::new())
    }

    /// Whether this one genre name classifies its series as adult.
    ///
    /// Matches the whole slug, never a substring. `Young Adult` is a real and common chip and
    /// contains `adult`; `Adventure` contains no term here but the next term added might make
    /// it contain one. A substring rule cannot be reasoned about as the term list grows.
    #[must_use]
    pub fn classifies(&self, name: &str) -> bool {
        self.slugs.contains(&slugify(name))
    }

    /// Whether any of a scanned series' genres classifies it as adult.
    #[must_use]
    pub fn classifies_any<I, S>(&self, names: I) -> bool
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        names.into_iter().any(|name| self.classifies(name.as_ref()))
    }
}

#[cfg(test)]
mod tests {
    use super::{AdultTagSet, DEFAULT_ADULT_TAGS};
    use crate::term_filter::slugify;

    #[test]
    fn shipped_terms_are_already_slugs() {
        for term in DEFAULT_ADULT_TAGS {
            assert_eq!(&slugify(term), term, "{term} is not its own slug");
        }
    }

    /// The genres that must never be treated as adult.
    ///
    /// The bug this pins: an obvious implementation matches substrings, which flags `Young
    /// Adult`; an over-eager term list flags `Ecchi` or `Yaoi`. The first hides a large slice
    /// of an ordinary catalogue, and the second hides one genre's worth of non-explicit work
    /// behind an age gate on the basis of its characters' gender. Both read as "the gate
    /// works" from the operator's side, because nothing errors.
    #[test]
    fn ordinary_genres_are_not_adult() {
        let tags = AdultTagSet::defaults();
        for genre in [
            "Young Adult",
            "Adventure",
            "Ecchi",
            "Yaoi",
            "Yuri",
            "Boys Love",
            "Mature",
            "Seinen",
            "Josei",
            "Doujinshi",
            "Romance",
            "Drama",
        ] {
            assert!(!tags.classifies(genre), "{genre} must not be adult");
        }
    }

    #[test]
    fn explicit_genres_are_adult_however_they_are_written() {
        let tags = AdultTagSet::defaults();
        for genre in [
            "Hentai", "hentai", "  Smut  ", "ADULT", "R-18", "r18", "18+",
        ] {
            assert!(tags.classifies(genre), "{genre} must be adult");
        }
    }

    #[test]
    fn one_adult_genre_among_many_classifies_the_series() {
        let tags = AdultTagSet::defaults();
        assert!(tags.classifies_any(["Action", "Romance", "Hentai"]));
        assert!(!tags.classifies_any(["Action", "Romance", "Comedy"]));
        assert!(!tags.classifies_any(Vec::<String>::new()));
    }

    /// An operator's additions must *add*. The natural implementation replaces.
    #[test]
    fn operator_terms_do_not_replace_the_shipped_ones() {
        let tags = AdultTagSet::with_extra(["Ero Guro"]);
        assert!(tags.classifies("Ero Guro"));
        assert!(tags.classifies("Hentai"));
    }
}
