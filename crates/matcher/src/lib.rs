//! # tankovault-matcher
//!
//! Series canonicalisation scoring (design §10, steps 3–4). Pure and DB-free: the `db`
//! layer supplies trigram [`Candidate`]s, this crate scores them and returns a
//! [`Decision`]. Automation is aggressive where safe and human-reviewed where ambiguous.
//!
//! Score = trigram similarity, boosted by content-type agreement and release-year
//! proximity, capped at 1.0. Three bands:
//! - `>= high` → [`Decision::Attach`] (attach the new source to the existing series),
//! - `[low, high)` → [`Decision::Ambiguous`] (create the source, flag for operator review),
//! - `< low` → [`Decision::Create`] (a new canonical series).

use tankovault_domain::{ContentType, SeriesId};

/// A candidate existing series to match against (from `db::repo::matching::find_candidates`).
#[derive(Debug, Clone)]
pub struct Candidate {
    pub series_id: SeriesId,
    pub normalized_title: String,
    /// Raw trigram similarity in `[0,1]`.
    pub similarity: f32,
    pub content_type: ContentType,
    pub release_year: Option<i32>,
}

/// The incoming source's identifying attributes.
#[derive(Debug, Clone)]
pub struct Query {
    pub normalized_title: String,
    pub content_type: ContentType,
    pub release_year: Option<i32>,
}

/// Confidence thresholds for the decision bands.
#[derive(Debug, Clone, Copy)]
pub struct Thresholds {
    pub high: f32,
    pub low: f32,
}

impl Default for Thresholds {
    fn default() -> Self {
        Self {
            high: 0.85,
            low: 0.6,
        }
    }
}

/// The matching outcome.
#[derive(Debug, Clone, PartialEq)]
pub enum Decision {
    /// High confidence: attach the new source to this existing series.
    Attach(SeriesId),
    /// Ambiguous: create the source but flag a merge candidate for operator review.
    Ambiguous { candidate: SeriesId, score: f32 },
    /// Low/no confidence: create a new canonical series.
    Create,
}

/// Score a single candidate against the query, capped at 1.0.
#[must_use]
pub fn score(query: &Query, candidate: &Candidate) -> f32 {
    let mut s = candidate.similarity;

    // Content-type agreement is a strong signal (a manhwa vs. manga split matters).
    if query.content_type != ContentType::Unknown && candidate.content_type != ContentType::Unknown
    {
        if query.content_type == candidate.content_type {
            s += 0.08;
        } else {
            s -= 0.15;
        }
    }

    // Release-year proximity.
    if let (Some(a), Some(b)) = (query.release_year, candidate.release_year) {
        match (a - b).abs() {
            0 => s += 0.06,
            1 => s += 0.03,
            d if d >= 3 => s -= 0.05,
            _ => {}
        }
    }

    // Exact normalized-title equality is decisive.
    if query.normalized_title == candidate.normalized_title {
        s += 0.1;
    }

    s.clamp(0.0, 1.0)
}

/// Decide how to canonicalise the query given its candidates and thresholds.
#[must_use]
pub fn decide(query: &Query, candidates: &[Candidate], thresholds: Thresholds) -> Decision {
    let best = candidates
        .iter()
        .map(|c| (c.series_id, score(query, c)))
        .max_by(|a, b| a.1.total_cmp(&b.1));

    match best {
        Some((id, s)) if s >= thresholds.high => Decision::Attach(id),
        Some((id, s)) if s >= thresholds.low => Decision::Ambiguous {
            candidate: id,
            score: s,
        },
        _ => Decision::Create,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cand(sim: f32, ct: ContentType, year: Option<i32>, title: &str) -> Candidate {
        Candidate {
            series_id: SeriesId::new(),
            normalized_title: title.to_owned(),
            similarity: sim,
            content_type: ct,
            release_year: year,
        }
    }

    fn query(title: &str, ct: ContentType, year: Option<i32>) -> Query {
        Query {
            normalized_title: title.to_owned(),
            content_type: ct,
            release_year: year,
        }
    }

    #[test]
    fn strong_match_attaches() {
        let q = query("solo leveling", ContentType::Manhwa, Some(2018));
        let c = cand(0.82, ContentType::Manhwa, Some(2018), "solo leveling");
        let id = c.series_id;
        assert_eq!(
            decide(&q, &[c], Thresholds::default()),
            Decision::Attach(id)
        );
    }

    #[test]
    fn ambiguous_band_flags_for_review() {
        let q = query("the beginning after the end", ContentType::Unknown, None);
        let c = cand(0.65, ContentType::Unknown, None, "beginning after end");
        match decide(&q, &[c], Thresholds::default()) {
            Decision::Ambiguous { score, .. } => assert!((0.6..0.85).contains(&score)),
            other => panic!("expected ambiguous, got {other:?}"),
        }
    }

    #[test]
    fn weak_match_creates_new_series() {
        let q = query("some obscure title", ContentType::Manga, Some(2024));
        let c = cand(0.2, ContentType::Manhua, Some(2010), "unrelated work");
        assert_eq!(decide(&q, &[c], Thresholds::default()), Decision::Create);
    }

    #[test]
    fn content_type_disagreement_penalised() {
        let q = query("title", ContentType::Manga, None);
        let same = cand(0.8, ContentType::Manga, None, "title");
        let diff = cand(0.8, ContentType::Manhwa, None, "title");
        assert!(score(&q, &same) > score(&q, &diff));
    }

    #[test]
    fn empty_candidates_creates() {
        let q = query("x", ContentType::Unknown, None);
        assert_eq!(decide(&q, &[], Thresholds::default()), Decision::Create);
    }
}
