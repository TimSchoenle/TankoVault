//! What a series *is*, as a bag of weighted features.
//!
//! The vocabulary decides recommendation quality more than the algorithm does. Terms like
//! "Action" and "Fantasy" each cover a large fraction of the catalogue, so every kind below adds
//! an axis they cannot.

use sha2::{Digest as _, Sha256};
use std::fmt;
use tankovault_domain::{ContentType, SeriesStatus};

/// The axis a feature lives on.
///
/// Part of a feature's identity: the tag "Action" and an author of the same name are different
/// features.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FeatureKind {
    /// A genre or theme slug from `series_tags`, carrying its own per-link weight.
    Tag,
    /// A credited creator. The one kind excluded from the dense embedding.
    Author,
    /// Manga, manhwa or manhua. Never emitted for [`ContentType::Unknown`].
    ContentType,
    /// Country of origin, lower-cased.
    Country,
    /// Publication state. Never emitted for [`SeriesStatus::Unknown`].
    Status,
    /// The decade the series started in, as `1990s`. Absent for an unknown or out-of-range year.
    Decade,
    /// A chapter-count bucket from [`length_bucket`]. Absent for a series with no chapters:
    /// that series is unscanned, not short.
    Length,
    /// What the work was adapted from: `AniList`'s `source`, such as `original` or `light_novel`.
    Source,
}

impl FeatureKind {
    /// Returns the token stored in `rec_features.kind`, one of the values the
    /// `rec_features_kind_check` constraint allows.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Tag => "tag",
            Self::Author => "author",
            Self::ContentType => "content_type",
            Self::Country => "country",
            Self::Status => "status",
            Self::Decade => "decade",
            Self::Length => "length",
            Self::Source => "source",
        }
    }

    /// Returns whether this kind may shape the dense embedding.
    ///
    /// Authors may not. Hundreds of thousands of authors at a document frequency of two or three
    /// would inflate the projection's input dimension by orders of magnitude, and a low-rank
    /// approximation cannot represent a feature that appears three times. Authors are retrieved
    /// exactly instead, which is cheaper and keeps a shared author, close to a certain
    /// recommendation, unblurred.
    #[must_use]
    pub const fn is_dense_eligible(self) -> bool {
        !matches!(self, Self::Author)
    }

    /// Returns the base term weight before idf.
    ///
    /// The values are an ordering, not tuned numbers: a tag says more than a status, and an author
    /// says most. The idf multiplier does the discriminating.
    #[must_use]
    pub const fn base_weight(self) -> f32 {
        match self {
            // A tag's strength is carried per link (`series_tags.weight`, `AniList`'s rank/100),
            // and an author is always wholly present.
            Self::Tag | Self::Author => 1.0,
            Self::ContentType => 0.6,
            Self::Country | Self::Length | Self::Source => 0.4,
            Self::Status | Self::Decade => 0.25,
        }
    }
}

impl fmt::Display for FeatureKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One feature: an axis and a value on it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FeatureKey {
    /// The axis, which is what keeps the tag `action` and the author `action` apart.
    pub kind: FeatureKind,
    /// The value on that axis, normalised by whoever built the key. Slugs and country codes are
    /// lower-case, and the vocabulary is compared byte for byte.
    pub value: String,
}

impl FeatureKey {
    /// Pairs `kind` with `value` verbatim, without normalising it.
    #[must_use]
    pub fn new(kind: FeatureKind, value: impl Into<String>) -> Self {
        Self {
            kind,
            value: value.into(),
        }
    }
}

/// Everything about a series that shapes its features.
///
/// The full build and a single-series repair both fill one, so extraction has one code path.
#[derive(Debug, Clone, Default)]
pub struct SeriesFacts {
    /// [`ContentType::Unknown`] emits no feature.
    pub content_type: ContentType,
    /// [`SeriesStatus::Unknown`] emits no feature.
    pub status: SeriesStatus,
    /// Year of first publication. Feeds the decade bucket, which [`decade_of`] drops outside
    /// 1900 to 2200.
    pub release_year: Option<i32>,
    /// Distinct whole chapters across every source. Feeds the length bucket only.
    pub chapter_count: i64,
    /// `(slug, link weight)`, where the link weight is `series_tags.weight`, `AniList`'s rank/100.
    pub tags: Vec<(String, f32)>,
    /// Author slugs. Every one carries the same weight, since a credit is not partial.
    pub authors: Vec<String>,
    /// Country of origin, lower-cased before it becomes a feature.
    pub country: Option<String>,
    /// `AniList`'s `source`, such as `original` or `light_novel`, lower-cased before use.
    pub source: Option<String>,
}

/// Returns how long a series is, as a reader would describe it: `oneshot` below 10 chapters,
/// then `short`, `medium` from 50, `long` from 200 and `epic` from 600.
///
/// Readers avoid thousand-chapter commitments or seek them out, and nobody tells 340 chapters
/// from 360. A raw count would be a near-unique feature, useless as a shared one.
///
/// ```
/// use tankovault_recsys::length_bucket;
///
/// assert_eq!(length_bucket(9), "oneshot");
/// assert_eq!(length_bucket(210), "long");
/// ```
#[must_use]
pub fn length_bucket(chapters: i64) -> &'static str {
    match chapters {
        ..=9 => "oneshot",
        10..=49 => "short",
        50..=199 => "medium",
        200..=599 => "long",
        _ => "epic",
    }
}

