//! Turning retrieved candidates into a shelf: blending the retrieval paths, then diversifying.

use std::collections::HashMap;
use tankovault_domain::Tunable;

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

/// How much each retrieval path carries in the blend, as an operator has it set.
///
/// Threaded in rather than read from a global for the same reason [`crate::AffinityParams`] is:
/// [`blend`] is a pure function and has to be testable at settings other than the live one.
///
/// [`Default`] reads the compiled defaults out of [`Tunable`], so the registry the console edits
/// is the only copy of these numbers.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PathWeights {
    /// `recsys.score.weight.knn` — neighbours of a series the reader already likes.
    pub seed: f32,
    /// `recsys.score.weight.profile` — neighbours of the reader's centre of gravity.
    pub profile: f32,
    /// `recsys.score.weight.prior` — the catalogue's popularity.
    pub prior: f32,
    /// How far the exact-feature path sits above [`Self::seed`].
    ///
    /// Ordering, not tuning, and therefore not in the registry: sharing an author is close to a
    /// certain recommendation and it is precisely what the dense space cannot represent, so it
    /// has to outrank a neighbour hit *whatever* the content weight is set to. Expressed as a
    /// multiplier so that moving `weight.knn` moves both content paths together, which is what an
    /// operator reaching for that knob means.
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

/// The exact path's standing premium over the seed path; see [`PathWeights::exact_premium`].
pub const EXACT_PREMIUM: f32 = 1.10;

impl PathWeights {
    /// The weight this set assigns to `path`.
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
    weights: &PathWeights,
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
        let contribution = weights.weight(candidate.path) * (candidate.score / ceiling);

        let entry = totals
            .entry(candidate.id)
            .or_insert((0.0, candidate.because, candidate.path));
        entry.0 += contribution;
        // The explanation comes from the strongest single path, not the last one seen — otherwise
        // it depends on retrieval order, which is not a fact about the recommendation.
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
    // Ties break by id so the shelf is stable between identical requests. A ranking that
    // reshuffles on refresh reads as broken even when every pick is defensible.
    scored.sort_by(|a, b| b.score.total_cmp(&a.score).then_with(|| a.id.cmp(&b.id)));
    scored
}

/// Re-rank for variety: maximal marginal relevance.
///
/// Twelve near-identical series is the failure mode a pure score ranking produces, and it reads
/// as broken even when every individual pick is defensible — a reader who liked one dungeon
/// manhwa does not want a shelf of nothing else.
///
/// `similarity` is asked only about pairs that are actually considered, so the caller can compute
/// it from whatever it has (here, the sparse vectors) without materialising a full matrix.
///
/// # Cost
///
/// Each candidate carries a running maximum of its similarity to anything already picked, updated
/// against the *one* newly picked item per round. That makes this `picks × candidates` calls to
/// `similarity` — the textbook incremental form.
///
/// Recomputing the maximum from scratch each round instead, which reads more obviously correct, is
/// `picks² × candidates`: at the shelf's real shape (~36 picks over ~2 000 candidates) that is 2.6
/// million sparse-cosine evaluations on the request path against 72 thousand, and it is the
/// difference between comfortably inside the latency budget and well outside it. The two are
/// arithmetically identical — `max` is associative — and
/// `the_running_maximum_agrees_with_recomputing_it` is what says so.
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
    // Bounded by the input before it reaches an allocation. Nothing can be picked that was not
    // passed in, so a caller's `limit` — which on the request path originates in a query
    // parameter — can never size the result above the slice it selects from.
    let limit = limit.min(ranked.len());

    let mut remaining: Vec<&Scored<Id>> = ranked.iter().collect();
    // Parallel to `remaining`: each entry is that candidate's greatest similarity to anything
    // already picked. Zero while nothing is picked, which is what makes the first round a pure
    // relevance ranking. Sized from the input, not from `limit`.
    let mut closest: Vec<f32> = vec![0.0; remaining.len()];

    // **No `with_capacity`, deliberately.** `limit` reaches here from a query parameter, and
    // sizing an allocation from it is a shape static analysis flags — reasonably, since `.min()`
    // is not something it can see through. The clamp above makes it safe, but the hint is not
    // worth defending: this vector holds at most a shelf's worth of items, so the handful of
    // reallocations it avoids are invisible beside the `picks x candidates` similarity work in
    // the loop below. Do not add it back for tidiness.
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

    /// Agreement between paths is stronger evidence than either alone.
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

    /// The explanation must come from the strongest path, not from retrieval order.
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

    /// **Turning a path's weight down has to demote what that path found.**
    ///
    /// The bug this pins is §8.4's: a weight published in the tuning console that reaches no
    /// arithmetic. The blend rank-normalises each path against its own best candidate, so a
    /// weight that were dropped on the floor would leave the ranking looking entirely sensible
    /// while every knob on this page did nothing.
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

    /// The exact path outranks the seed path at every content weight, because sharing an author
    /// is a fact the dense space cannot represent at all.
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

    /// Identical inputs must produce an identical shelf, every time.
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

    /// **The running maximum must equal recomputing it, and must not cost the same.**
    ///
    /// `diversify` keeps each candidate's greatest similarity to anything already picked and folds
    /// only the newest pick into it. The obvious implementation recomputes that maximum over every
    /// pick each round — arithmetically identical, and quadratically more expensive in the number
    /// of picks. This pins both halves: the same shelf, from far fewer calls.
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
        // The shipped lambda, read out of the registry rather than a local constant: the two
        // forms have to agree at the setting production actually runs.
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
