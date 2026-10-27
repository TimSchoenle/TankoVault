//! Turning term weights into discriminating ones, and the vectors into comparable ones.

/// Returns the inverse document frequency of a feature seen in `doc_count` of `total_docs` series.
///
/// The smoothed form, `ln(N / (1 + df)) + 1`, keeps a feature on every series above zero. The
/// unsmoothed `ln(N/df)` is zero there, which would give a series of only common features a zero
/// vector. An empty catalogue weights every feature `1.0`.
///
/// Clamped at zero, because `df > N` is possible during an incremental build, when the counts and
/// the vectors are from different generations.
///
/// ```
/// use tankovault_recsys::idf;
///
/// assert!(idf(2, 1_000) > idf(300, 1_000));
/// assert!(idf(1_000, 1_000) > 0.0);
/// ```
#[must_use]
pub fn idf(doc_count: i64, total_docs: i64) -> f32 {
    if total_docs <= 0 {
        return 1.0;
    }
    #[expect(
        clippy::cast_precision_loss,
        reason = "catalogue counts are far below f64's exact-integer range"
    )]
    let ratio = total_docs as f64 / (1.0 + doc_count.max(0) as f64);
    #[expect(
        clippy::cast_possible_truncation,
        reason = "idf is stored as f32 throughout"
    )]
    let value = (ratio.ln() + 1.0) as f32;
    value.max(0.0)
}

/// Scales each weight by its feature's idf and L2-normalises the result in place.
///
/// Normalised, cosine compares shape and not size: without it a series with forty tags would
/// outscore one with eight by mass alone.
///
/// A vector that is all zero after weighting stays zero. Callers treat it as not recommendable,
/// which is the `min_features` gate.
pub fn apply_idf(weights: &mut [f32], idfs: &[f32]) {
    debug_assert_eq!(weights.len(), idfs.len());
    for (weight, idf) in weights.iter_mut().zip(idfs) {
        *weight *= idf;
    }
    normalise(weights);
}

/// Scales a vector to unit length. A zero vector is left alone.
pub fn normalise(values: &mut [f32]) {
    let norm = values.iter().map(|v| v * v).sum::<f32>().sqrt();
    if norm > f32::EPSILON {
        for value in values.iter_mut() {
            *value /= norm;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A feature on every series keeps a positive weight, so a series described only by universal
    /// features still has a cosine against others.
    #[test]
    fn a_universal_feature_still_contributes_something() {
        assert!(idf(1000, 1000) > 0.0);
        assert!(idf(1000, 1000) < idf(1, 1000));
    }

    #[test]
    fn rarer_features_score_higher() {
        let rare = idf(2, 1_000_000);
        let common = idf(300_000, 1_000_000);
        assert!(rare > common, "{rare} should exceed {common}");
    }

    #[test]
    fn degenerate_counts_do_not_produce_nonsense() {
        assert!(
            (idf(5, 0) - 1.0).abs() < f32::EPSILON,
            "an empty catalogue has no information to weight by"
        );
        assert!(
            idf(-1, 100) > 0.0,
            "a negative count is clamped, not propagated"
        );
        // Mid-build, counts and vectors can be from different generations.
        assert!(idf(200, 100) >= 0.0, "df > N must not go negative");
    }

    #[test]
    fn normalising_gives_unit_length() {
        let mut v = [3.0, 4.0];
        normalise(&mut v);
        assert!((v.iter().map(|x| x * x).sum::<f32>() - 1.0).abs() < 1e-6);
    }

    /// A zero vector stays zero under normalisation and never becomes NaN.
    #[test]
    fn a_zero_vector_is_left_alone() {
        let mut v = [0.0_f32, 0.0, 0.0];
        normalise(&mut v);
        assert!(v.iter().all(|x| x.is_finite() && x.abs() < f32::EPSILON));
    }

    /// After normalising, a well-documented series does not outscore a well-matched one.
    #[test]
    fn a_longer_vector_does_not_outscore_a_shorter_one_after_normalising() {
        let mut many = vec![1.0_f32; 40];
        let mut few = vec![1.0_f32; 8];
        normalise(&mut many);
        normalise(&mut few);
        let norm = |v: &[f32]| v.iter().map(|x| x * x).sum::<f32>();
        assert!((norm(&many) - norm(&few)).abs() < 1e-5);
    }
}