/// Returns the decade a series started in, as `2010s`, or `None` for an unknown year or one
/// outside 1900 to 2200.
///
/// An unknown year produces no feature. A shared absence of metadata is not evidence two series
/// are alike, and an `unknown` bucket would be the most common feature in the vocabulary.
#[must_use]
pub fn decade_of(year: Option<i32>) -> Option<String> {
    let year = year?;
    (1900..=2200)
        .contains(&year)
        .then(|| format!("{}s", year - year.rem_euclid(10)))
}

/// Extracts a series' weighted feature bag.
///
/// The weights are term weights. [`crate::weighting::apply_idf`] applies idf later, once the
/// whole vocabulary has been counted.
///
/// The output is sorted and deduplicated by key, so the same facts always produce the same
/// vector and the same [`digest`]. A duplicated key keeps its strongest weight.
#[must_use]
pub fn extract(facts: &SeriesFacts) -> Vec<(FeatureKey, f32)> {
    let mut out: Vec<(FeatureKey, f32)> =
        Vec::with_capacity(facts.tags.len() + facts.authors.len() + 5);

    for (slug, link_weight) in &facts.tags {
        if slug.is_empty() {
            continue;
        }
        // Clamped: the column has a CHECK, but a negative weight from any other caller would flip
        // the feature's sign.
        let weight = link_weight.clamp(0.0, 1.0) * FeatureKind::Tag.base_weight();
        if weight > 0.0 {
            out.push((FeatureKey::new(FeatureKind::Tag, slug.clone()), weight));
        }
    }

    for slug in &facts.authors {
        if slug.is_empty() {
            continue;
        }
        out.push((
            FeatureKey::new(FeatureKind::Author, slug.clone()),
            FeatureKind::Author.base_weight(),
        ));
    }

    if facts.content_type != ContentType::Unknown {
        out.push((
            FeatureKey::new(FeatureKind::ContentType, facts.content_type.as_str()),
            FeatureKind::ContentType.base_weight(),
        ));
    }
    if facts.status != SeriesStatus::Unknown {
        out.push((
            FeatureKey::new(FeatureKind::Status, facts.status.as_str()),
            FeatureKind::Status.base_weight(),
        ));
    }
    if let Some(country) = facts.country.as_ref().filter(|c| !c.is_empty()) {
        out.push((
            FeatureKey::new(FeatureKind::Country, country.to_lowercase()),
            FeatureKind::Country.base_weight(),
        ));
    }
    if let Some(source) = facts.source.as_ref().filter(|s| !s.is_empty()) {
        out.push((
            FeatureKey::new(FeatureKind::Source, source.to_lowercase()),
            FeatureKind::Source.base_weight(),
        ));
    }
    if let Some(decade) = decade_of(facts.release_year) {
        out.push((
            FeatureKey::new(FeatureKind::Decade, decade),
            FeatureKind::Decade.base_weight(),
        ));
    }
    // A series with no chapters is unscanned, not a oneshot. The bucket would make every
    // unscanned series resemble every other one.
    if facts.chapter_count > 0 {
        out.push((
            FeatureKey::new(FeatureKind::Length, length_bucket(facts.chapter_count)),
            FeatureKind::Length.base_weight(),
        ));
    }

    out.sort_by(|a, b| a.0.cmp(&b.0));
    // Providers emit the same tag twice, and summing would let a duplicate outweigh a stronger
    // single tag.
    out.dedup_by(|b, a| {
        if a.0 == b.0 {
            a.1 = a.1.max(b.1);
            true
        } else {
            false
        }
    });
    out
}

