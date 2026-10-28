//! Turns retrieved candidates into a shelf: the retrieval paths are blended, then diversified.

use std::collections::HashMap;
use tankovault_domain::Tunable;

/// Which retrieval path produced a candidate.
///
/// Kept per candidate, because the paths score on incomparable scales and [`blend`] normalises
/// each one within itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Path {
    /// Nearest neighbours of a series the reader already likes.
    Seed,
    /// Nearest neighbours of the reader's centre of gravity.
    Profile,
    /// An exact match on a rare, high-precision feature, such as an author or a long-tail tag.
    Exact,
    /// The catalogue's popularity prior, for cold start and shelf backfill.
    Prior,
}

/// How much each retrieval path carries in the blend, as an operator has it set.
///
/// [`Default`] reads the compiled defaults from [`Tunable`], the registry the console edits.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PathWeights {
    /// `recsys.score.weight.knn`: neighbours of a series the reader already likes.
    pub seed: f32,
    /// `recsys.score.weight.profile`: neighbours of the reader's centre of gravity.
    pub profile: f32,
    /// `recsys.score.weight.prior`: the catalogue's popularity.
    pub prior: f32,
    /// How far the exact-feature path sits above [`Self::seed`].
    ///
    /// An ordering, not a tunable, so it is not in the registry. A shared author is close to a
    /// certain recommendation and the dense space cannot represent it, so it outranks a
    /// neighbour hit at any content weight. As a multiplier, moving `weight.knn` moves both
    /// content paths together.
    pub exact_premium: f32,
}

impl Default for PathWeights {
    fn default() -> Self {
        #[expect(
            clippy::cast_possible_truncation,
            reason = "the score weights range over 0..=10"
        )]
        let at = |tunable: Tunable| tunable.default_value() as f32;
        Self {
            seed: at(Tunable::ScoreWeightKnn),
            profile: at(Tunable::ScoreWeightProfile),
            prior: at(Tunable::ScoreWeightPrior),
            exact_premium: EXACT_PREMIUM,
        }
    }
}

/// The exact path's premium over the seed path, as [`PathWeights::exact_premium`].
pub const EXACT_PREMIUM: f32 = 1.10;

impl PathWeights {
    /// Returns the weight this set assigns to `path`.
    #[must_use]
    pub fn weight(&self, path: Path) -> f32 {
        match path {
            Path::Exact => self.seed * self.exact_premium,
            Path::Seed => self.seed,
            Path::Profile => self.profile,
            Path::Prior => self.prior,
        }
    }
}

/// One candidate as retrieval produced it.
#[derive(Debug, Clone)]
pub struct Candidate<Id> {
    /// The series retrieval turned up.
    pub id: Id,
    /// Which retrieval path produced it. The same series may arrive by several.
    pub path: Path,
    /// The path's own score, on the path's own scale.
    pub score: f32,
    /// The series that produced this candidate, when one did.
    pub because: Option<Id>,
}

/// A candidate after blending.
#[derive(Debug, Clone)]
pub struct Scored<Id> {
    /// The series being recommended.
    pub id: Id,
    /// The sum of this series' rank-normalised contributions, so it is comparable across
    /// candidates where a [`Candidate::score`] is not.
    pub score: f32,
    /// The series that produced it, for the "because you read …" line.
    pub because: Option<Id>,
    /// The strongest path that produced it, for explanation and debugging.
    pub path: Path,
}

