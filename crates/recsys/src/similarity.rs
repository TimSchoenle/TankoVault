//! Exact scoring over the sparse vectors, and the explanation that comes with it.
//!
//! The dense index says that two series are close; these functions say why. Retrieval is
//! approximate, and the sparse vectors keep the explanation exact.

/// Returns the cosine similarity of two L2-normalised sparse vectors, both sorted ascending by
/// feature id.
///
/// The result is the dot product, which equals the cosine for normalised input; unnormalised
/// input returns the plain dot product. It walks both vectors once and allocates nothing. Debug
/// builds assert the sort order.
///
/// ```
/// use tankovault_recsys::cosine;
///
/// let a = [(1, 0.6), (2, 0.8)];
/// assert!((cosine(&a, &a) - 1.0).abs() < 1e-6);
/// assert!((cosine(&a, &[(2, 1.0)]) - 0.8).abs() < 1e-6);
/// ```
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

/// Returns up to `limit` feature ids two series share, strongest contribution first.
///
/// A feature's contribution is the product of its two weights, which is its share of the cosine.
/// Ties are ordered by id.
///
/// ```
/// use tankovault_recsys::shared_features;
///
/// // Feature 1 is prominent on one side only, so feature 2 explains more.
/// let a = [(1, 0.9), (2, 0.5)];
/// let b = [(1, 0.05), (2, 0.5)];
/// assert_eq!(shared_features(&a, &b, 5), [2, 1]);
/// ```
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
    // Ties break by id, so identical requests explain in the same order.
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
        // Exactly zero: no term is added, so the accumulator keeps its initial value.
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

    /// A pair scores the same in either order, whichever vector is longer.
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
        assert_eq!(
            shared_features(&[(1, 1.0)], &[(9, 1.0)], 3),
            Vec::<i32>::new()
        );
    }
}