/// Returns a stable SHA-256 of the extracted features, so an incremental build skips unchanged
/// series.
///
/// It hashes the extracted bag, not the raw facts, because the enrichment sweep touches
/// `series.updated_at` far more often than it changes a feature. Weights are quantised to four
/// decimal places first, so a change in the last bit of a float does not count.
#[must_use]
pub fn digest(features: &[(FeatureKey, f32)]) -> Vec<u8> {
    let mut hasher = Sha256::new();
    for (key, weight) in features {
        hasher.update(key.kind.as_str().as_bytes());
        hasher.update([0x1f]);
        hasher.update(key.value.as_bytes());
        hasher.update([0x1f]);
        // Saturating, so a weight outside `[0, 1]` from a buggy caller still digests stably.
        #[expect(
            clippy::cast_possible_truncation,
            reason = "weights are clamped to [0,1] at extraction, so the product is within i32"
        )]
        let quantised = (weight * 10_000.0)
            .round()
            .clamp(f32::from(i16::MIN), f32::from(i16::MAX)) as i32;
        hasher.update(quantised.to_le_bytes());
        hasher.update([0x1e]);
    }
    hasher.finalize().to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts() -> SeriesFacts {
        SeriesFacts {
            content_type: ContentType::Manhwa,
            status: SeriesStatus::Ongoing,
            release_year: Some(2018),
            chapter_count: 210,
            tags: vec![("action".to_owned(), 1.0), ("regression".to_owned(), 0.87)],
            authors: vec!["chugong".to_owned()],
            country: Some("KR".to_owned()),
            source: Some("web_novel".to_owned()),
        }
    }

    #[test]
    fn every_axis_produces_a_feature() {
        let features = extract(&facts());
        let kinds: std::collections::BTreeSet<FeatureKind> =
            features.iter().map(|(k, _)| k.kind).collect();
        assert_eq!(
            kinds,
            [
                FeatureKind::Tag,
                FeatureKind::Author,
                FeatureKind::ContentType,
                FeatureKind::Country,
                FeatureKind::Status,
                FeatureKind::Decade,
                FeatureKind::Length,
                FeatureKind::Source,
            ]
            .into_iter()
            .collect()
        );
    }

    /// Unknown metadata produces no feature, never an "unknown" one.
    #[test]
    fn absent_metadata_produces_no_feature_rather_than_an_unknown_one() {
        let bare = SeriesFacts {
            content_type: ContentType::Unknown,
            status: SeriesStatus::Unknown,
            release_year: None,
            chapter_count: 0,
            tags: Vec::new(),
            authors: Vec::new(),
            country: None,
            source: None,
        };
        assert_eq!(extract(&bare), Vec::<(FeatureKey, f32)>::new());
        assert_eq!(decade_of(None), None);
        // A year of 0 is a parse artefact, so it is unknown too.
        assert_eq!(decade_of(Some(0)), None);
    }

    #[test]
    fn decades_floor_to_ten_years() {
        assert_eq!(decade_of(Some(2018)).as_deref(), Some("2010s"));
        assert_eq!(decade_of(Some(2010)).as_deref(), Some("2010s"));
        assert_eq!(decade_of(Some(1999)).as_deref(), Some("1990s"));
    }

    #[test]
    fn length_buckets_cover_every_count() {
        assert_eq!(length_bucket(0), "oneshot");
        assert_eq!(length_bucket(9), "oneshot");
        assert_eq!(length_bucket(10), "short");
        assert_eq!(length_bucket(49), "short");
        assert_eq!(length_bucket(50), "medium");
        assert_eq!(length_bucket(199), "medium");
        assert_eq!(length_bucket(200), "long");
        assert_eq!(length_bucket(599), "long");
        assert_eq!(length_bucket(600), "epic");
        assert_eq!(length_bucket(10_000), "epic");
    }

    /// A duplicated tag keeps its strongest weight and is not summed.
    #[test]
    fn a_duplicated_feature_keeps_its_strongest_weight_and_is_not_summed() {
        let mut f = facts();
        f.tags = vec![
            ("action".to_owned(), 0.4),
            ("action".to_owned(), 0.9),
            ("action".to_owned(), 0.2),
        ];
        let features = extract(&f);
        let action: Vec<f32> = features
            .iter()
            .filter(|(k, _)| k.kind == FeatureKind::Tag && k.value == "action")
            .map(|(_, w)| *w)
            .collect();
        assert_eq!(
            action.len(),
            1,
            "the duplicate must collapse to one feature"
        );
        assert!(
            (action[0] - 0.9 * FeatureKind::Tag.base_weight()).abs() < 1e-6,
            "the strongest weight must win, got {}",
            action[0]
        );
    }

    #[test]
    fn extraction_is_ordered_and_therefore_the_digest_is_stable() {
        let mut shuffled = facts();
        shuffled.tags.reverse();
        shuffled.authors.push(String::new()); // empty names are dropped, not stored
        assert_eq!(extract(&facts()), extract(&shuffled));
        assert_eq!(digest(&extract(&facts())), digest(&extract(&shuffled)));
    }

    /// The digest moves when a feature or its weight does.
    #[test]
    fn the_digest_moves_when_a_feature_does() {
        let base = digest(&extract(&facts()));
        let mut changed = facts();
        changed.tags.push(("isekai".to_owned(), 0.5));
        assert_ne!(base, digest(&extract(&changed)));

        let mut reweighted = facts();
        reweighted.tags[0].1 = 0.5;
        assert_ne!(base, digest(&extract(&reweighted)));
    }

    /// Only authors stay out of the dense space.
    #[test]
    fn only_authors_are_excluded_from_the_dense_space() {
        assert!(!FeatureKind::Author.is_dense_eligible());
        for kind in [
            FeatureKind::Tag,
            FeatureKind::ContentType,
            FeatureKind::Country,
            FeatureKind::Status,
            FeatureKind::Decade,
            FeatureKind::Length,
            FeatureKind::Source,
        ] {
            assert!(kind.is_dense_eligible(), "{kind} must shape the embedding");
        }
    }
}
