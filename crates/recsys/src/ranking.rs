//! Turning retrieved candidates into a shelf: blending the retrieval paths, then diversifying.

use std::collections::HashMap;

/// Which retrieval path produced a candidate.
///
/// Kept per candidate rather than collapsed at retrieval time because the paths are scored on
/// incomparable scales and each has to be normalised within itself before any of them are added
/// together (see [`blend`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Path {
    /// Nearest neighbours of a series the reader already likes.
    Seed,
    /// Nearest neighbours of the reader's centre of gravity.
    Profile,
    /// An exact match on a rare, high-precision feature — an author, or a long-tail tag.
    Exact,
    /// The catalogue's popularity prior. Cold start and shelf backfill.
    Prior,
}

impl Path {
    /// The weight this path carries in the blend.
    ///
    /// Ordering, not tuning: precision paths above recall paths, and the prior last because it
    /// knows nothing about the reader. `Exact` sits at the top because sharing an author is close
    /// to a certain recommendation and it is exactly what the dense space cannot see.
    #[must_use]
    pub const fn weight(self) -> f32 {
        match self {
            Self::Exact => 1.10,
            Self::Seed => 1.00,
            Self::Profile => 0.70,
            Self::Prior => 0.25,
        }
    }
}

/// One candidate as retrieval produced it.
#[derive(Debug, Clone)]
pub struct Candidate<Id> {
    pub id: Id,
    pub path: Path,
    /// The path's own score, on the path's own scale.
    pub score: f32,
    /// The series that produced this candidate, when one did.
    pub because: Option<Id>,
}

/// A candidate after blending.
#[derive(Debug, Clone)]
pub struct Scored<Id> {
    pub id: Id,
    pub score: f32,
    pub because: Option<Id>,
    /// The strongest path that produced it, for explanation and debugging.
    pub path: Path,
}

/// Blend the retrieval paths into one ranking.
///
/// # Why rank-normalise instead of adding the raw scores
///
/// A cosine over an embedding, a cosine over sparse features, a count of shared authors and a
/// popularity prior are four different scales. Added raw, whichever has the widest natural range
/// wins every comparison, and the weights above become decorative. Normalising each path against
/// *its own* best candidate makes the weights mean what they say.
///
/// A candidate found by several paths keeps the sum of its normalised contributions, which is the
/// point: agreement between an exact author match and the dense space is much stronger evidence
/// than either alone.
#[must_use]
pub fn blend<Id: Copy + Eq + std::hash::Hash + Ord>(
    candidates: &[Candidate<Id>],
) -> Vec<Scored<Id>> {
    let mut best_of_path: HashMap<Path, f32> = HashMap::new();
    for candidate in candidates {
        let best = best_of_path.entry(candidate.path).or_insert(f32::MIN);
        *best = best.max(candidate.score);
    }

    let mut totals: HashMap<Id, (f32, Option<Id>, Path)> = HashMap::new();
    for candidate in candidates {
        // A path whose best score is zero or negative carries no ranking information; treating
        // its ceiling as 1 avoids dividing by it and leaves every contribution at zero.
        let ceiling = best_of_path
            .get(&candidate.path)
            .copied()
            .filter(|value| *value > f32::EPSILON)
            .unwrap_or(1.0);
        let contribution = candidate.path.weight() * (candidate.score / ceiling);

        let entry = totals
            .entry(candidate.id)
            .or_insert((0.0, candidate.because, candidate.path));
        entry.0 += contribution;
        // The explanation comes from the strongest single path, not the last one seen — otherwise
        // it depends on retrieval order, which is not a fact about the recommendation.
        if candidate.path.weight() > entry.2.weight() {
            entry.1 = candidate.because;
            entry.2 = candidate.path;
        }
    }

    let mut scored: Vec<Scored<Id>> = totals
        .into_iter()
        .map(|(id, (score, because, path))| Scored {
            id,
            score,
            because,
            path,
        })
        .collect();
    // Ties break by id so the shelf is stable between identical requests. A ranking that
    // reshuffles on refresh reads as broken even when every pick is defensible.
    scored.sort_by(|a, b| b.score.total_cmp(&a.score).then_with(|| a.id.cmp(&b.id)));
    scored
}

