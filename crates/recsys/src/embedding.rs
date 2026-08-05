//! The dense projection: sparse TF-IDF vectors reduced to a fixed-width space an ANN index can
//! search.
//!
//! # Why a Gram matrix and not a randomized SVD
//!
//! The textbook randomized SVD forms an `n x k` sketch of the data — a million rows by 128
//! columns, half a gigabyte, all resident before the first singular vector exists. Every other
//! stage of this builder is streaming, and that one allocation would set the floor for the whole
//! job.
//!
//! Accumulating `C = AᵀA` instead moves the cost to the *feature* side: `d x d`, where `d` is the
//! projection's input dimension (~2 000), so a few tens of megabytes regardless of catalogue
//! size. `C`'s top eigenvectors are exactly the right singular vectors of `A`, so the resulting
//! basis is the same one, and both passes over the catalogue stream a batch at a time.
//!
//! The trade is numerical: squaring the matrix squares its condition number, so `C` resolves
//! small singular values less precisely than an SVD of `A` would. That is irrelevant here —
//! the output is a *ranking* over fp16 vectors, and the components that suffer are the ones the
//! truncation discards anyway.
//!
//! # Why subspace iteration and not a full eigendecomposition
//!
//! Only the top `k` of `d` eigenvectors are wanted, `k` is an order of magnitude below `d`, and a
//! full symmetric decomposition is `O(d³)` with a dependency to match. Orthogonal iteration is
//! `O(d²k)` per pass, converges geometrically in the eigenvalue gap, and is fifty lines.

/// `SplitMix64`, inline, rather than a dependency on `rand`.
///
/// The starting matrix only has to be arbitrary and *reproducible*: the same catalogue must
/// produce the same basis, or two builds embed into different spaces and their vectors stop
/// being comparable. `rand` makes no stability guarantee across versions for its generators, so
/// a dependency bump could silently invalidate every stored embedding. Nine lines removes that.
fn splitmix64(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// A uniform `f64` in `[-1, 1)` from the generator above.
fn signed_unit(state: &mut u64) -> f64 {
    #[expect(
        clippy::cast_precision_loss,
        reason = "53 bits is exactly f64's mantissa; the shift is what makes this exact"
    )]
    let unit = (splitmix64(state) >> 11) as f64 / (1_u64 << 53) as f64;
    unit.mul_add(2.0, -1.0)
}

/// Accumulates `C = AᵀA` one sparse row at a time.
///
/// Rows are the series' dense-eligible features (authors excluded — see
/// [`crate::FeatureKind::is_dense_eligible`]), already idf-weighted and normalised, indexed by
/// `rec_features.dense_index`.
pub struct GramAccumulator {
    dim: usize,
    /// Row-major `dim x dim`, `f64` because this sums a million outer products and `f32` loses
    /// the tail of that sum to rounding.
    cells: Vec<f64>,
    rows: u64,
}

impl GramAccumulator {
    #[must_use]
    pub fn new(dim: usize) -> Self {
        Self {
            dim,
            cells: vec![0.0; dim * dim],
            rows: 0,
        }
    }

    #[must_use]
    pub const fn dim(&self) -> usize {
        self.dim
    }

    #[must_use]
    pub const fn rows(&self) -> u64 {
        self.rows
    }

    /// Add one row's outer product. Entries whose index is outside the projection's input
    /// dimension are skipped, which is how features past the vocabulary cap stay out of the
    /// dense space while still scoring and explaining.
    pub fn push(&mut self, row: &[(usize, f32)]) {
        self.rows += 1;
        for &(i, wi) in row {
            if i >= self.dim {
                continue;
            }
            let base = i * self.dim;
            for &(j, wj) in row {
                if j < self.dim {
                    self.cells[base + j] += f64::from(wi) * f64::from(wj);
                }
            }
        }
    }

    /// Multiply `C` by a `dim x k` column-major matrix.
    fn multiply(&self, v: &[f64], k: usize, out: &mut [f64]) {
        out.fill(0.0);
        for i in 0..self.dim {
            let row = &self.cells[i * self.dim..(i + 1) * self.dim];
            for column in 0..k {
                let vcol = &v[column * self.dim..(column + 1) * self.dim];
                let mut sum = 0.0;
                for (j, cell) in row.iter().enumerate() {
                    sum += cell * vcol[j];
                }
                out[column * self.dim + i] = sum;
            }
        }
    }

