//! The dense projection: sparse TF-IDF vectors reduced to a fixed-width space an ANN index can
//! search.
//!
//! # Why a Gram matrix and not a randomized SVD
//!
//! A randomized SVD forms an `n x k` sketch of the data: a million rows by 128 columns, half a
//! gigabyte, resident before the first singular vector exists. Every other stage of the builder
//! streams, and that allocation would set the memory floor for the whole job.
//!
//! Accumulating `C = AᵀA` puts the cost on the feature side: `d x d`, where `d` is the
//! projection's input dimension (about 2,000), so a few tens of megabytes at any catalogue size.
//! `C`'s top eigenvectors are the right singular vectors of `A`, so the basis is the same, and
//! both passes over the catalogue stream a batch at a time.
//!
//! Squaring the matrix squares its condition number, so `C` resolves small singular values less
//! precisely than an SVD of `A`. The output is a ranking over fp16 vectors, and the components
//! that lose precision are the ones the truncation discards.
//!
//! # Why subspace iteration and not a full eigendecomposition
//!
//! Only the top `k` of `d` eigenvectors are needed, with `k` an order of magnitude below `d`. A
//! full symmetric decomposition is `O(d³)` and needs a dependency. Orthogonal iteration is
//! `O(d²k)` per pass, converges geometrically in the eigenvalue gap, and is fifty lines.

/// Advances a `SplitMix64` generator and returns its next value.
///
/// The starting matrix has to be reproducible: the same catalogue must produce the same basis,
/// or two builds embed into different spaces. `rand` does not promise its generators stay stable
/// across versions, so a dependency bump could invalidate every stored embedding.
fn splitmix64(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// Returns a uniform `f64` in `[-1, 1)` from the generator above.
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
/// A row is one series' dense-eligible features ([`crate::FeatureKind::is_dense_eligible`]),
/// idf-weighted and normalised, indexed by `rec_features.dense_index`.
pub struct GramAccumulator {
    dim: usize,
    /// Row-major `dim x dim`. `f64`, because the sum runs over a million outer products and `f32`
    /// would lose its tail to rounding.
    cells: Vec<f64>,
    rows: u64,
}

impl GramAccumulator {
    /// Returns an empty accumulator over a `dim`-wide input space.
    ///
    /// Allocates `dim * dim` `f64` at once, the one structure in the build that grows with the
    /// vocabulary instead of the catalogue.
    #[must_use]
    pub fn new(dim: usize) -> Self {
        Self {
            dim,
            cells: vec![0.0; dim * dim],
            rows: 0,
        }
    }

    /// Returns the input space's width, as passed to [`Self::new`].
    #[must_use]
    pub const fn dim(&self) -> usize {
        self.dim
    }

    /// Returns the rows pushed so far, including rows whose every entry fell outside `dim`.
    #[must_use]
    pub const fn rows(&self) -> u64 {
        self.rows
    }

    /// Adds one row's outer product, skipping entries whose index is at or past `dim`.
    ///
    /// Skipping is how features past the vocabulary cap stay out of the dense space while they
    /// still score and explain.
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

    /// Writes `C` times the `dim x k` column-major matrix `v` into `out`.
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

    /// Returns the top-`k` eigenvectors of `C`, as a projection basis.
    ///
    /// `k` is clamped to `1..=dim`. `iterations` is the orthogonal-iteration count
    /// ([`crate::BASIS_ITERATIONS`]), and `seed` makes the starting matrix, and so the basis,
    /// reproducible.
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

/// Orthonormalises a `dim x k` column-major matrix in place by modified Gram-Schmidt.
///
/// Classical Gram-Schmidt loses orthogonality once the columns are nearly dependent, which is
/// what happens as iteration converges: the later columns collapse onto the leading eigenvector
/// and the basis becomes rank-1.
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
            // A collapsed column has no direction left. A unit axis keeps the basis full-rank;
            // zeros would leave every embedding with a dead component.
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
    /// Returns the input space this basis projects from.
    #[must_use]
    pub const fn dim(&self) -> usize {
        self.dim
    }

    /// Returns the number of directions, which is the length of a projected vector.
    #[must_use]
    pub const fn width(&self) -> usize {
        self.k
    }

    /// Rebuilds a basis from stored coefficients, so a resumed build or an incremental pass
    /// projects into the space an earlier full build defined.
    ///
    /// Returns `None` when `columns` is not exactly `dim * k` long.
    #[must_use]
    pub fn from_columns(dim: usize, k: usize, columns: Vec<f32>) -> Option<Self> {
        (columns.len() == dim * k).then_some(Self { dim, k, columns })
    }

    /// Returns the columns, column-major and `dim * k` long, as [`Self::from_columns`] takes them.
    #[must_use]
    pub fn as_columns(&self) -> &[f32] {
        &self.columns
    }

    /// Projects one sparse row and returns it at unit length, or zero for an empty row.
    ///
    /// The index is searched by cosine distance, and unit vectors make that distance a pure angle.
    /// Entries at or past `dim` are skipped.
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

    /// The basis columns are orthonormal, so the space does not collapse to one dimension.
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

    /// Series sharing features land closer than series sharing none.
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

    /// Two series sharing no feature still land close when their features co-occur elsewhere in
    /// the catalogue, which a random projection would not give.
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

    /// An empty row projects to zero, never NaN, which would poison every distance against it.
    #[test]
    fn an_empty_row_projects_to_zero_not_nan() {
        let mut gram = GramAccumulator::new(4);
        gram.push(&[(0, 1.0)]);
        let basis = gram.basis(2, 10, 3);
        let v = basis.project(&[]);
        assert!(v.iter().all(|x| x.is_finite() && *x == 0.0));
    }

    /// Indices past the input dimension are skipped without a panic.
    #[test]
    fn indices_outside_the_input_dimension_are_skipped() {
        let mut gram = GramAccumulator::new(4);
        gram.push(&[(0, 1.0), (99, 1.0)]);
        assert_eq!(gram.rows(), 1);
        let basis = gram.basis(2, 10, 5);
        let v = basis.project(&[(0, 1.0), (12345, 1.0)]);
        assert!(v.iter().all(|x| x.is_finite()));
    }

    /// A basis round-trips through its stored coefficients, and a wrongly shaped one is refused.
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