/// How much the ranking is allowed to give up for variety, in `[0, 1]`.
///
/// 1.0 is pure relevance. Lower trades score for distance from what has already been picked.
pub const DIVERSITY_LAMBDA: f32 = 0.7;

/// Re-rank for variety: maximal marginal relevance.
///
/// Twelve near-identical series is the failure mode a pure score ranking produces, and it reads
/// as broken even when every individual pick is defensible — a reader who liked one dungeon
/// manhwa does not want a shelf of nothing else.
///
/// `similarity` is asked only about pairs that are actually considered, so the caller can compute
/// it from whatever it has (here, the sparse vectors) without materialising a full matrix.
#[must_use]
pub fn diversify<Id: Copy + Eq, F>(
    ranked: &[Scored<Id>],
    limit: usize,
    lambda: f32,
    mut similarity: F,
) -> Vec<Scored<Id>>
where
    F: FnMut(Id, Id) -> f32,
{
    let lambda = lambda.clamp(0.0, 1.0);
    let mut remaining: Vec<&Scored<Id>> = ranked.iter().collect();
    let mut picked: Vec<Scored<Id>> = Vec::with_capacity(limit.min(remaining.len()));

    while picked.len() < limit && !remaining.is_empty() {
        let mut best_index = 0;
        let mut best_value = f32::MIN;
        for (index, candidate) in remaining.iter().enumerate() {
            let closest = picked
                .iter()
                .map(|chosen| similarity(candidate.id, chosen.id))
                .fold(0.0_f32, f32::max);
            let value = lambda.mul_add(candidate.score, -((1.0 - lambda) * closest));
            if value > best_value {
                best_value = value;
                best_index = index;
            }
        }
        picked.push(remaining.remove(best_index).clone());
    }
    picked
}

