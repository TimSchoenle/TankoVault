//! Exact scoring over the sparse vectors, and the explanation that comes with it.
//!
//! The dense index says *that* two series are close; only these functions can say *why*. Keeping
//! the sparse vectors is what lets the reader be told something true even though retrieval is
//! approximate.

/// Cosine similarity of two sparse vectors, both sorted ascending by feature id.
///
/// A merge, not a hash join: the vectors are ~20 entries each and already ordered by
/// construction, so this is two cursors and no allocation.
///
/// Assumes both inputs are L2-normalised (they are, as stored), so the dot product *is* the
/// cosine. Passing unnormalised vectors yields a dot product, silently — which is why the one
/// place that could is a debug assertion.
#[must_use]
pub fn cosine(a: &[(i32, f32)], b: &[(i32, f32)]) -> f32 {
    debug_assert!(
        a.windows(2).all(|w| w[0].0 < w[1].0),
        "left vector must be sorted"
    );
    debug_assert!(
        b.windows(2).all(|w| w[0].0 < w[1].0),
        "right vector must be sorted"
    );

    let (mut i, mut j, mut sum) = (0, 0, 0.0_f32);
    while i < a.len() && j < b.len() {
        match a[i].0.cmp(&b[j].0) {
            std::cmp::Ordering::Less => i += 1,
            std::cmp::Ordering::Greater => j += 1,
            std::cmp::Ordering::Equal => {
                sum += a[i].1 * b[j].1;
                i += 1;
                j += 1;
            }
        }
    }
    sum
}

/// The features two series share, strongest contribution first.
///
/// "Contribution" is the product of the two weights — the term's actual share of the cosine —
/// not either weight alone. A feature that is prominent on one side and incidental on the other
/// explains far less than the number on the prominent side suggests.
#[must_use]
pub fn shared_features(a: &[(i32, f32)], b: &[(i32, f32)], limit: usize) -> Vec<i32> {
    let (mut i, mut j) = (0, 0);
    let mut shared: Vec<(i32, f32)> = Vec::new();
    while i < a.len() && j < b.len() {
        match a[i].0.cmp(&b[j].0) {
            std::cmp::Ordering::Less => i += 1,
            std::cmp::Ordering::Greater => j += 1,
            std::cmp::Ordering::Equal => {
                shared.push((a[i].0, a[i].1 * b[j].1));
                i += 1;
                j += 1;
            }
        }
    }
    // Descending by contribution, then by id so the output is deterministic when two features
    // contribute identically — an explanation that reorders between identical requests reads as
    // a bug to anyone watching.
    shared.sort_by(|x, y| y.1.total_cmp(&x.1).then_with(|| x.0.cmp(&y.0)));
    shared.truncate(limit);
    shared.into_iter().map(|(id, _)| id).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_vectors_score_one() {
        let v = [(1, 0.6), (5, 0.8)];
        assert!((cosine(&v, &v) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn disjoint_vectors_score_zero() {
        // Exactly zero, not approximately: no term is ever added, so this is the identity of the
        // accumulator rather than the result of any arithmetic.
        for (a, b) in [
            (&[(1, 1.0_f32)][..], &[(2, 1.0_f32)][..]),
            (&[][..], &[(2, 1.0)][..]),
            (&[][..], &[][..]),
        ] {
            assert!(cosine(a, b).abs() < f32::EPSILON);
        }
    }

    #[test]
    fn partial_overlap_scores_between() {
        let a = [(1, 0.6), (2, 0.8)];
        let b = [(2, 1.0)];
        let score = cosine(&a, &b);
        assert!(score > 0.0 && score < 1.0, "got {score}");
        assert!((score - 0.8).abs() < 1e-6);
    }

    /// The merge must not depend on which vector is longer, or scoring becomes asymmetric and a
    /// pair's similarity changes with the order it is asked about.
    #[test]
    fn cosine_is_symmetric_regardless_of_length() {
        let a = [(1, 0.5), (3, 0.5), (7, 0.7)];
        let b = [(3, 1.0)];
        assert!((cosine(&a, &b) - cosine(&b, &a)).abs() < 1e-7);
    }

    #[test]
    fn shared_features_rank_by_contribution_not_by_one_side() {
        // Feature 1 is prominent on the left and incidental on the right; feature 2 is moderate
        // on both. The *product* is what explains the match.
        let a = [(1, 0.9), (2, 0.5)];
        let b = [(1, 0.05), (2, 0.5)];
        assert_eq!(shared_features(&a, &b, 5), vec![2, 1]);
    }

    #[test]
    fn shared_features_respects_the_limit_and_is_deterministic() {
        let a = [(1, 0.5), (2, 0.5), (3, 0.5)];
        let b = [(1, 0.5), (2, 0.5), (3, 0.5)];
        assert_eq!(shared_features(&a, &b, 2), vec![1, 2]);
        assert_eq!(shared_features(&a, &b, 0), Vec::<i32>::new());
        // Equal contributions must break ties by id, every time.
        for _ in 0..10 {
            assert_eq!(shared_features(&a, &b, 3), vec![1, 2, 3]);
        }
    }

    #[test]
    fn nothing_shared_explains_nothing() {
        assert!(shared_features(&[(1, 1.0)], &[(9, 1.0)], 3).is_empty());
    }
}