    /// The top-`k` eigenvectors of `C`, as a projection basis.
    ///
    /// `iterations` is the orthogonal-iteration count; convergence is geometric in the ratio
    /// between consecutive eigenvalues, and the default is chosen to be comfortably past the
    /// point where the *ranking* the basis produces stops moving — which is a much weaker
    /// requirement than converging the vectors themselves.
    #[must_use]
    pub fn basis(&self, k: usize, iterations: usize, seed: u64) -> Basis {
        let k = k.min(self.dim).max(1);
        let mut state = seed;
        let mut v: Vec<f64> = (0..self.dim * k).map(|_| signed_unit(&mut state)).collect();
        orthonormalise(&mut v, self.dim, k);

        let mut scratch = vec![0.0; self.dim * k];
        for _ in 0..iterations {
            self.multiply(&v, k, &mut scratch);
            orthonormalise(&mut scratch, self.dim, k);
            std::mem::swap(&mut v, &mut scratch);
        }

        #[expect(
            clippy::cast_possible_truncation,
            reason = "the basis is applied to f32 vectors and stored as fp16; f64 precision is \
                      needed only while accumulating"
        )]
        let columns = v.iter().map(|x| *x as f32).collect();
        Basis {
            dim: self.dim,
            k,
            columns,
        }
    }
}

/// Modified Gram-Schmidt over a `dim x k` column-major matrix.
///
/// *Modified*, not classical: classical Gram-Schmidt loses orthogonality catastrophically once
/// the columns are nearly dependent, which is exactly what happens as iteration converges — the
/// later columns collapse onto the leading eigenvector and the basis silently becomes rank-1.
fn orthonormalise(matrix: &mut [f64], dim: usize, k: usize) {
    for column in 0..k {
        for previous in 0..column {
            let mut dot = 0.0;
            for row in 0..dim {
                dot += matrix[previous * dim + row] * matrix[column * dim + row];
            }
            for row in 0..dim {
                matrix[column * dim + row] -= dot * matrix[previous * dim + row];
            }
        }
        let mut norm = 0.0;
        for row in 0..dim {
            norm += matrix[column * dim + row] * matrix[column * dim + row];
        }
        let norm = norm.sqrt();
        if norm > 1e-12 {
            for row in 0..dim {
                matrix[column * dim + row] /= norm;
            }
        } else {
            // A column that collapsed carries no direction left. Replacing it with a unit axis
            // keeps the basis full-rank and orthogonal; leaving zeros would produce embeddings
            // with a permanently dead component and an ANN index that ranks on fewer dimensions
            // than it reports.
            for row in 0..dim {
                matrix[column * dim + row] = f64::from(u8::from(row == column % dim));
            }
        }
    }
}

/// A fixed projection from the sparse feature space into `k` dimensions.
#[derive(Debug, Clone)]
pub struct Basis {
    dim: usize,
    k: usize,
    /// Column-major `dim x k`.
    columns: Vec<f32>,
}

impl Basis {
    #[must_use]
    pub const fn dim(&self) -> usize {
        self.dim
    }

    #[must_use]
    pub const fn width(&self) -> usize {
        self.k
    }

    /// Reconstruct a basis from stored coefficients, for a build that resumes or an incremental
    /// pass that must project into the *same* space an earlier full build defined.
    ///
    /// # Errors
    /// Returns `None` when `columns` is not exactly `dim * k` long — a basis of the wrong shape
    /// would silently produce embeddings that are not comparable with the stored ones.
    #[must_use]
    pub fn from_columns(dim: usize, k: usize, columns: Vec<f32>) -> Option<Self> {
        (columns.len() == dim * k).then_some(Self { dim, k, columns })
    }

    #[must_use]
    pub fn as_columns(&self) -> &[f32] {
        &self.columns
    }

