//! What a series *is*, as a bag of weighted features.
//!
//! The vocabulary is the design's biggest quality lever, not the algorithm: a recommender whose
//! only terms are "Action" and "Fantasy" cannot say anything specific, because each covers a
//! large fraction of the catalogue. Every kind below exists to add an axis those two cannot.

use sha2::{Digest as _, Sha256};
use std::fmt;
use tankovault_domain::{ContentType, SeriesStatus};

/// The axis a feature lives on.
///
/// Part of a feature's identity, not decoration: "Action" the tag and an author of the same name
/// are different features, and a single string vocabulary would collide them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FeatureKind {
    Tag,
    Author,
    ContentType,
    Country,
    Status,
    Decade,
    Length,
}

impl FeatureKind {
    /// The token stored in `rec_features.kind`. Mirrors the SQL `CHECK` in migration 0028.
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
        }
    }

    /// Whether this kind may shape the dense embedding.
    ///
    /// Authors may not. With hundreds of thousands of distinct authors at a document frequency
    /// of two or three, they would inflate the projection's input dimension by orders of
    /// magnitude and then be annihilated anyway — a low-rank approximation cannot represent a
    /// feature that appears three times. They are retrieved exactly instead, which is both
    /// cheaper and strictly more faithful: sharing an author is close to a certain
    /// recommendation, and no compression should be allowed to blur it.
    #[must_use]
    pub const fn is_dense_eligible(self) -> bool {
        !matches!(self, Self::Author)
    }

    /// The base term weight before idf.
    ///
    /// These are not tuned numbers and are not meant to look like any: they express an ordering
    /// — a tag says more than a status, an author says most of all — and the idf multiplier does
    /// the discriminating.
    #[must_use]
    pub const fn base_weight(self) -> f32 {
        match self {
            // A tag's strength is carried per link instead (`series_tags.weight`, `AniList`'s
            // rank/100), and an author is always wholly present — the two reach 1.0 by different
            // arguments, so they stay separate arms.
            Self::Tag | Self::Author => 1.0,
            Self::ContentType => 0.6,
            Self::Country | Self::Length => 0.4,
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
    pub kind: FeatureKind,
    pub value: String,
}

impl FeatureKey {
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
/// A plain struct rather than a database row so extraction can be tested — and property-tested —
/// without a database, and so the same extraction serves the full build and a single-series
/// repair without two code paths.
#[derive(Debug, Clone, Default)]
pub struct SeriesFacts {
    pub content_type: ContentType,
    pub status: SeriesStatus,
    pub release_year: Option<i32>,
    /// Distinct whole chapters across every source. Feeds the length bucket only.
    pub chapter_count: i64,
    /// `(slug, link weight)` — the link weight is `series_tags.weight`, `AniList`'s rank/100.
    pub tags: Vec<(String, f32)>,
    pub authors: Vec<String>,
    pub country: Option<String>,
}

/// How long a series is, as a reader would describe it.
///
/// Bucketed rather than continuous because the preference is categorical: readers avoid
/// thousand-chapter commitments or seek them out, and nobody distinguishes 340 chapters from
/// 360. A raw count would also be a near-unique feature and therefore useless as a shared one.
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

/// The decade a series started in, or `None` when the year is unknown.
///
/// Unknown produces no feature at all rather than a `"unknown"` bucket: a shared *absence* of
/// metadata is not evidence two series are alike, and with a large fraction of the catalogue
/// unenriched that bucket would be the single most common feature in the vocabulary.
#[must_use]
pub fn decade_of(year: Option<i32>) -> Option<String> {
    let year = year?;
    (1900..=2200)
        .contains(&year)
        .then(|| format!("{}s", year - year.rem_euclid(10)))
}

/// Extract a series' weighted feature bag.
///
/// Weights here are *term* weights only; idf is applied later, by
/// [`crate::weighting::apply_idf`], because it is a property of the catalogue rather than of the
/// series and is not known until the whole vocabulary has been counted.
///
/// Output is sorted and deduplicated by key, so two runs over the same facts produce
/// byte-identical vectors and the digest is stable.
#[must_use]
pub fn extract(facts: &SeriesFacts) -> Vec<(FeatureKey, f32)> {
    let mut out: Vec<(FeatureKey, f32)> =
        Vec::with_capacity(facts.tags.len() + facts.authors.len() + 5);

    for (slug, link_weight) in &facts.tags {
        if slug.is_empty() {
            continue;
        }
        // Clamped, not trusted: the column has a CHECK, but this function is also fed by tests
        // and by any future provider, and a negative weight would flip the feature's sign.
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
    if let Some(decade) = decade_of(facts.release_year) {
        out.push((
            FeatureKey::new(FeatureKind::Decade, decade),
            FeatureKind::Decade.base_weight(),
        ));
    }
    // A series with no chapters is not "a oneshot"; it is unscanned. Emitting the bucket anyway
    // would make every unscanned series resemble every other one.
    if facts.chapter_count > 0 {
        out.push((
            FeatureKey::new(FeatureKind::Length, length_bucket(facts.chapter_count)),
            FeatureKind::Length.base_weight(),
        ));
    }

    out.sort_by(|a, b| a.0.cmp(&b.0));
    // Keep the strongest of a duplicated key. Providers do emit the same tag twice, and summing
    // would let a duplicate outweigh a genuinely stronger single tag.
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

/// A stable hash of the extracted features, used to skip unchanged series on an incremental
/// build.
///
/// Over the *extracted* bag rather than the raw facts: the enrichment sweep touches
/// `series.updated_at` far more often than it changes anything a feature depends on, and hashing
/// the output is what turns that into no work. Byte-order and separators are explicit so the
/// digest cannot change under a formatting change.
#[must_use]
pub fn digest(features: &[(FeatureKey, f32)]) -> Vec<u8> {
    let mut hasher = Sha256::new();
    for (key, weight) in features {
        hasher.update(key.kind.as_str().as_bytes());
        hasher.update([0x1f]);
        hasher.update(key.value.as_bytes());
        hasher.update([0x1f]);
        // Quantised before hashing: a float that differs only in its last bit is not a change
        // worth re-embedding a series for, and `to_bits` would make it one. Saturating, because
        // a weight outside `[0, 1]` cannot reach this function but a future caller's bug should
        // produce a stable digest rather than an unspecified cast.
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
            ]
            .into_iter()
            .collect()
        );
    }

    /// Unknown metadata must produce *no* feature, never an "unknown" one.
    ///
    /// The bug this pins: with a large fraction of the catalogue unenriched, an `unknown` bucket
    /// becomes the most common feature in the vocabulary, and every unenriched series then
    /// resembles every other unenriched series more than anything it is actually like.
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
        };
        assert!(extract(&bare).is_empty());
        assert_eq!(decade_of(None), None);
        // Out of range is as unknown as absent: a year of 0 is a parse artefact, not a decade.
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

    /// A duplicated tag must not outweigh a stronger single one.
    ///
    /// Providers do emit the same tag twice. Summing the weights would make "Action, Action"
    /// score above a genuinely stronger "Regression", which is the opposite of what the ranks
    /// mean.
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

    /// The digest must move when a feature does, or an incremental build never re-embeds
    /// anything.
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

    /// Authors are excluded from the dense space on purpose (see [`FeatureKind::is_dense_eligible`]).
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
        ] {
            assert!(kind.is_dense_eligible(), "{kind} must shape the embedding");
        }
    }
}