/// Cap how many picks may share one attribute.
///
/// Applied after [`diversify`] rather than instead of it: MMR discourages similarity in general,
/// while this forbids a specific kind of repetition outright. Three books by the same author is a
/// bibliography, not a recommendation.
pub fn cap_by<Id: Copy, K: Eq + std::hash::Hash, F>(
    ranked: Vec<Scored<Id>>,
    max_per_key: usize,
    mut key_of: F,
) -> Vec<Scored<Id>>
where
    F: FnMut(Id) -> Option<K>,
{
    let mut seen: HashMap<K, usize> = HashMap::new();
    let mut out = Vec::with_capacity(ranked.len());
    for candidate in ranked {
        let Some(key) = key_of(candidate.id) else {
            out.push(candidate);
            continue;
        };
        let count = seen.entry(key).or_insert(0);
        if *count < max_per_key {
            *count += 1;
            out.push(candidate);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(id: u32, path: Path, score: f32) -> Candidate<u32> {
        Candidate {
            id,
            path,
            score,
            because: None,
        }
    }

    /// **A path with a wide natural range must not swamp the others.**
    ///
    /// The bug this pins: added raw, a prior scored in the hundreds buries a cosine in `[0, 1]`,
    /// and every weight in `Path::weight` becomes decorative while the shelf silently becomes a
    /// popularity ranking.
    #[test]
    fn a_paths_scale_does_not_decide_the_ranking() {
        let ranked = blend(&[
            candidate(1, Path::Seed, 0.9),
            candidate(2, Path::Prior, 900.0),
        ]);
        assert_eq!(
            ranked[0].id, 1,
            "the seed match must outrank the popular one despite a 1000x raw score"
        );
    }

    /// Agreement between paths is stronger evidence than either alone.
    #[test]
    fn a_candidate_found_by_several_paths_outranks_one_found_by_the_best_alone() {
        let ranked = blend(&[
            candidate(1, Path::Exact, 1.0),
            candidate(2, Path::Seed, 1.0),
            candidate(2, Path::Profile, 1.0),
            candidate(2, Path::Exact, 1.0),
        ]);
        assert_eq!(ranked[0].id, 2, "three agreeing paths must beat one");
    }

    /// The explanation must come from the strongest path, not from retrieval order.
    #[test]
    fn the_explanation_comes_from_the_strongest_path() {
        let ranked = blend(&[
            Candidate {
                id: 7,
                path: Path::Prior,
                score: 1.0,
                because: None,
            },
            Candidate {
                id: 7,
                path: Path::Exact,
                score: 1.0,
                because: Some(42),
            },
            Candidate {
                id: 7,
                path: Path::Profile,
                score: 1.0,
                because: Some(99),
            },
        ]);
        assert_eq!(ranked[0].because, Some(42));
        assert_eq!(ranked[0].path, Path::Exact);
    }

    /// Identical inputs must produce an identical shelf, every time.
    #[test]
    fn ties_break_deterministically() {
        let input = [
            candidate(3, Path::Seed, 0.5),
            candidate(1, Path::Seed, 0.5),
            candidate(2, Path::Seed, 0.5),
        ];
        for _ in 0..10 {
            let ids: Vec<u32> = blend(&input).into_iter().map(|s| s.id).collect();
            assert_eq!(ids, vec![1, 2, 3]);
        }
    }

    #[test]
    fn a_degenerate_path_does_not_divide_by_zero() {
        let ranked = blend(&[
            candidate(1, Path::Prior, 0.0),
            candidate(2, Path::Seed, 0.5),
        ]);
        assert!(ranked.iter().all(|s| s.score.is_finite()));
        assert_eq!(ranked[0].id, 2);
    }

    /// MMR must actually give up some relevance for variety.
    #[test]
    fn diversify_breaks_up_a_run_of_near_identical_picks() {
        // 1, 2, 3 are near-duplicates of each other; 4 is unrelated and slightly worse.
        let ranked = vec![
            Scored {
                id: 1,
                score: 1.00,
                because: None,
                path: Path::Seed,
            },
            Scored {
                id: 2,
                score: 0.99,
                because: None,
                path: Path::Seed,
            },
            Scored {
                id: 3,
                score: 0.98,
                because: None,
                path: Path::Seed,
            },
            Scored {
                id: 4,
                score: 0.90,
                because: None,
                path: Path::Seed,
            },
        ];
        let similarity = |a: u32, b: u32| {
            if [1, 2, 3].contains(&a) && [1, 2, 3].contains(&b) {
                1.0
            } else {
                0.0
            }
        };
        let picked = diversify(&ranked, 2, DIVERSITY_LAMBDA, similarity);
        assert_eq!(picked[0].id, 1, "the best pick is still first");
        assert_eq!(
            picked[1].id, 4,
            "the second pick must be the distinct one, not the next-best near-duplicate"
        );
    }

    /// With lambda at 1 there is no diversity term, so MMR degenerates to the plain ranking —
    /// which is what makes the knob honest at its extreme.
    #[test]
    fn lambda_one_is_pure_relevance() {
        let ranked = vec![
            Scored {
                id: 1,
                score: 1.0,
                because: None,
                path: Path::Seed,
            },
            Scored {
                id: 2,
                score: 0.9,
                because: None,
                path: Path::Seed,
            },
            Scored {
                id: 3,
                score: 0.8,
                because: None,
                path: Path::Seed,
            },
        ];
        let picked = diversify(&ranked, 3, 1.0, |_, _| 1.0);
        assert_eq!(
            picked.iter().map(|s| s.id).collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
    }

    #[test]
    fn diversify_never_returns_more_than_asked_or_invents_candidates() {
        let ranked = vec![Scored {
            id: 1,
            score: 1.0,
            because: None,
            path: Path::Seed,
        }];
        assert_eq!(diversify(&ranked, 10, 0.7, |_, _| 0.0).len(), 1);
        assert!(diversify(&[], 5, 0.7, |_: u32, _: u32| 0.0).is_empty());
    }

    /// Three books by the same author is a bibliography, not a recommendation.
    #[test]
    fn cap_by_limits_repetition_of_one_attribute() {
        let ranked: Vec<Scored<u32>> = (1..=6)
            .map(|id| Scored {
                id,
                score: 1.0,
                because: None,
                path: Path::Seed,
            })
            .collect();
        // 1, 2, 3 share an author; 4, 5, 6 have none.
        let capped = cap_by(ranked, 2, |id| (id <= 3).then_some("miura"));
        let ids: Vec<u32> = capped.iter().map(|s| s.id).collect();
        assert_eq!(
            ids,
            vec![1, 2, 4, 5, 6],
            "only two of the shared-author run survive"
        );
    }
}