/// Blends the retrieval paths into one ranking, best first.
///
/// A cosine over an embedding, a cosine over sparse features, a count of shared authors and a
/// popularity prior are four scales. Added raw, the widest range would win every comparison, so
/// each path's scores are divided by that path's best score before the weights apply.
///
/// A candidate found by several paths sums its contributions, because agreement between paths is
/// stronger evidence than either alone. Its explanation comes from its highest-weighted path, and
/// ties in score break by id.
///
/// ```
/// use tankovault_recsys::{Candidate, Path, PathWeights, blend};
///
/// // A prior scored in the hundreds does not bury a cosine in `[0, 1]`.
/// let ranked = blend(
///     &[
///         Candidate { id: 1, path: Path::Seed, score: 0.9, because: None },
///         Candidate { id: 2, path: Path::Prior, score: 900.0, because: None },
///     ],
///     &PathWeights::default(),
/// );
/// assert_eq!(ranked[0].id, 1);
/// ```
#[must_use]
pub fn blend<Id: Copy + Eq + std::hash::Hash + Ord>(
    candidates: &[Candidate<Id>],
    weights: &PathWeights,
) -> Vec<Scored<Id>> {
    let mut best_of_path: HashMap<Path, f32> = HashMap::new();
    for candidate in candidates {
        let best = best_of_path.entry(candidate.path).or_insert(f32::MIN);
        *best = best.max(candidate.score);
    }

    let mut totals: HashMap<Id, (f32, Option<Id>, Path)> = HashMap::new();
    for candidate in candidates {
        // A path whose best score is zero or negative carries no ranking information; a ceiling
        // of 1 avoids dividing by it.
        let ceiling = best_of_path
            .get(&candidate.path)
            .copied()
            .filter(|value| *value > f32::EPSILON)
            .unwrap_or(1.0);
        let contribution = weights.weight(candidate.path) * (candidate.score / ceiling);

        let entry = totals
            .entry(candidate.id)
            .or_insert((0.0, candidate.because, candidate.path));
        entry.0 += contribution;
        // The strongest path explains. Between two candidates on one path the first one seen
        // keeps it, so only there does retrieval order pick the explanation.
        if weights.weight(candidate.path) > weights.weight(entry.2) {
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
    // Ties break by id, so identical requests get the same shelf.
    scored.sort_by(|a, b| b.score.total_cmp(&a.score).then_with(|| a.id.cmp(&b.id)));
    scored
}

/// Re-ranks for variety by maximal marginal relevance and returns at most `limit` picks.
///
/// A pure score ranking yields a shelf of near-identical series. Each round picks the candidate
/// maximising `lambda * score - (1 - lambda) * closest`, where `closest` is its greatest
/// similarity to anything already picked. `lambda` is clamped to `0..=1`, and at 1 the result is
/// the plain ranking.
///
/// `similarity` is asked only about pairs that are considered, so the caller computes it from
/// what it has without materialising a matrix. It is called `picks × candidates` times.
///
/// ```
/// use tankovault_recsys::{Path, Scored, diversify};
///
/// let pick = |id, score| Scored { id, score, because: None, path: Path::Seed };
/// // 1 and 2 are near-duplicates; 3 is distinct and slightly worse.
/// let ranked = [pick(1, 1.0), pick(2, 0.99), pick(3, 0.9)];
/// let similar = |a: u32, b: u32| if a + b == 3 { 1.0 } else { 0.0 };
/// let ids: Vec<u32> = diversify(&ranked, 2, 0.7, similar).iter().map(|s| s.id).collect();
/// assert_eq!(ids, [1, 3]);
/// ```
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
    // On the request path `limit` comes from a query parameter; bounding it by the input keeps
    // it from sizing anything.
    let limit = limit.min(ranked.len());

    let mut remaining: Vec<&Scored<Id>> = ranked.iter().collect();
    // Parallel to `remaining`: each candidate's greatest similarity to anything picked. Zero
    // while nothing is picked, so the first round is a pure relevance ranking. Recomputing it
    // over every pick each round would cost `picks² × candidates` similarity calls.
    let mut closest: Vec<f32> = vec![0.0; remaining.len()];

    // No `with_capacity`: static analysis flags an allocation sized from a query parameter and
    // cannot see through the clamp above. The vector holds a shelf's worth of items.
    let mut picked: Vec<Scored<Id>> = Vec::new();

    while picked.len() < limit && !remaining.is_empty() {
        let mut best_index = 0;
        let mut best_value = f32::MIN;
        for (index, candidate) in remaining.iter().enumerate() {
            let value = lambda.mul_add(candidate.score, -((1.0 - lambda) * closest[index]));
            if value > best_value {
                best_value = value;
                best_index = index;
            }
        }

        let chosen = remaining.remove(best_index);
        closest.remove(best_index);
        // Folded against the newly picked item only; every earlier pick is already in `closest`.
        for (index, candidate) in remaining.iter().enumerate() {
            closest[index] = closest[index].max(similarity(candidate.id, chosen.id));
        }
        picked.push(chosen.clone());
    }
    picked
}

/// Caps how many picks one seed may explain.
///
/// Seeds are the top of the reader's affinity ordering, so the strongest seed's neighbourhood is
/// also the highest-scoring band of candidates and would otherwise fill the shelf.
///
/// Picks with no seed, from the profile, exact-feature and prior paths, are uncapped, since they
/// carry no "because you read" line. This is not a [`cap_by`] call because `because` describes
/// how the pick was retrieved, which no lookup by id can recover.
#[must_use]
pub fn cap_by_seed<Id: Copy + Eq + std::hash::Hash>(
    ranked: Vec<Scored<Id>>,
    max_per_seed: usize,
) -> Vec<Scored<Id>> {
    let mut picked_from: HashMap<Id, usize> = HashMap::new();
    let mut out = Vec::with_capacity(ranked.len());
    for candidate in ranked {
        let Some(seed) = candidate.because else {
            out.push(candidate);
            continue;
        };
        let count = picked_from.entry(seed).or_insert(0);
        if *count < max_per_seed {
            *count += 1;
            out.push(candidate);
        }
    }
    out
}

/// Caps how many picks may share one attribute.
///
/// Applied after [`diversify`], which discourages similarity in general, to forbid one kind of
/// repetition outright, such as three books by one author. A pick whose `key_of` is `None` is
/// uncapped.
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

    /// A path with a wide natural range does not swamp the others.
    #[test]
    fn a_paths_scale_does_not_decide_the_ranking() {
        let ranked = blend(
            &[
                candidate(1, Path::Seed, 0.9),
                candidate(2, Path::Prior, 900.0),
            ],
            &PathWeights::default(),
        );
        assert_eq!(
            ranked[0].id, 1,
            "the seed match must outrank the popular one despite a 1000x raw score"
        );
    }

    /// A candidate found by several paths outranks one found by the best path alone.
    #[test]
    fn a_candidate_found_by_several_paths_outranks_one_found_by_the_best_alone() {
        let ranked = blend(
            &[
                candidate(1, Path::Exact, 1.0),
                candidate(2, Path::Seed, 1.0),
                candidate(2, Path::Profile, 1.0),
                candidate(2, Path::Exact, 1.0),
            ],
            &PathWeights::default(),
        );
        assert_eq!(ranked[0].id, 2, "three agreeing paths must beat one");
    }

    /// The explanation comes from the strongest path, not from retrieval order.
    #[test]
    fn the_explanation_comes_from_the_strongest_path() {
        let ranked = blend(
            &[
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
            ],
            &PathWeights::default(),
        );
        assert_eq!(ranked[0].because, Some(42));
        assert_eq!(ranked[0].path, Path::Exact);
    }

    /// A path's weight decides which path wins, so the console's weights are not inert.
    #[test]
    fn a_paths_weight_decides_which_path_wins() {
        let candidates = [
            candidate(1, Path::Seed, 1.0),
            candidate(2, Path::Prior, 1.0),
        ];

        let shipped = blend(&candidates, &PathWeights::default());
        assert_eq!(shipped[0].id, 1, "at the defaults the seed hit leads");

        let prior_heavy = blend(
            &candidates,
            &PathWeights {
                seed: 0.1,
                prior: 5.0,
                ..PathWeights::default()
            },
        );
        assert_eq!(
            prior_heavy[0].id, 2,
            "raising the prior weight above the seed weight must reorder the shelf"
        );
    }

    /// The exact path outranks the seed path at every content weight.
    #[test]
    fn the_exact_path_keeps_its_premium_over_the_seed_path() {
        for seed in [0.1_f32, 1.0, 9.9] {
            let weights = PathWeights {
                seed,
                ..PathWeights::default()
            };
            assert!(
                weights.weight(Path::Exact) > weights.weight(Path::Seed),
                "exact must lead seed at weight {seed}"
            );
        }
    }

    /// Identical inputs produce an identical shelf.
    #[test]
    fn ties_break_deterministically() {
        let input = [
            candidate(3, Path::Seed, 0.5),
            candidate(1, Path::Seed, 0.5),
            candidate(2, Path::Seed, 0.5),
        ];
        for _ in 0..10 {
            let ids: Vec<u32> = blend(&input, &PathWeights::default())
                .into_iter()
                .map(|s| s.id)
                .collect();
            assert_eq!(ids, vec![1, 2, 3]);
        }
    }

    #[test]
    fn a_degenerate_path_does_not_divide_by_zero() {
        let ranked = blend(
            &[
                candidate(1, Path::Prior, 0.0),
                candidate(2, Path::Seed, 0.5),
            ],
            &PathWeights::default(),
        );
        assert!(ranked.iter().all(|s| s.score.is_finite()));
        assert_eq!(ranked[0].id, 2);
    }

    /// MMR gives up some relevance for variety.
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
        #[expect(
            clippy::cast_possible_truncation,
            reason = "the shipped lambda is a small ratio"
        )]
        let lambda = Tunable::DiversityLambda.default_value() as f32;
        let picked = diversify(&ranked, 2, lambda, similarity);
        assert_eq!(picked[0].id, 1, "the best pick is still first");
        assert_eq!(
            picked[1].id, 4,
            "the second pick must be the distinct one, not the next-best near-duplicate"
        );
    }

    /// With lambda at 1 there is no diversity term, so MMR returns the plain ranking.
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

    /// The running maximum selects the same shelf as recomputing it each round, from about
    /// `picks × candidates` similarity calls.
    #[test]
    fn the_running_maximum_agrees_with_recomputing_it() {
        let ranked: Vec<Scored<u32>> = (0..40_u32)
            .map(|id| Scored {
                id,
                score: 1.0 - f32::from(u16::try_from(id).expect("ids below 40")) / 100.0,
                because: None,
                path: Path::Seed,
            })
            .collect();
        // Deterministic and symmetric, with enough spread that the fold order would show.
        let sim =
            |a: u32, b: u32| f32::from(u16::try_from((a * 7 + b * 7) % 13).unwrap_or(0)) / 13.0;

        let calls = std::cell::Cell::new(0_u32);
        let counted = |a, b| {
            calls.set(calls.get() + 1);
            sim(a, b)
        };
        // The shipped lambda, from the registry, so the two forms agree at the production setting.
        #[expect(
            clippy::cast_possible_truncation,
            reason = "the shipped lambda is a small ratio"
        )]
        let lambda = Tunable::DiversityLambda.default_value() as f32;
        let picked = diversify(&ranked, 10, lambda, counted);
        let incremental_calls = calls.get();

        // The naive form, written out here so the comparison is against something readable.
        let mut remaining: Vec<&Scored<u32>> = ranked.iter().collect();
        let mut naive: Vec<u32> = Vec::new();
        while naive.len() < 10 && !remaining.is_empty() {
            let (best, _) = remaining
                .iter()
                .enumerate()
                .map(|(index, candidate)| {
                    let closest = naive
                        .iter()
                        .map(|c| sim(candidate.id, *c))
                        .fold(0.0, f32::max);
                    (
                        index,
                        lambda.mul_add(candidate.score, -((1.0 - lambda) * closest)),
                    )
                })
                .fold(
                    (0, f32::MIN),
                    |acc, next| if next.1 > acc.1 { next } else { acc },
                );
            naive.push(remaining.remove(best).id);
        }

        assert_eq!(
            picked.iter().map(|s| s.id).collect::<Vec<_>>(),
            naive,
            "the incremental maximum must select exactly the same shelf"
        );
        assert!(
            incremental_calls < 500,
            "ten picks over forty candidates must stay near picks x candidates, not \
             picks^2 x candidates; made {incremental_calls} calls"
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

    /// `cap_by` limits how many picks share one attribute.
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

    /// One seed explains at most the cap, and seedless picks are uncapped.
    #[test]
    fn cap_by_seed_stops_one_anchor_taking_the_shelf() {
        let seeded = |id: u32, seed: Option<u32>| Scored {
            id,
            score: 1.0,
            because: seed,
            path: Path::Seed,
        };
        let ranked = vec![
            seeded(1, Some(100)),
            seeded(2, Some(100)),
            seeded(3, Some(100)),
            seeded(4, Some(200)),
            seeded(5, None),
            seeded(6, None),
        ];
        let ids: Vec<u32> = cap_by_seed(ranked, 2).iter().map(|s| s.id).collect();
        assert_eq!(ids, vec![1, 2, 4, 5, 6]);
    }
}