    /// Project one sparse row, and normalise the result.
    ///
    /// Normalised because the index is searched with cosine distance: unit vectors make the
    /// distance a pure angle, which is what the ranking means.
    #[must_use]
    pub fn project(&self, row: &[(usize, f32)]) -> Vec<f32> {
        let mut out = vec![0.0_f32; self.k];
        for &(i, weight) in row {
            if i >= self.dim {
                continue;
            }
            for (component, value) in out.iter_mut().enumerate() {
                *value += weight * self.columns[component * self.dim + i];
            }
        }
        crate::weighting::normalise(&mut out);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cosine(a: &[f32], b: &[f32]) -> f32 {
        a.iter().zip(b).map(|(x, y)| x * y).sum()
    }

    /// The basis must be orthonormal, or the projection is not a projection.
    ///
    /// The bug this guards: with classical Gram-Schmidt, converging columns collapse onto the
    /// leading eigenvector and the "128-dimensional" space silently becomes one-dimensional —
    /// every series then embeds to nearly the same point and the ANN index returns arbitrary
    /// neighbours while looking perfectly healthy.
    #[test]
    fn the_basis_columns_are_orthonormal() {
        let mut gram = GramAccumulator::new(12);
        for seed in 0..200_u64 {
            let a = (seed % 12) as usize;
            let b = ((seed * 7) % 12) as usize;
            gram.push(&[(a, 0.8), (b, 0.6)]);
        }
        let basis = gram.basis(5, 30, 42);
        let columns = basis.as_columns();
        for i in 0..basis.width() {
            let ci = &columns[i * 12..(i + 1) * 12];
            assert!(
                (cosine(ci, ci) - 1.0).abs() < 1e-3,
                "column {i} is not unit length"
            );
            for j in (i + 1)..basis.width() {
                let cj = &columns[j * 12..(j + 1) * 12];
                assert!(
                    cosine(ci, cj).abs() < 1e-3,
                    "columns {i} and {j} are not orthogonal: {}",
                    cosine(ci, cj)
                );
            }
        }
    }

    /// The whole point: series sharing features must land closer than series sharing none.
    #[test]
    fn co_occurring_features_project_close_together() {
        let mut gram = GramAccumulator::new(8);
        // Two clusters that never co-occur: {0,1,2} and {5,6,7}.
        for _ in 0..100 {
            gram.push(&[(0, 0.7), (1, 0.7), (2, 0.2)]);
            gram.push(&[(5, 0.7), (6, 0.7), (7, 0.2)]);
        }
        let basis = gram.basis(4, 40, 7);

        let left = basis.project(&[(0, 0.7), (1, 0.7)]);
        let also_left = basis.project(&[(1, 0.7), (2, 0.7)]);
        let right = basis.project(&[(5, 0.7), (6, 0.7)]);

        assert!(
            cosine(&left, &also_left) > cosine(&left, &right),
            "same-cluster {} must beat cross-cluster {}",
            cosine(&left, &also_left),
            cosine(&left, &right)
        );
    }

    /// Generalisation is what the SVD buys over a random projection: two series sharing *no*
    /// feature must still land close when their features co-occur elsewhere in the catalogue.
    #[test]
    fn features_that_co_occur_elsewhere_pull_disjoint_series_together() {
        let mut gram = GramAccumulator::new(8);
        for _ in 0..200 {
            // 0 and 1 always appear together, so the basis learns they mean the same thing.
            gram.push(&[(0, 0.7), (1, 0.7)]);
            gram.push(&[(4, 0.7), (5, 0.7)]);
        }
        let basis = gram.basis(4, 40, 11);

        let only_zero = basis.project(&[(0, 1.0)]);
        let only_one = basis.project(&[(1, 1.0)]);
        let unrelated = basis.project(&[(4, 1.0)]);

        assert!(
            cosine(&only_zero, &only_one).abs() > cosine(&only_zero, &unrelated).abs(),
            "features that co-occur must project together even with no shared feature"
        );
    }

    #[test]
    fn projection_is_unit_length_and_finite() {
        let mut gram = GramAccumulator::new(6);
        gram.push(&[(0, 1.0), (3, 0.5)]);
        gram.push(&[(1, 1.0)]);
        let basis = gram.basis(3, 20, 1);
        let v = basis.project(&[(0, 0.9), (3, 0.4)]);
        assert!(v.iter().all(|x| x.is_finite()));
        assert!((v.iter().map(|x| x * x).sum::<f32>() - 1.0).abs() < 1e-4);
    }

    /// An empty row cannot be normalised; it must stay zero rather than becoming NaN, because a
    /// NaN vector poisons every distance the index computes against it.
    #[test]
    fn an_empty_row_projects_to_zero_not_nan() {
        let mut gram = GramAccumulator::new(4);
        gram.push(&[(0, 1.0)]);
        let basis = gram.basis(2, 10, 3);
        let v = basis.project(&[]);
        assert!(v.iter().all(|x| x.is_finite() && *x == 0.0));
    }

    /// Indices past the projection's input dimension are ignored, not panicked on: that is how
    /// features beyond the vocabulary cap keep scoring without shaping the embedding.
    #[test]
    fn indices_outside_the_input_dimension_are_skipped() {
        let mut gram = GramAccumulator::new(4);
        gram.push(&[(0, 1.0), (99, 1.0)]);
        assert_eq!(gram.rows(), 1);
        let basis = gram.basis(2, 10, 5);
        let v = basis.project(&[(0, 1.0), (12345, 1.0)]);
        assert!(v.iter().all(|x| x.is_finite()));
    }

    /// A basis must round-trip through storage, or an incremental build projects into a
    /// different space than the full build did and the two sets of embeddings are incomparable.
    #[test]
    fn a_basis_round_trips_through_its_coefficients() {
        let mut gram = GramAccumulator::new(6);
        gram.push(&[(0, 1.0), (2, 0.4)]);
        gram.push(&[(1, 1.0), (5, 0.3)]);
        let basis = gram.basis(3, 15, 9);
        let restored =
            Basis::from_columns(basis.dim(), basis.width(), basis.as_columns().to_vec()).unwrap();
        assert_eq!(
            basis.project(&[(0, 1.0)]),
            restored.project(&[(0, 1.0)]),
            "a restored basis must project identically"
        );
        assert!(
            Basis::from_columns(6, 3, vec![0.0; 5]).is_none(),
            "a wrongly shaped basis must be refused, not silently used"
        );
    }
}
