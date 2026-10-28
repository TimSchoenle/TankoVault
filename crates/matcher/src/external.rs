//! Whether an external catalogue record describes a local series, judged on titles alone.

use tankovault_domain::matching::{Candidate, Query};
use tankovault_domain::{ContentType, SeriesId, normalize_title};

use crate::assess::assess;

/// Returns whether any of `remote_titles` is a primary identity for the local series: equal to its
/// normalized canonical title or one of its sources' listing titles, exactly or compacted.
///
/// A provider's title search returns its best-ranked record, not a matching one, so its answer
/// is a claim to check before metadata or a mapping is written from it. Alternative titles do
/// not count: a synonym shared across a franchise is exactly how a search lands on a sibling.
///
/// ```
/// use tankovault_matcher::titles_identify;
///
/// let listings = ["na honjaman level up".to_owned()];
/// assert!(titles_identify("solo leveling", &listings, &["Na Honjaman Level Up".to_owned()]));
/// assert!(!titles_identify("solo leveling", &listings, &["Solo Leveling: Ragnarok".to_owned()]));
/// ```
#[must_use]
pub fn titles_identify(
    normalized_title: &str,
    listing_titles: &[String],
    remote_titles: &[String],
) -> bool {
    let local = Candidate {
        // Not read by `assess`; the record is judged against titles, not a stored row.
        series_id: SeriesId::default(),
        normalized_title: normalized_title.to_owned(),
        similarity: 0.0,
        alt_normalized_titles: Vec::new(),
        source_titles: listing_titles.to_vec(),
        content_type: ContentType::Unknown,
        release_year: None,
        tags: Vec::new(),
        authors: Vec::new(),
    };
    remote_titles.iter().any(|title| {
        let normalized = normalize_title(title);
        !normalized.is_empty()
            && assess(
                &Query {
                    normalized_title: normalized,
                    content_type: ContentType::Unknown,
                    release_year: None,
                    tags: Vec::new(),
                    authors: Vec::new(),
                },
                &local,
            )
            .signals
            .is_primary_identity()
    })
}

#[cfg(test)]
mod tests {
    use super::titles_identify;

    fn owned(titles: &[&str]) -> Vec<String> {
        titles.iter().map(|t| (*t).to_owned()).collect()
    }

    #[test]
    fn a_canonical_or_listing_title_identifies() {
        assert!(titles_identify(
            "solo leveling",
            &[],
            &owned(&["Na Honjaman Level Up", "Solo Leveling"]),
        ));
        assert!(titles_identify(
            "only i level up",
            &owned(&["na honjaman level up"]),
            &owned(&["Na Honjaman Level Up"]),
        ));
        assert!(titles_identify(
            "onepunch man",
            &[],
            &owned(&["One-Punch Man"])
        ));
    }

    /// A provider's top-ranked record for a similar title is a different work; enriching from
    /// it would overwrite the series' cover, description and status.
    #[test]
    fn a_merely_similar_record_does_not_identify() {
        assert!(!titles_identify(
            "solo leveling ragnarok",
            &[],
            &owned(&["Solo Leveling"]),
        ));
        assert!(!titles_identify("solo leveling", &[], &[]));
    }
}
